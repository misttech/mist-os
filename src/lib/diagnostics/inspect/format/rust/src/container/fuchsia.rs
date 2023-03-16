// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{PtrEq, ReadBytes, ReadableBlockContainer, WritableBlockContainer, WriteBytes};
use fuchsia_zircon as zx;
use mapped_vmo::Mapping;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Container {
    inner: Arc<Mapping>,
}

impl ReadBytes for zx::Vmo {
    #[inline]
    fn read_at(&self, offset: usize, dst: &mut [u8]) {
        zx::Vmo::read(&self, dst, offset as u64).ok();
    }

    #[inline]
    fn len(&self) -> usize {
        self.get_size().unwrap_or(0) as usize
    }
}

impl ReadBytes for Arc<zx::Vmo> {
    #[inline]
    fn read_at(&self, offset: usize, dst: &mut [u8]) {
        self.as_ref().read_at(offset, dst)
    }

    #[inline]
    fn len(&self) -> usize {
        self.as_ref().len()
    }
}

impl PtrEq for Container {
    fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl ReadableBlockContainer for Container {
    type BackingStorage = Arc<zx::Vmo>;
    type Error = zx::Status;

    #[inline]
    fn read_only(vmo: &Self::BackingStorage) -> Result<Self, Self::Error> {
        Self::try_from(vmo.as_ref())
    }
}

impl TryFrom<&zx::Vmo> for Container {
    type Error = zx::Status;
    fn try_from(vmo: &zx::Vmo) -> Result<Self, Self::Error> {
        let size = vmo.get_size()? as usize;
        let mapping =
            Arc::new(Mapping::create_from_vmo(&vmo, size as usize, zx::VmarFlags::PERM_READ)?);
        Ok(Self { inner: mapping })
    }
}

impl ReadBytes for Container {
    #[inline]
    fn read_at(&self, offset: usize, dst: &mut [u8]) {
        self.inner.read_at(offset, dst);
    }

    #[inline]
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl WritableBlockContainer for Container {
    #[inline]
    fn read_and_write(size: usize) -> Result<(Self, Self::BackingStorage), zx::Status> {
        let (mapping, vmo) = Mapping::allocate(size)?;
        Ok((Self { inner: Arc::new(mapping) }, Arc::new(vmo)))
    }
}

impl WriteBytes for Container {
    #[inline]
    fn write_at(&mut self, offset: usize, src: &[u8]) -> usize {
        self.inner.write_at(offset, src) as usize
    }
}
