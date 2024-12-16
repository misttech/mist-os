// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Implements Zxcrypt.  This is tested via the fshost tests.

use super::{AlignedMem, Fvm, IoTrait, ReadToMem, WriteFromMem};
use crate::device::{BufferGuard, Device, BUFFER_SIZE};
use aes::cipher::generic_array::GenericArray;
use aes::cipher::inout::InOut;
use aes::cipher::typenum::consts::U16;
use aes::cipher::{BlockBackend, BlockClosure, BlockDecrypt, BlockEncrypt, BlockSizeUser, KeyInit};
use aes::Aes256;
use anyhow::{ensure, Error};
use block_client::{BlockClient, BufferSlice, MutableBufferSlice, RemoteBlockClient, WriteOptions};

use futures::stream::{FuturesUnordered, TryStreamExt};
use zerocopy::{little_endian, FromBytes, Immutable, IntoBytes, KnownLayout};

pub struct Key {
    data_cipher: Aes256,
    iv_cipher: Aes256,
    iv: u128,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, FromBytes, Immutable, KnownLayout)]
struct ZxcryptHeaderAndKey {
    magic: u128,
    guid: [u8; 16],
    version: u32,

    // The header is immediately followed by keys a.k.a. "slots".  Only the first slot is used.

    // The wrapped key is 128 bit GCM SIV wrapping a 64-byte AES 256 XTS key and a 16 byte
    // IV.
    wrapped_key: [u8; 96],
}

const ZXCRYPT_MAGIC: u128 = 0x74707972_63787a80_e7116db3_00f8e85f;
const ZXCRYPT_VERSION: u32 = 0x01000000;

impl Key {
    pub async fn unseal(
        fvm: &Fvm,
        partition_index: u16,
        crypt: &fidl_fuchsia_fxfs::CryptProxy,
    ) -> Result<Self, Error> {
        // Read the first block which contains Zxcrypt's header.
        let block_size = fvm.device.block_size() as usize;
        let mut data = AlignedMem::<ZxcryptHeaderAndKey>::new(block_size);
        fvm.do_io(ReadToMem::new(&fvm.device, &mut data), partition_index, 0, 1).await?;
        let (zxcrypt_header, _) = ZxcryptHeaderAndKey::ref_from_prefix(&data).unwrap();

        ensure!(zxcrypt_header.magic == ZXCRYPT_MAGIC, zx::Status::WRONG_TYPE);
        ensure!(zxcrypt_header.version == ZXCRYPT_VERSION, zx::Status::NOT_SUPPORTED);

        // This is tightly coupled with the implementation of Crypt in //src/storage/crypt/zxcrypt.
        // It expects to receive the zxcrypt header (which includes the magic, guid and version)
        // followed by the wrapped key.  The unwrapped key consists of 64 bytes for the XTS key
        // which is made up of two 32 bytes Aes256 keys, one for the data and one for the IV/tweak,
        // followed by 16 bytes which make up the IV.
        let wrapping_key_id_0 = [0; 16];
        let unwrapped_key = crypt
            .unwrap_key(&wrapping_key_id_0, 0, &data[..std::mem::size_of::<ZxcryptHeaderAndKey>()])
            .await?
            .map_err(zx::Status::from_raw)?;

        Ok(Self {
            data_cipher: Aes256::new(GenericArray::from_slice(&unwrapped_key[..32])),
            iv_cipher: Aes256::new(GenericArray::from_slice(&unwrapped_key[32..64])),
            iv: little_endian::U128::from_bytes(unwrapped_key[64..80].try_into().unwrap()).get(),
        })
    }

    pub async fn format(
        fvm: &Fvm,
        partition_index: u16,
        crypt: &fidl_fuchsia_fxfs::CryptProxy,
    ) -> Result<Self, Error> {
        let block_size = fvm.device.block_size() as usize;
        let mut data = AlignedMem::<ZxcryptHeaderAndKey>::new(block_size);

        // Fill the block with random data.  This is what the old driver did.
        zx::cprng_draw(&mut data);

        let (_, key, unwrapped_key) = crypt
            .create_key(0, fidl_fuchsia_fxfs::KeyPurpose::Data)
            .await?
            .map_err(zx::Status::from_raw)?;
        ensure!(key.len() == std::mem::size_of::<ZxcryptHeaderAndKey>(), zx::Status::INTERNAL);

        data[..std::mem::size_of::<ZxcryptHeaderAndKey>()].copy_from_slice(&key);

        let (zxcrypt_header, _) = ZxcryptHeaderAndKey::ref_from_prefix(&data).unwrap();
        ensure!(zxcrypt_header.magic == ZXCRYPT_MAGIC, zx::Status::INTERNAL);

        // Make sure the first two slices are allocated.
        fvm.ensure_allocated(partition_index, 2).await?;
        fvm.do_io(WriteFromMem::new(&fvm.device, &data), partition_index, 0, 1).await?;

        Ok(Self {
            data_cipher: Aes256::new(GenericArray::from_slice(&unwrapped_key[..32])),
            iv_cipher: Aes256::new(GenericArray::from_slice(&unwrapped_key[32..64])),
            iv: little_endian::U128::from_bytes(unwrapped_key[64..80].try_into().unwrap()).get(),
        })
    }
}

pub struct EncryptedRead<'a> {
    device: &'a Device,
    key: &'a Key,
    tweak: u128,
    target_vmo: &'a zx::Vmo,
    vmo_offset: u64,
    buffer: BufferGuard<'a, RemoteBlockClient>,
    ops: Vec<(u64, u64)>,
    queued_len: u64,
}

impl<'a> EncryptedRead<'a> {
    pub async fn new(
        device: &'a Device,
        key: &'a Key,
        block_offset: u64,
        target_vmo: &'a zx::Vmo,
        vmo_offset: u64,
    ) -> Self {
        Self {
            device,
            key,
            tweak: key.iv + block_offset as u128,
            target_vmo,
            vmo_offset,
            buffer: device.get_buffer().await,
            ops: Vec::new(),
            queued_len: 0,
        }
    }
}

impl IoTrait for EncryptedRead<'_> {
    async fn add_op(&mut self, mut offset: u64, mut len: u64) -> Result<(), zx::Status> {
        loop {
            let space = BUFFER_SIZE as u64 - self.queued_len;
            if space >= len {
                break;
            }
            self.ops.push((offset, space));
            self.queued_len += space;
            self.flush().await?;
            offset += space;
            len -= space;
        }
        self.ops.push((offset, len));
        self.queued_len += len;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), zx::Status> {
        // Read into the buffer.
        let mut buf_offset = 0;
        let futures = FuturesUnordered::from_iter(self.ops.drain(..).map(|(offset, len)| {
            let fut = self.device.read_at(
                MutableBufferSlice::new_with_vmo_id(
                    self.buffer.vmo_id(),
                    self.buffer.vmo_offset() + buf_offset,
                    len,
                ),
                offset,
            );
            buf_offset += len;
            fut
        }));
        self.queued_len = 0;
        let () = futures.try_collect().await?;

        let buf = &mut self.buffer[..buf_offset as usize];

        // Decrypt the buffer
        let iv = &mut self.tweak;
        for chunk in buf.chunks_exact_mut(self.device.block_size() as usize) {
            let mut tweak = Tweak(*iv);
            self.key.iv_cipher.encrypt_block(GenericArray::from_mut_slice(tweak.as_mut_bytes()));
            self.key.data_cipher.decrypt_with_backend(XtsProcessor::new(tweak, chunk));
            *iv += 1;
        }

        // Write to the VMO.
        self.target_vmo.write(buf, self.vmo_offset)?;
        self.vmo_offset += buf_offset;
        Ok(())
    }
}

pub struct EncryptedWrite<'a> {
    device: &'a Device,
    key: &'a Key,
    tweak: u128,
    source_vmo: &'a zx::Vmo,
    vmo_offset: u64,
    options: WriteOptions,
    buffer: BufferGuard<'a, RemoteBlockClient>,
    ops: Vec<(u64, u64)>,
    queued_len: u64,
}

impl<'a> EncryptedWrite<'a> {
    pub async fn new(
        device: &'a Device,
        key: &'a Key,
        block_offset: u64,
        source_vmo: &'a zx::Vmo,
        vmo_offset: u64,
        options: WriteOptions,
    ) -> Self {
        Self {
            device,
            key,
            tweak: key.iv + block_offset as u128,
            source_vmo,
            vmo_offset,
            options,
            buffer: device.get_buffer().await,
            ops: Vec::new(),
            queued_len: 0,
        }
    }
}

impl IoTrait for EncryptedWrite<'_> {
    async fn add_op(&mut self, mut offset: u64, mut len: u64) -> Result<(), zx::Status> {
        loop {
            let space = BUFFER_SIZE as u64 - self.queued_len;
            if space >= len {
                break;
            }
            self.ops.push((offset, space));
            self.queued_len += space;
            self.flush().await?;
            offset += space;
            len -= space;
        }
        self.ops.push((offset, len));
        self.queued_len += len;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), zx::Status> {
        // Read from the source VMO.
        let buf = &mut self.buffer[..self.queued_len as usize];
        self.source_vmo.read(buf, self.vmo_offset)?;
        self.vmo_offset += self.queued_len;
        self.queued_len = 0;

        // Encrypt the buffer
        let iv = &mut self.tweak;
        for chunk in buf.chunks_exact_mut(self.device.block_size() as usize) {
            let mut tweak = Tweak(*iv);
            self.key.iv_cipher.encrypt_block(GenericArray::from_mut_slice(tweak.as_mut_bytes()));
            self.key.data_cipher.encrypt_with_backend(XtsProcessor::new(tweak, chunk));
            *iv += 1;
        }

        // Write to the device.
        let mut buf_offset = 0;
        FuturesUnordered::from_iter(self.ops.drain(..).map(|(offset, len)| {
            let fut = self.device.write_at_with_opts(
                BufferSlice::new_with_vmo_id(
                    self.buffer.vmo_id(),
                    self.buffer.vmo_offset() + buf_offset,
                    len,
                ),
                offset,
                self.options,
            );
            buf_offset += len;
            fut
        }))
        .try_collect()
        .await
    }
}

#[derive(FromBytes, IntoBytes)]
#[repr(C)]
struct Tweak(u128);

// To be used with encrypt|decrypt_with_backend.
struct XtsProcessor<'a> {
    tweak: Tweak,
    data: &'a mut [u8],
}

impl<'a> XtsProcessor<'a> {
    // `tweak` should be encrypted.  `data` should be a single sector and *must* be 16 byte aligned.
    fn new(tweak: Tweak, data: &'a mut [u8]) -> Self {
        assert_eq!(data.as_ptr() as usize & 15, 0, "data must be 16 byte aligned");
        Self { tweak, data }
    }
}

impl BlockSizeUser for XtsProcessor<'_> {
    type BlockSize = U16;
}

impl BlockClosure for XtsProcessor<'_> {
    fn call<B: BlockBackend<BlockSize = Self::BlockSize>>(self, backend: &mut B) {
        let Self { mut tweak, data } = self;
        for chunk in data.chunks_exact_mut(16) {
            let ptr = chunk.as_mut_ptr() as *mut u128;
            // SAFETY: We know each chunk is exactly 16 bytes and it should be safe to transmute to
            // u128 and GenericArray<u8, U16>.  There are safe ways of doing the following, but this
            // is extremely performance sensitive, and even seemingly innocuous changes here can
            // have an order-of-magnitude impact on what the compiler produces and that can be seen
            // in our benchmarks.  This assumes little-endianness which is likely to always be the
            // case.
            unsafe {
                *ptr ^= tweak.0;
                let chunk = ptr as *mut GenericArray<u8, U16>;
                backend.proc_block(InOut::from_raw(chunk, chunk));
                *ptr ^= tweak.0;
            }
            tweak.0 = (tweak.0 << 1) ^ ((tweak.0 as i128 >> 127) as u128 & 0x87);
        }
    }
}
