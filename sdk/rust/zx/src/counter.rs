// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon cuonter objects.

use crate::{ok, sys, AsHandleRef, Handle, HandleBased, HandleRef, Status};

/// An object representing a Zircon
/// [counter](https://fuchsia.dev/fuchsia-src/concepts/objects/counter.md).
///
/// As essentially a subtype of `Handle`, it can be freely interconverted.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Counter(Handle);
impl_handle_based!(Counter);

impl Counter {
    pub fn create() -> Result<Counter, Status> {
        let options = 0;
        let mut handle = 0;
        let status = unsafe { sys::zx_counter_create(options, &mut handle) };
        ok(status)?;
        unsafe { Ok(Counter::from(Handle::from_raw(handle))) }
    }

    pub fn add(&self, value: i64) -> Result<(), Status> {
        let status = unsafe { sys::zx_counter_add(self.raw_handle(), value) };
        ok(status)
    }

    pub fn read(&self) -> Result<i64, Status> {
        let mut value = 0;
        let status = unsafe { sys::zx_counter_read(self.raw_handle(), &mut value) };
        ok(status).map(|()| value)
    }

    pub fn write(&self, value: i64) -> Result<(), Status> {
        let status = unsafe { sys::zx_counter_write(self.raw_handle(), value) };
        ok(status)
    }
}

// These tests are intended to verify the Rust bindings rather the counter
// Zircon object.  The tests for the Zircon object are part of the core-tests
// suite.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_create() {
        let counter = Counter::create().unwrap();
        assert_eq!(counter.read().unwrap(), 0);
    }

    #[test]
    fn counter_add() {
        let counter = Counter::create().unwrap();
        assert_eq!(counter.read().unwrap(), 0);
        assert!(counter.add(i64::max_value()).is_ok());
        assert_eq!(counter.read().unwrap(), i64::max_value());
        assert_eq!(counter.add(1), Err(Status::OUT_OF_RANGE));
    }

    #[test]
    fn counter_read_write() {
        let counter = Counter::create().unwrap();
        assert_eq!(counter.read().unwrap(), 0);
        assert!(counter.write(i64::min_value()).is_ok());
        assert_eq!(counter.read().unwrap(), i64::min_value());
    }
}
