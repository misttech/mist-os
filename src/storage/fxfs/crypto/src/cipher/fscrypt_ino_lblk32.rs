// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use super::{Cipher, Tweak, UnwrappedKey, XtsProcessor};
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes256;
use anyhow::Error;
use siphasher::sip::SipHasher;
use std::hash::Hasher;
use zerocopy::IntoBytes;

#[derive(Debug)]
pub(super) struct FscryptInoLblk32FileCipher {
    xts_key1: Aes256,
    xts_key2: Aes256,
    ino_hash_key: [u8; 16],
}
impl FscryptInoLblk32FileCipher {
    pub fn new(key: &UnwrappedKey) -> Self {
        Self {
            xts_key1: Aes256::new(GenericArray::from_slice(&key[..32])),
            xts_key2: Aes256::new(GenericArray::from_slice(&key[32..64])),
            ino_hash_key: key[64..80].try_into().unwrap(),
        }
    }

    #[inline(always)]
    fn tweak(&self, ino: u64, block_num: u32) -> u128 {
        let mut hasher = SipHasher::new_with_key(&self.ino_hash_key);
        let ino64 = ino as u64;
        hasher.write(ino64.as_bytes());
        (hasher.finish() as u32 + block_num) as u128
    }
}
impl Cipher for FscryptInoLblk32FileCipher {
    fn encrypt(
        &self,
        ino: u64,
        _device_offset: u64,
        file_offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), Error> {
        fxfs_trace::duration!(c"encrypt", "len" => buffer.len());
        assert_eq!(file_offset % 4096, 0);
        let block_num = file_offset as u32 / 4096;
        let mut tweak = self.tweak(ino, block_num);

        for block in buffer.chunks_exact_mut(4096 as usize) {
            self.xts_key2.encrypt_block(GenericArray::from_mut_slice(tweak.as_mut_bytes()));
            self.xts_key1.encrypt_with_backend(XtsProcessor::new(Tweak(tweak), block));
            tweak += 1;
        }
        Ok(())
    }

    fn decrypt(
        &self,
        ino: u64,
        _device_offset: u64,
        file_offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), Error> {
        fxfs_trace::duration!(c"decrypt", "len" => buffer.len());
        assert_eq!(file_offset % 4096, 0);
        let block_num = file_offset as u32 / 4096;
        let mut tweak = self.tweak(ino, block_num);
        for block in buffer.chunks_exact_mut(4096 as usize) {
            self.xts_key2.encrypt_block(GenericArray::from_mut_slice(tweak.as_mut_bytes()));
            self.xts_key1.decrypt_with_backend(XtsProcessor::new(Tweak(tweak), block));
            tweak += 1;
        }
        Ok(())
    }

    fn encrypt_filename(&self, _object_id: u64, _buffer: &mut Vec<u8>) -> Result<(), Error> {
        let e: Error = zx_status::Status::NOT_SUPPORTED.into();
        Err(e.context("encrypt_filename not supported for InoLblk32File"))
    }

    fn decrypt_filename(&self, _object_id: u64, _buffer: &mut Vec<u8>) -> Result<(), Error> {
        let e: Error = zx_status::Status::NOT_SUPPORTED.into();
        Err(e.context("decrypt_filename not supported for InoLblk32File"))
    }

    fn hash_code(&self, _raw_filename: &[u8]) -> u32 {
        debug_assert!(false, "hash_code called on file cipher");
        0
    }

    fn hash_code_casefold(&self, _filename: &str) -> u32 {
        debug_assert!(false, "hash_code_casefold called on file cipher");
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{FscryptInoLblk32FileCipher, UnwrappedKey};
    use crate::Cipher;
    use std::sync::Arc;

    /// Test data for byte offset 4096 produced by:
    ///
    /// ```python3
    /// from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
    /// from cryptography.hazmat.backends import default_backend
    /// import siphashc
    /// keys = bytes([i for i in range(80)])
    /// inode = 0x02468ace.to_bytes(8,'little')
    /// tweak = ((siphashc.siphash(bytes(keys[64:]), inode) + 1) & 0xffffffff).to_bytes(16,'little')
    /// plaintext = b"This is aligned sensitive data!!"
    /// cipher = Cipher(algorithms.AES(keys[:64]), modes.XTS(tweak), backend=default_backend())
    /// encryptor = cipher.encryptor()
    /// ciphertext = encryptor.update(plaintext) + encryptor.finalize()
    /// print("Tweak:", tweak)
    /// print("Encrypted:", ciphertext)
    /// ```
    const PLAINTEXT: &[u8] = b"This is aligned sensitive data!!";
    const INODE: u32 = 0x02468ace;
    const _TWEAK: &[u8] = b"\xda\xeb\x8c\xf1\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
    const ENCRYPTED: &[u8] = b"\x94\xd3\xfc\xc9\xac\x1e:;\n\x87\x9ec\x93/\xa1l+\xad\xbc\xff\xbcf\x87\xe5\x8f\xf0\xf3\x0c\xc0$\xb8/";
    #[test]
    fn test_encrypt() {
        let unwrapped_key = UnwrappedKey::new((0..80).collect());
        let cipher: Arc<dyn Cipher> = Arc::new(FscryptInoLblk32FileCipher::new(&unwrapped_key));
        let object_id = INODE as u64;

        let mut text = PLAINTEXT.to_vec();
        text.resize(4096, 0);
        cipher.encrypt(object_id, 16384, 0, &mut text).expect("encrypt failed");
        text.truncate(PLAINTEXT.len());
        assert_ne!(text, ENCRYPTED);

        let mut text = PLAINTEXT.to_vec();
        text.resize(4096, 0);
        cipher.encrypt(0, 16384, 4096, &mut text).expect("encrypt failed");
        text.truncate(PLAINTEXT.len());
        assert_ne!(text, ENCRYPTED);

        let mut text = PLAINTEXT.to_vec();
        text.resize(4096, 0);
        cipher.encrypt(object_id, 16384, 4096, &mut text).expect("encrypt failed");
        text.truncate(PLAINTEXT.len());
        assert_eq!(text, ENCRYPTED);
    }

    #[test]
    fn test_decrypt() {
        let unwrapped_key = UnwrappedKey::new((0..80).collect());
        let cipher: Arc<dyn Cipher> = Arc::new(FscryptInoLblk32FileCipher::new(&unwrapped_key));
        let object_id = INODE as u64;

        let mut text = ENCRYPTED.to_vec();
        text.resize(4096, 0);
        cipher.decrypt(object_id, 16384, 0, &mut text).expect("encrypt failed");
        text.truncate(PLAINTEXT.len());
        assert_ne!(text, PLAINTEXT);

        let mut text = ENCRYPTED.to_vec();
        text.resize(4096, 0);
        cipher.decrypt(0, 16384, 4096, &mut text).expect("encrypt failed");
        text.truncate(PLAINTEXT.len());
        assert_ne!(text, PLAINTEXT);

        let mut text = ENCRYPTED.to_vec();
        text.resize(4096, 0);
        cipher.decrypt(object_id, 16384, 4096, &mut text).expect("encrypt failed");
        text.truncate(PLAINTEXT.len());
        assert_eq!(text, PLAINTEXT);
    }
}
