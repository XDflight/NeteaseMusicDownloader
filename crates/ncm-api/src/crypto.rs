//! `eapi` — the request encryption used by the NetEase Cloud Music desktop client.
//!
//! Wire format of a request body (`application/x-www-form-urlencoded`): `params=<HEX>` where
//! `<HEX>` is the upper-case hex of `AES-128-ECB/PKCS7(key = "e82ckenh8dichen8", plain)` and
//! `plain = "{path}-36cd479b6b5-{json}-36cd479b6b5-{md5}"` with
//! `md5 = MD5("nobody" + path + "use" + json + "md5forencrypt")`.
//! `path` is the `/api/...` form of the endpoint, while the request itself goes to `/eapi/...`.

use aes::Aes128;
use cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyInit, block_padding::Pkcs7};
use md5::{Digest, Md5};

use crate::error::{Error, Result};

const EAPI_KEY: &[u8; 16] = b"e82ckenh8dichen8";
const EAPI_SEPARATOR: &str = "-36cd479b6b5-";

pub fn md5_hex(data: impl AsRef<[u8]>) -> String {
    hex::encode(Md5::digest(data.as_ref()))
}

fn aes_ecb_encrypt(plain: &[u8]) -> Vec<u8> {
    ecb::Encryptor::<Aes128>::new(EAPI_KEY.into()).encrypt_padded_vec::<Pkcs7>(plain)
}

/// Build the value of the `params` form field for an eapi request.
pub fn eapi_params(api_path: &str, json: &str) -> String {
    let digest = md5_hex(format!("nobody{api_path}use{json}md5forencrypt"));
    let plain = format!("{api_path}{EAPI_SEPARATOR}{json}{EAPI_SEPARATOR}{digest}");
    hex::encode_upper(aes_ecb_encrypt(plain.as_bytes()))
}

const ANONYMOUS_ID_KEY: &[u8] = b"3go8&$8*3*3h0k(2)2";

/// The `username` of the anonymous guest registration: `base64(<id> + " " + base64(md5(id XOR key)))`.
/// The XOR key is the one the desktop client uses for its `encodeAnonymousId` bridge function.
pub fn anonymous_username(device_id: &str) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    let xored: Vec<u8> = device_id.bytes().enumerate().map(|(i, b)| b ^ ANONYMOUS_ID_KEY[i % ANONYMOUS_ID_KEY.len()]).collect();
    let digest = Md5::digest(&xored);
    STANDARD.encode(format!("{device_id} {}", STANDARD.encode(digest)))
}

/// Decrypt an eapi payload (raw ciphertext bytes, as sent for `e_r: true` responses).
pub fn eapi_decrypt(cipher: &[u8]) -> Result<Vec<u8>> {
    ecb::Decryptor::<Aes128>::new(EAPI_KEY.into())
        .decrypt_padded_vec::<Pkcs7>(cipher)
        .map_err(|e| Error::Crypto(format!("eapi decrypt failed: {e}")))
}

/// Decrypt a `params` request body back to `(api_path, json, md5)` — used to self-test the codec.
pub fn eapi_open_params(params_hex: &str) -> Result<(String, String, String)> {
    let raw = hex::decode(params_hex).map_err(|e| Error::Crypto(e.to_string()))?;
    let plain = String::from_utf8(eapi_decrypt(&raw)?).map_err(|e| Error::Crypto(e.to_string()))?;
    let mut parts = plain.splitn(3, EAPI_SEPARATOR);
    match (parts.next(), parts.next(), parts.next()) {
        (Some(p), Some(j), Some(d)) => Ok((p.to_owned(), j.to_owned(), d.to_owned())),
        _ => Err(Error::Crypto("unexpected eapi plaintext layout".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let json = r#"{"ids":"[347230]","level":"exhigh","encodeType":"flac"}"#;
        let params = eapi_params("/api/song/enhance/player/url/v1", json);
        assert!(params.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
        let (path, body, digest) = eapi_open_params(&params).unwrap();
        assert_eq!(path, "/api/song/enhance/player/url/v1");
        assert_eq!(body, json);
        assert_eq!(digest, md5_hex(format!("nobody{path}use{json}md5forencrypt")));
    }

    #[test]
    fn anonymous_username_layout() {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD;
        let name = anonymous_username("0123456789ABCDEF0123456789ABCDEF");
        let decoded = String::from_utf8(STANDARD.decode(&name).unwrap()).unwrap();
        let (id, digest) = decoded.split_once(' ').unwrap();
        assert_eq!(id, "0123456789ABCDEF0123456789ABCDEF");
        assert_eq!(STANDARD.decode(digest).unwrap().len(), 16, "an MD5 digest");
        assert_eq!(name, anonymous_username("0123456789ABCDEF0123456789ABCDEF"), "deterministic");
    }

    #[test]
    fn md5_known_vector() {
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }
}
