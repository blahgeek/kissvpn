use anyhow::Result;
use bytes::BytesMut;
use sha2::Sha256;
use hkdf::Hkdf;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, AeadCore, AeadInPlace, Nonce};

pub struct Cipher {
    chacha20: ChaCha20Poly1305,
}

// TODO: how to get these values from chacha20 crate
const KEY_SIZE: usize = 32;
const NONCE_SIZE: usize = 12;

impl Cipher {
    pub fn new(passphrase: &str) -> Cipher {
        let hkdf = Hkdf::<Sha256>::new(None, passphrase.as_bytes());
        let mut key = [0 as u8; KEY_SIZE];
        hkdf.expand(&[], &mut key).unwrap();

        Cipher {
            chacha20: ChaCha20Poly1305::new_from_slice(&key).unwrap(),
        }
    }

    // Encrypt in-place. The buffer capacity must be large enough.
    pub fn encrypt(&self, buf: &mut BytesMut) -> Result<()> {
        let mut rng = rand::thread_rng();
        let nonce = ChaCha20Poly1305::generate_nonce(&mut rng);

        self.chacha20.encrypt_in_place(&nonce, &[], buf)?;
        buf.extend_from_slice(&nonce);

        Ok(())
    }

    pub fn decrypt(&self, buf: &mut BytesMut) -> Result<()> {
        if buf.len() < NONCE_SIZE {
            anyhow::bail!("Invalid length {}", buf.len());
        }
        let nonce_buf = buf.split_off(buf.len() - NONCE_SIZE);
        let nonce = Nonce::from_slice(&nonce_buf);

        self.chacha20.decrypt_in_place(&nonce, &[], buf)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_encrypt_decrypt() -> Result<()> {
        let ciphertext = {
            let cipher = Cipher::new("key0");

            let mut buf = BytesMut::from("hello world!");
            buf.reserve(100);
            cipher.encrypt(&mut buf)?;

            assert_eq!(buf.len(), 12 + NONCE_SIZE + 16);
            buf
        };

        {
            let mut buf = ciphertext.clone();
            let cipher = Cipher::new("key0");

            cipher.decrypt(&mut buf)?;
            assert_eq!(buf, "hello world!");

            // fail to decrypt
            let mut buf = BytesMut::from("sksksksksksksksks");
            assert!(cipher.decrypt(&mut buf).is_err());
        }

        {
            let mut buf = ciphertext.clone();
            let cipher = Cipher::new("key1");
            assert!(cipher.decrypt(&mut buf).is_err());
        }

        Ok(())
    }
}
