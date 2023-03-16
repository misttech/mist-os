// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{PtrEq, ReadBytes, ReadableBlockContainer, WritableBlockContainer, WriteBytes};
use std::{
    convert::TryFrom,
    sync::{Arc, Mutex},
};

impl ReadBytes for Arc<Mutex<Vec<u8>>> {
    #[inline]
    fn read_at(&self, offset: usize, dst: &mut [u8]) {
        let guard = self.lock().unwrap();
        ReadBytes::read_at(&guard.as_slice(), offset, dst);
    }

    /// The number of bytes in the buffer.
    #[inline]
    fn len(&self) -> usize {
        self.lock().unwrap().len()
    }
}

#[derive(Clone, Debug)]
pub struct Container {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl PtrEq for Container {
    fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl ReadableBlockContainer for Container {
    type BackingStorage = Arc<Mutex<Vec<u8>>>;
    type Error = ();

    #[inline]
    fn read_only(storage: &Self::BackingStorage) -> Result<Self, Self::Error> {
        Ok(Self { inner: storage.clone() })
    }
}

impl ReadBytes for Container {
    #[inline]
    fn read_at(&self, offset: usize, dst: &mut [u8]) {
        self.inner.read_at(offset, dst);
    }

    /// The number of bytes in the buffer.
    #[inline]
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl WritableBlockContainer for Container {
    #[inline]
    fn read_and_write(size: usize) -> Result<(Self, Self::BackingStorage), ()> {
        let inner = Arc::new(Mutex::new(vec![0; size]));
        let storage = inner.clone();
        Ok((Self { inner }, storage))
    }
}

impl WriteBytes for Container {
    #[inline]
    fn write_at(&mut self, offset: usize, src: &[u8]) -> usize {
        let guard = self.inner.lock().unwrap();
        if offset >= guard.len() {
            return 0;
        }
        let bytes_written = std::cmp::min(guard.len() - offset, src.len());
        let base = (guard.as_ptr() as usize).checked_add(offset).unwrap() as *mut u8;
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), base, bytes_written) };
        bytes_written
    }
}

impl TryFrom<&Arc<Mutex<Vec<u8>>>> for Container {
    type Error = ();

    fn try_from(storage: &Arc<Mutex<Vec<u8>>>) -> Result<Self, ()> {
        Container::read_only(storage)
    }
}
