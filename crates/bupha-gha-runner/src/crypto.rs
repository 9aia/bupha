//! The cryptography the protocol needs: an RSA keypair per runner
//! (public half registered, private half signs the token assertion
//! and unwraps session keys), JWT bearer assertions, RSA-OAEP session
//! key unwrap, and AES-256-CBC message decryption.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts as _;
use rsa::{Oaep, RsaPrivateKey};
use sha2::{Digest, Sha256};

use crate::types::PublicKey;
use crate::Credentials;

/// Errors here are protocol failures, not I/O.
#[derive(Debug)]
pub struct CryptoError(pub String);

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "crypto: {}", self.0)
    }
}

impl std::error::Error for CryptoError {}

fn err(e: impl std::fmt::Display) -> CryptoError {
    CryptoError(e.to_string())
}

/// A fresh 2048-bit runner key.
pub fn generate_key() -> Result<RsaPrivateKey, CryptoError> {
    RsaPrivateKey::new(&mut rand::thread_rng(), 2048).map_err(err)
}

/// The public half, base64 big-endian — the shape `AgentRequest`
/// sends.
pub fn public_key(key: &RsaPrivateKey) -> PublicKey {
    let encode = |n: &rsa::BigUint| STANDARD.encode(n.to_bytes_be());
    PublicKey {
        exponent: encode(key.e()),
        modulus: encode(key.n()),
    }
}

/// PKCS#1 PEM — what the credentials file stores.
pub fn private_key_pem(key: &RsaPrivateKey) -> Result<String, CryptoError> {
    key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .map(|p| p.to_string())
        .map_err(err)
}

/// Either spelling: registration writes PKCS#1; a key from elsewhere
/// may be PKCS#8.
pub fn read_key(pem: &str) -> Result<RsaPrivateKey, CryptoError> {
    RsaPrivateKey::from_pkcs1_pem(pem)
        .or_else(|_| RsaPrivateKey::from_pkcs8_pem(pem))
        .map_err(err)
}

fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn seconds_since_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The RS256 JWT the token endpoint trades for an access token.
///
/// Claims are `iss`/`sub` = client id, `aud` = token url, unique
/// `jti`, and a five-minute life counted from `nbf` — the service
/// rejects longer. `nbf` is backdated 60s so a slightly fast clock
/// still validates.
pub fn sign_assertion(credentials: &Credentials) -> Result<String, CryptoError> {
    let key = read_key(&credentials.private_key)?;
    let now = seconds_since_epoch();
    let claims = serde_json::json!({
        "iss": credentials.client_id,
        "sub": credentials.client_id,
        "aud": credentials.token_url,
        "jti": uuid::Uuid::new_v4().to_string(),
        "nbf": now.saturating_sub(60),
        "exp": now.saturating_sub(60) + 300,
    });
    let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
    let payload = b64url(serde_json::to_string(&claims).map_err(err)?.as_bytes());
    let signing_input = format!("{header}.{payload}");
    let digest = Sha256::digest(signing_input.as_bytes());
    let signature = key
        .sign(rsa::Pkcs1v15Sign::new::<Sha256>(), &digest)
        .map_err(err)?;
    Ok(format!("{signing_input}.{}", b64url(&signature)))
}

/// Unwrap the session's AES key — RSA-OAEP, SHA-256 under FIPS,
/// SHA-1 otherwise (what the service defaults to).
pub fn unwrap_session_key(
    key: &RsaPrivateKey,
    wrapped: &[u8],
    fips: bool,
) -> Result<Vec<u8>, CryptoError> {
    let padding = if fips {
        Oaep::new::<Sha256>()
    } else {
        Oaep::new::<sha1::Sha1>()
    };
    key.decrypt(padding, wrapped)
        .map_err(|e| CryptoError(format!("cannot unwrap the session key: {e}")))
}

/// AES-256-CBC with PKCS#7 — what the service's `Aes.Create()`
/// defaults to on the other side.
pub fn decrypt_message(key: &[u8], iv: &[u8], body: &[u8]) -> Result<String, CryptoError> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit};
    type Decryptor = cbc::Decryptor<aes::Aes256>;
    let decryptor = Decryptor::new_from_slices(key, iv)
        .map_err(|e| CryptoError(format!("bad key or iv: {e}")))?;
    let mut buf = body.to_vec();
    let plain = decryptor
        .decrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf)
        .map_err(|e| CryptoError(format!("cannot decrypt the message: {e}")))?;
    String::from_utf8(plain.to_vec()).map_err(|e| CryptoError(format!("message is not utf8: {e}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use aes::cipher::{BlockEncryptMut, KeyIvInit};

    #[test]
    fn key_pem_round_trips() {
        let key = generate_key().unwrap();
        let pem = private_key_pem(&key).unwrap();
        let back = read_key(&pem).unwrap();
        assert_eq!(back.n(), key.n());
    }

    #[test]
    fn assertion_is_a_verifiable_rs256_jwt() {
        let key = generate_key().unwrap();
        let pem = private_key_pem(&key).unwrap();
        let creds = Credentials {
            service_url: "https://svc.example".into(),
            pool_id: 1,
            agent_id: 2,
            agent_name: "bupha".into(),
            client_id: "cid".into(),
            token_url: "https://auth.example/token".into(),
            private_key: pem,
        };
        let jwt = sign_assertion(&creds).unwrap();
        let mut parts = jwt.split('.');
        let (h, p, s) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        assert_eq!(parts.next(), None);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h).unwrap()).unwrap();
        assert_eq!(header["alg"], "RS256");
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).unwrap()).unwrap();
        assert_eq!(claims["iss"], "cid");
        assert_eq!(claims["aud"], "https://auth.example/token");
        assert!(claims["exp"].as_u64().unwrap() - claims["nbf"].as_u64().unwrap() <= 300);
        // Verify the signature with the public key.
        let sig = URL_SAFE_NO_PAD.decode(s).unwrap();
        let n = key.n().to_bytes_be();
        let e = key.e().to_bytes_be();
        let pubkey = rsa::RsaPublicKey::new(
            rsa::BigUint::from_bytes_be(&n),
            rsa::BigUint::from_bytes_be(&e),
        )
        .unwrap();
        pubkey
            .verify(
                rsa::Pkcs1v15Sign::new::<Sha256>(),
                Sha256::digest(format!("{h}.{p}").as_bytes()).as_slice(),
                &sig,
            )
            .unwrap();
    }

    #[test]
    fn session_key_unwraps() {
        let key = generate_key().unwrap();
        let session = [9u8; 32];
        let wrapped = key
            .to_public_key()
            .encrypt(&mut rand::thread_rng(), Oaep::new::<sha1::Sha1>(), &session)
            .unwrap();
        assert_eq!(unwrap_session_key(&key, &wrapped, false).unwrap(), session);
    }

    #[test]
    fn message_round_trips() {
        use aes::cipher::block_padding::Pkcs7;
        type Encryptor = cbc::Encryptor<aes::Aes256>;
        let key = [7u8; 32];
        let iv = [3u8; 16];
        let msg = b"{\"messageType\":\"X\"}";
        let mut buf = msg.to_vec();
        buf.resize(32, 0); // room for the padding block
        let body = Encryptor::new_from_slices(&key, &iv)
            .unwrap()
            .encrypt_padded_mut::<Pkcs7>(&mut buf, msg.len())
            .unwrap()
            .to_vec();
        let plain = decrypt_message(&key, &iv, &body).unwrap();
        assert!(plain.contains("X"));
    }
}
