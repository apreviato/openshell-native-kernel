//! The release manifest's signature: Ed25519 over the manifest's exact bytes,
//! base64 in `<manifest>.sig`, by one of the trusted keys, as the OpenShell
//! Desktop's updater signs and checks its releases (hostd/services/updates.ts
//! in the `openshell` repository). Keys are PEM: SubjectPublicKeyInfo for the
//! trusted ones, PKCS#8 for the release tooling's private one.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// DER before the 32 key bytes of an Ed25519 SubjectPublicKeyInfo.
const SPKI_PREFIX: [u8; 12] = [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
/// DER before the 32 seed bytes of an Ed25519 PKCS#8 private key.
const PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64, padded; whitespace is ignored.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let symbols: Vec<u8> = text.bytes().filter(|byte| !byte.is_ascii_whitespace()).collect();
    if symbols.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(symbols.len() / 4 * 3);
    for (index, chunk) in symbols.chunks(4).enumerate() {
        let last = index == symbols.len() / 4 - 1;
        let mut value = 0u32;
        let mut padding = 0;
        for (position, &symbol) in chunk.iter().enumerate() {
            let digit = if symbol == b'=' && last && position >= 2 {
                padding += 1;
                0
            } else if padding > 0 {
                return None;
            } else {
                ALPHABET.iter().position(|&letter| letter == symbol)? as u32
            };
            value = value << 6 | digit;
        }
        let bytes = value.to_be_bytes();
        out.extend_from_slice(&bytes[1..4 - padding]);
    }
    Some(out)
}

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = u32::from(chunk[0]) << 16 | u32::from(*chunk.get(1).unwrap_or(&0)) << 8 | u32::from(*chunk.get(2).unwrap_or(&0));
        for position in 0..4 {
            if position <= chunk.len() {
                out.push(ALPHABET[(value >> (18 - 6 * position) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The DER of a PEM block with this label.
fn pem_body(text: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = text.find(&begin)? + begin.len();
    let stop = text[start..].find(&end)? + start;
    base64_decode(&text[start..stop])
}

/// A trusted key: an Ed25519 public key in PEM.
pub fn public_key(pem: &str) -> Option<VerifyingKey> {
    let der = pem_body(pem, "PUBLIC KEY")?;
    let key: [u8; 32] = der.strip_prefix(&SPKI_PREFIX)?.try_into().ok()?;
    VerifyingKey::from_bytes(&key).ok()
}

/// The release tooling's key: an Ed25519 private key in PKCS#8 PEM.
pub fn private_key(pem: &str) -> Option<SigningKey> {
    let der = pem_body(pem, "PRIVATE KEY")?;
    let seed: [u8; 32] = der.strip_prefix(&PKCS8_PREFIX)?.try_into().ok()?;
    Some(SigningKey::from_bytes(&seed))
}

pub fn public_pem(key: &VerifyingKey) -> String {
    let mut der = SPKI_PREFIX.to_vec();
    der.extend_from_slice(key.as_bytes());
    format!("-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n", base64_encode(&der))
}

pub fn private_pem(key: &SigningKey) -> String {
    let mut der = PKCS8_PREFIX.to_vec();
    der.extend_from_slice(&key.to_bytes());
    format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", base64_encode(&der))
}

/// Whether `signature` (base64) is a trusted key's signature of `text`.
pub fn verify(text: &[u8], signature: &str, keys: &[VerifyingKey]) -> bool {
    let Some(raw) = base64_decode(signature) else { return false };
    let Ok(raw) = <[u8; 64]>::try_from(raw.as_slice()) else { return false };
    let signature = Signature::from_bytes(&raw);
    keys.iter().any(|key| key.verify_strict(text, &signature).is_ok())
}

/// The base64 signature of `text`, for `<manifest>.sig`.
pub fn sign(text: &[u8], key: &SigningKey) -> String {
    base64_encode(&key.sign(text).to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap()).collect()
    }

    // RFC 8032, section 7.1, test 1.
    const SECRET: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const PUBLIC: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const SIGNATURE: &str = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";

    #[test]
    fn base64_round_trips() {
        for length in 0..40 {
            let bytes: Vec<u8> = (0..length).map(|value| (value * 37 % 251) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&bytes)).unwrap(), bytes);
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert!(base64_decode("Zm9v=mFy").is_none());
        assert!(base64_decode("Zm9").is_none());
    }

    #[test]
    fn verifies_the_rfc_8032_vector_and_nothing_else() {
        let signing = SigningKey::from_bytes(&hex(SECRET).try_into().unwrap());
        assert_eq!(signing.verifying_key().as_bytes().as_slice(), hex(PUBLIC).as_slice());
        let signature = base64_encode(&hex(SIGNATURE));
        assert_eq!(sign(b"", &signing), signature);
        let trusted = [signing.verifying_key()];
        assert!(verify(b"", &signature, &trusted));
        assert!(!verify(b"x", &signature, &trusted));
        assert!(!verify(b"", &signature, &[SigningKey::from_bytes(&[7; 32]).verifying_key()]));
        assert!(!verify(b"", "not base64", &trusted));
    }

    #[test]
    fn keys_round_trip_through_pem() {
        let signing = SigningKey::from_bytes(&hex(SECRET).try_into().unwrap());
        let private = private_key(&private_pem(&signing)).unwrap();
        assert_eq!(private.to_bytes(), signing.to_bytes());
        let public = public_key(&public_pem(&signing.verifying_key())).unwrap();
        assert_eq!(public, signing.verifying_key());
        assert!(public_key(&private_pem(&signing)).is_none());
    }
}
