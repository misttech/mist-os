// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use fuchsia_zircon::{AsHandleRef, HandleBased, Koid};
use starnix_logging::{impossible_error, set_zx_name};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use std::mem::MaybeUninit;
use zerocopy::FromBytes;

#[derive(Debug, PartialEq, Eq)]
pub enum MemoryObject {
    Vmo(zx::Vmo),
}

impl From<zx::Vmo> for MemoryObject {
    fn from(vmo: zx::Vmo) -> Self {
        Self::Vmo(vmo)
    }
}

impl MemoryObject {
    pub fn as_vmo(&self) -> Option<&zx::Vmo> {
        match self {
            Self::Vmo(vmo) => Some(&vmo),
        }
    }

    pub fn into_vmo(self) -> Option<zx::Vmo> {
        match self {
            Self::Vmo(vmo) => Some(vmo),
        }
    }

    pub fn get_content_size(&self) -> u64 {
        match self {
            Self::Vmo(vmo) => vmo.get_content_size().map_err(impossible_error).unwrap(),
        }
    }

    pub fn set_content_size(&self, size: u64) {
        match self {
            Self::Vmo(vmo) => vmo.set_content_size(&size).map_err(impossible_error).unwrap(),
        }
    }

    pub fn get_size(&self) -> u64 {
        match self {
            Self::Vmo(vmo) => vmo.get_size().map_err(impossible_error).unwrap(),
        }
    }

    pub fn set_size(&self, size: u64) -> Result<(), zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.set_size(size),
        }
    }

    pub fn create_child(
        &self,
        option: zx::VmoChildOptions,
        offset: u64,
        size: u64,
    ) -> Result<Self, zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.create_child(option, offset, size).map(Self::from),
        }
    }

    pub fn duplicate_handle(&self, rights: zx::Rights) -> Result<Self, zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.duplicate_handle(rights).map(Self::from),
        }
    }

    pub fn read(&self, data: &mut [u8], offset: u64) -> Result<(), zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.read(data, offset),
        }
    }

    pub fn read_to_array<T: FromBytes, const N: usize>(
        &self,
        offset: u64,
    ) -> Result<[T; N], zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.read_to_array(offset),
        }
    }

    pub fn read_to_vec(&self, offset: u64, length: u64) -> Result<Vec<u8>, zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.read_to_vec(offset, length),
        }
    }

    pub fn read_uninit<'a>(
        &self,
        data: &'a mut [MaybeUninit<u8>],
        offset: u64,
    ) -> Result<&'a mut [u8], zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.read_uninit(data, offset),
        }
    }

    pub fn write(&self, data: &[u8], offset: u64) -> Result<(), zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.write(data, offset),
        }
    }

    pub fn basic_info(&self) -> zx::HandleBasicInfo {
        match self {
            Self::Vmo(vmo) => vmo.basic_info().map_err(impossible_error).unwrap(),
        }
    }

    pub fn get_koid(&self) -> Koid {
        self.basic_info().koid
    }

    pub fn info(&self) -> Result<zx::VmoInfo, Errno> {
        match self {
            Self::Vmo(vmo) => vmo.info().map_err(|_| errno!(EIO)),
        }
    }

    pub fn set_zx_name(&self, name: &[u8]) {
        match self {
            Self::Vmo(vmo) => set_zx_name(vmo, name),
        }
    }

    pub fn op_range(&self, op: zx::VmoOp, offset: u64, size: u64) -> Result<(), zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.op_range(op, offset, size),
        }
    }

    pub fn replace_as_executable(self, vmex: &zx::Resource) -> Result<Self, zx::Status> {
        match self {
            Self::Vmo(vmo) => vmo.replace_as_executable(vmex).map(Self::from),
        }
    }

    pub fn map_in_vmar(
        &self,
        vmar: &zx::Vmar,
        vmar_offset: usize,
        memory_offset: u64,
        len: usize,
        flags: zx::VmarFlags,
    ) -> Result<usize, zx::Status> {
        match self {
            Self::Vmo(vmo) => vmar.map(vmar_offset, vmo, memory_offset, len, flags),
        }
    }
}
