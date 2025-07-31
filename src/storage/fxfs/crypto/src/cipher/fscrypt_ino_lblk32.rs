// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use super::{Cipher, Tweak, UnwrappedKey, XtsProcessor};
use aes::cipher::generic_array::GenericArray;
use aes::cipher::inout::InOutBuf;
use aes::cipher::{
    Block, BlockDecrypt, BlockDecryptMut, BlockEncrypt, BlockEncryptMut, KeyInit, KeyIvInit,
};
use aes::Aes256;
use anyhow::{Context, Error};
use siphasher::sip::SipHasher;
use std::hash::Hasher;
use zerocopy::IntoBytes;

const BLOCK_SIZE: usize = 4096;

#[derive(Debug)]
pub(crate) struct FscryptInoLblk32DirCipher {
    cts_key: [u8; 32],
    ino_hash_key: [u8; 16],
    dir_hash_key: [u8; 16],
}
impl FscryptInoLblk32DirCipher {
    pub fn new(key: &UnwrappedKey) -> Self {
        Self {
            cts_key: key[..32].try_into().unwrap(),
            ino_hash_key: key[32..48].try_into().unwrap(),
            dir_hash_key: key[48..64].try_into().unwrap(),
        }
    }
}
impl Cipher for FscryptInoLblk32DirCipher {
    fn encrypt(
        &self,
        _ino: u64,
        _device_offset: u64,
        _file_offset: u64,
        _buffer: &mut [u8],
    ) -> Result<(), Error> {
        Err(zx_status::Status::NOT_SUPPORTED).context("encrypt not supported for InoLblk32Dir")
    }

    fn decrypt(
        &self,
        _ino: u64,
        _device_offset: u64,
        _file_offset: u64,
        _buffer: &mut [u8],
    ) -> Result<(), Error> {
        Err(zx_status::Status::NOT_SUPPORTED).context("decrypt not supported for InoLblk32Dir")
    }
    fn encrypt_filename(&self, object_id: u64, buffer: &mut Vec<u8>) -> Result<(), Error> {
        let mut iv = [0u8; 16];

        // Add padding.
        while buffer.len() % 16 != 0 {
            buffer.push(0);
        }

        let mut hasher = SipHasher::new_with_key(&self.ino_hash_key);
        hasher.write(object_id.as_bytes());
        iv[..4].copy_from_slice(&hasher.finish().as_bytes()[..4]);

        // AES-256-CTS is used for filename encryption and symlinks but because we
        // require POLICY_FLAGS_PAD_16, we never actually steal any ciphertext and
        // so CTS is equivalent to swapping the last two blocks and using CBC instead.
        let mut cbc = cbc::Encryptor::<aes::Aes256>::new((&self.cts_key).into(), (&iv).into());
        let inout = InOutBuf::<'_, '_, u8>::from(&mut buffer[..]);
        let (mut blocks, tail): (InOutBuf<'_, '_, Block<aes::Aes256>>, _) = inout.into_chunks();
        debug_assert_eq!(tail.len(), 0);
        let mut chunks = blocks.get_out();
        cbc.encrypt_blocks_mut(&mut chunks);
        if chunks.len() >= 2 {
            chunks.swap(chunks.len() - 1, chunks.len() - 2);
        }
        Ok(())
    }

    fn decrypt_filename(&self, object_id: u64, buffer: &mut Vec<u8>) -> Result<(), Error> {
        let mut iv = [0u8; 16];
        let mut hasher = SipHasher::new_with_key(&self.ino_hash_key);
        hasher.write(object_id.as_bytes());
        iv[..4].copy_from_slice(&hasher.finish().as_bytes()[..4]);

        // AES-256-CTS is used for filename encryption and symlinks but because we
        // require POLICY_FLAGS_PAD_16, we never actually steal any ciphertext and
        // so CTS is equivalent to swapping the last two blocks and using CBC instead.
        let mut cbc = cbc::Decryptor::<aes::Aes256>::new((&self.cts_key).into(), (&iv).into());
        let inout = InOutBuf::<'_, '_, u8>::from(&mut buffer[..]);
        let (mut blocks, tail): (InOutBuf<'_, '_, Block<aes::Aes256>>, _) = inout.into_chunks();
        debug_assert_eq!(tail.len(), 0);
        let mut chunks = blocks.get_out();
        if chunks.len() >= 2 {
            chunks.swap(chunks.len() - 1, chunks.len() - 2);
        }
        cbc.decrypt_blocks_mut(&mut chunks);
        // Strip padding
        while let Some(0) = buffer.last() {
            buffer.pop();
        }
        Ok(())
    }

    fn hash_code(&self, raw_filename: &[u8], _filename: &str) -> u32 {
        fscrypt::direntry::tea_hash_filename(raw_filename)
    }

    fn hash_code_casefold(&self, filename: &str) -> u32 {
        let casefolded_filename: String = fxfs_unicode::casefold(filename.chars()).collect();
        fscrypt::direntry::casefold_encrypt_hash_filename(
            casefolded_filename.as_bytes(),
            &self.dir_hash_key,
        )
    }
}

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
        assert_eq!(file_offset % (BLOCK_SIZE as u64), 0);
        assert_eq!(buffer.len() % BLOCK_SIZE, 0);
        let block_num = file_offset as u32 / BLOCK_SIZE as u32;
        let mut tweak = self.tweak(ino, block_num);

        for block in buffer.chunks_exact_mut(BLOCK_SIZE) {
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
        assert_eq!(file_offset % BLOCK_SIZE as u64, 0);
        assert_eq!(buffer.len() % BLOCK_SIZE, 0);
        let block_num = file_offset as u32 / BLOCK_SIZE as u32;
        let mut tweak = self.tweak(ino, block_num);
        for block in buffer.chunks_exact_mut(BLOCK_SIZE) {
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

    fn hash_code(&self, _raw_filename: &[u8], _filename: &str) -> u32 {
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
    use fscrypt::proxy_filename::ProxyFilename;

    use super::{FscryptInoLblk32DirCipher, FscryptInoLblk32FileCipher, UnwrappedKey};
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

    #[test]
    fn test_encrypt_filename() {
        let mut unwrapped_key = UnwrappedKey::new([0; 64].to_vec());
        unwrapped_key.0[0] = 0x10;
        let cipher: Arc<dyn Cipher> = Arc::new(FscryptInoLblk32DirCipher::new(&unwrapped_key));
        let object_id = 2;

        // One block case.
        // ```shell
        // echo -n filename > in.txt ; truncate -s 16 in.txt
        // openssl aes-256-cbc -e -iv 014ae2cc000000000000000000000000 -nosalt -K 1000000000000000000000000000000000000000000000000000000000000000  -in in.txt -out out.txt -nopad
        // hexdump out.txt -e "16/1 \"%02x\" \"\n\"" -v
        // ```
        let mut text = b"filename".to_vec();
        cipher.encrypt_filename(object_id, &mut text).expect("encrypt filename failed");
        assert_eq!(text, hex::decode("2b7c885165f090393fcbb15f5018f18a").expect("decode failed"));

        // Two block case.
        // ```shell
        // echo -n "0123456789abcdef_filename" > in.txt ; truncate -s 16 in.txt
        // openssl aes-256-cbc -e -iv 014ae2cc000000000000000000000000 -nosalt -K 1000000000000000000000000000000000000000000000000000000000000000  -in in.txt -out out.txt -nopad
        // hexdump out.txt -e "16/1 \"%02x\" \"\n\"" -v
        // 3da06c8fc2e54065f391531affeae1fb
        // d6bad68cc11eb87719735fc50b7efbb3
        // <Swap the last two blocks and concatenate>
        // ``````
        let mut text = b"0123456789abcdef_filename".to_vec();
        cipher.encrypt_filename(object_id, &mut text).expect("encrypt filename failed");
        assert_eq!(
            text,
            hex::decode("d6bad68cc11eb87719735fc50b7efbb33da06c8fc2e54065f391531affeae1fb")
                .expect("decode failed")
        );

        // Test a 192 byte filename -- same as in test image (known to decrypt successfully).
        // ```shell
        // export LONG_NAME_16=xxxxxxxxyyyyyyyy
        // export LONG_NAME_32=${LONG_NAME_16}${LONG_NAME_16}
        // export LONG_NAME_64=${LONG_NAME_32}${LONG_NAME_32}
        // export LONG_NAME_128=${LONG_NAME_64}${LONG_NAME_64}
        // export LONG_NAME_192=${LONG_NAME_128}${LONG_NAME_64}
        // echo -n "${LONG_NAME_192}" > in.txt
        // openssl aes-256-cbc -e -iv 014ae2cc000000000000000000000000 -nosalt -K 1000000000000000000000000000000000000000000000000000000000000000  -in in.txt -out out.txt -nopad
        // hexdump out.txt -e "16/1 \"%02x\" \"\n\"" -v
        // f59d083c16915d5d3479b9dbf7b7f053
        // 1905bde71624f4ba1ab416b15831ca87
        // c2d99e43f97bd2fc18f2ad03da252715
        // abf9d0cd9bde4215bfeeec7d07dbcf89
        // 0bcc4a230faaaf73cabdfc3ca8b20a06
        // 84847f7f3991d55b6b30859dfc662c1a
        // ef03c7d16830ef7df367a3392a82e588
        // 629b89feffe49036e420686598545b20
        // 119c346af4f80fdbd225a625aa0f45ce
        // 393cfff0bd9971b6782d8768dbd13587
        // 38e3a65f8ef14612881e6cbd38cf4bcf
        // 08a75c38d9fb681304fdaa1e85a091ce
        // <Swap the last two blocks and concatenate>
        // ``````
        let long_name_64 = b"xxxxxxxxyyyyyyyyxxxxxxxxyyyyyyyyxxxxxxxxyyyyyyyyxxxxxxxxyyyyyyyy";
        let mut text = vec![];
        for _ in 0..3 {
            text.extend_from_slice(long_name_64);
        }

        let raw = hex::decode("f59d083c16915d5d3479b9dbf7b7f0531905bde71624f4ba1ab416b15831ca87c2d99e43f97bd2fc18f2ad03da252715abf9d0cd9bde4215bfeeec7d07dbcf890bcc4a230faaaf73cabdfc3ca8b20a0684847f7f3991d55b6b30859dfc662c1aef03c7d16830ef7df367a3392a82e588629b89feffe49036e420686598545b20119c346af4f80fdbd225a625aa0f45ce393cfff0bd9971b6782d8768dbd1358708a75c38d9fb681304fdaa1e85a091ce38e3a65f8ef14612881e6cbd38cf4bcf").expect("decode failed");
        cipher.encrypt_filename(object_id, &mut text).expect("encrypt filename failed");
        assert_eq!(text, raw);
        println!("Encoded to filename: {}", Into::<String>::into(ProxyFilename::new(0, &raw)));
    }

    #[test]
    fn test_decrypt_filename() {
        // Should be equivalent to:
        // ```shell
        // openssl aes-256-cbc -d -iv 014ae2cc000000000000000000000000 -nosalt -K 1000000000000000000000000000000000000000000000000000000000000000  -in in.txt -out out.txt -nopad
        // cat in.txt
        // ```
        let mut unwrapped_key = UnwrappedKey::new([0; 64].to_vec());
        unwrapped_key.0[0] = 0x10;
        let cipher: Arc<dyn Cipher> = Arc::new(FscryptInoLblk32DirCipher::new(&unwrapped_key));
        let object_id = 2;

        // One block case.
        let mut text = hex::decode("2b7c885165f090393fcbb15f5018f18a").expect("decode failed");
        cipher.decrypt_filename(object_id, &mut text).expect("encrypt filename failed");
        assert_eq!(text, b"filename".to_vec());

        // Two block case.
        let mut text =
            hex::decode("d6bad68cc11eb87719735fc50b7efbb33da06c8fc2e54065f391531affeae1fb")
                .expect("decode failed");
        cipher.decrypt_filename(object_id, &mut text).expect("encrypt filename failed");
        assert_eq!(text, b"0123456789abcdef_filename".to_vec());
    }
}
