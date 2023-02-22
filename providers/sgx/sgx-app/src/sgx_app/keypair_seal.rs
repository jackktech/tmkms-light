use aes_gcm_siv::{
    aead::{generic_array::GenericArray, Aead, Payload},
    Aes128GcmSiv, KeyInit,
};
use ed25519_consensus::{SigningKey, VerificationKey};
use rand::{rngs::OsRng, RngCore};
use sgx_isa::{ErrorCode, Keyname, Keypolicy, Keyrequest, Report};
use std::convert::TryInto;
use subtle::ConstantTimeEq;
use tmkms_light_sgx_runner::SealedKeyData;
use zeroize::Zeroize;

fn seal_payload(
    csprng: &mut OsRng,
    payload: Payload,
    keyid: [u8; 32],
) -> Result<SealedKeyData, ErrorCode> {
    let mut nonce = [0u8; 12];
    csprng.fill_bytes(&mut nonce);
    let report = Report::for_self();
    let key_request = Keyrequest {
        keyname: Keyname::Seal as _,
        keypolicy: Keypolicy::MRSIGNER,
        isvsvn: report.isvsvn,
        cpusvn: report.cpusvn,
        keyid,
        ..Default::default()
    };
    let nonce_ga = GenericArray::from_slice(&nonce);
    let mut key = key_request.egetkey()?;
    let gk = GenericArray::from_slice(&key);
    let aead = Aes128GcmSiv::new(gk);
    if let Ok(sealed_secret) = aead.encrypt(nonce_ga, payload) {
        key.zeroize();
        Ok(SealedKeyData {
            seal_key_request: key_request.into(),
            nonce,
            sealed_secret,
        })
    } else {
        key.zeroize();
        Err(ErrorCode::MacCompareFail)
    }
}

/// Seals the provided ed25519 keypair with `Aes128GcmSiv`
/// via a key request against MRSIGNER (so that versions with higher `isvsvn`
/// can unseal the keypair)
pub fn seal(csprng: &mut OsRng, keypair: &SigningKey) -> Result<SealedKeyData, ErrorCode> {
    let pubkey = keypair.verification_key();
    let payload = Payload {
        msg: keypair.as_bytes(),
        aad: pubkey.as_bytes(),
    };
    seal_payload(csprng, payload, pubkey.to_bytes())
}

pub fn seal_secret(
    csprng: &mut OsRng,
    secret: &[u8],
    keyid: [u8; 32],
) -> Result<SealedKeyData, ErrorCode> {
    let payload = Payload {
        msg: secret,
        aad: &keyid,
    };
    seal_payload(csprng, payload, keyid)
}

pub fn unseal_secret(sealed_data: &SealedKeyData) -> Result<Vec<u8>, ErrorCode> {
    if sealed_data.seal_key_request.keyname != (Keyname::Seal as u16) {
        return Err(ErrorCode::InvalidKeyname);
    }
    let key_request: Keyrequest = sealed_data
        .seal_key_request
        .try_into()
        .map_err(|_| ErrorCode::InvalidAttribute)?;
    let payload = Payload {
        msg: &sealed_data.sealed_secret,
        aad: &sealed_data.seal_key_request.keyid,
    };
    let nonce_ga = GenericArray::from_slice(&sealed_data.nonce);
    let mut key = key_request.egetkey()?;
    let gk = GenericArray::from_slice(&key);
    let aead = Aes128GcmSiv::new(gk);
    if let Ok(secret_key) = aead.decrypt(nonce_ga, payload) {
        key.zeroize();
        Ok(secret_key)
    } else {
        key.zeroize();
        Err(ErrorCode::MacCompareFail)
    }
}

/// Checks the provided keyrequests
/// and attempts to unseal the ed25519 keypair with `Aes128GcmSiv`
pub fn unseal(sealed_data: &SealedKeyData) -> Result<SigningKey, ErrorCode> {
    if let Ok(public) = VerificationKey::try_from(&sealed_data.seal_key_request.keyid[..]) {
        let mut secret_key = unseal_secret(sealed_data)?;
        let mut secret =
            SigningKey::try_from(secret_key.as_ref()).map_err(|_| ErrorCode::InvalidSignature)?;
        secret_key.zeroize();
        if secret
            .verification_key()
            .as_bytes()
            .ct_eq(public.as_bytes())
            .unwrap_u8()
            == 0
        {
            secret.zeroize();
            return Err(ErrorCode::InvalidSignature);
        }
        Ok(secret)
    } else {
        Err(ErrorCode::InvalidSignature)
    }
}
