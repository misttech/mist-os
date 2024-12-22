// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_logging::with_zx_name;
use std::marker::PhantomData;
use std::mem::{align_of, size_of};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use zerocopy::{Immutable, IntoBytes};
use zx::{self as zx, AsHandleRef as _, HandleBased as _};

/// Declare an instance of [`SeqLock`] by supplying header([`H`]) and value([`T`]) types,
/// which should be configured with C-style layout & alignment.
/// [`SeqLock`] will place a 32-bit atomic sequence number in-between the
/// header and value, in a VMO.
pub struct SeqLock<H: IntoBytes + Immutable, T: IntoBytes + Immutable> {
    map_addr: usize,
    readonly_vmo: Arc<zx::Vmo>,
    writable_vmo: zx::Vmo,
    _phantom_data: PhantomData<(H, T)>,
}

impl<H: IntoBytes + Default + Immutable, T: IntoBytes + Default + Immutable> SeqLock<H, T> {
    pub fn new_default() -> Result<Self, zx::Status> {
        Self::new(H::default(), T::default())
    }
}

const fn sequence_offset<H>() -> usize {
    let offset = size_of::<H>();
    assert!(offset % align_of::<AtomicU32>() == 0, "Sequence must be correctly aligned");
    offset
}

const fn value_offset<H, T>() -> usize {
    let offset = sequence_offset::<H>() + size_of::<AtomicU32>();
    assert!(offset % align_of::<T>() == 0, "Value alignment must allow packing without padding");
    offset
}

const fn vmo_size<H, T>() -> usize {
    value_offset::<H, T>() + size_of::<T>()
}

impl<H: IntoBytes + Immutable, T: IntoBytes + Immutable> SeqLock<H, T> {
    /// Returns an instance with initial values and a read-only VMO handle.
    /// May fail if the VMO backing the structure cannot be created, duplicated
    /// read-only, or mapped.
    pub fn new(header: H, value: T) -> Result<Self, zx::Status> {
        // Create a VMO sized to hold the header, value, and sequence number.
        let writable_vmo =
            with_zx_name(zx::Vmo::create(vmo_size::<H, T>() as u64)?, b"starnix:selinux");

        // Populate the initial default values.
        writable_vmo.write(header.as_bytes(), 0)?;
        writable_vmo.write(value.as_bytes(), value_offset::<H, T>() as u64)?;

        // Create a readonly handle to the VMO.
        let writable_rights = writable_vmo.basic_info()?.rights;
        let readonly_rights = writable_rights.difference(zx::Rights::WRITE);
        let readonly_vmo = Arc::new(writable_vmo.duplicate_handle(readonly_rights)?);

        // Map the VMO writable by this object, and populate it.
        let flags = zx::VmarFlags::PERM_READ
            | zx::VmarFlags::ALLOW_FAULTS
            | zx::VmarFlags::REQUIRE_NON_RESIZABLE
            | zx::VmarFlags::PERM_WRITE;

        let status = Self {
            map_addr: fuchsia_runtime::vmar_root_self().map(
                0,
                &writable_vmo,
                0,
                vmo_size::<H, T>(),
                flags,
            )?,
            readonly_vmo: readonly_vmo,
            writable_vmo: writable_vmo,
            _phantom_data: PhantomData,
        };

        Ok(status)
    }

    /// Returns a read-only handle to the VMO containing the header, atomic
    /// sequence number, and value.
    pub fn get_readonly_vmo(&self) -> Arc<zx::Vmo> {
        self.readonly_vmo.clone()
    }

    /// Updates the value held by this instance using the Seqlock pattern.
    pub fn set_value(&mut self, value: T) {
        // SAFETY: `map_addr` is sized to fit `H` and `T` and unmapped when
        // `self` is dropped.
        let sequence =
            unsafe { &mut *((self.map_addr + sequence_offset::<H>()) as *mut AtomicU32) };
        let old_sequence = sequence.fetch_add(1, Ordering::Relaxed);

        // [`SecurityServer`] ensures mutual exclusion of writers, so the
        // old `sequence` value must always be even (i.e. unlocked).
        assert!((old_sequence % 2) == 0, "expected sequence to be unlocked");

        self.writable_vmo.write(value.as_bytes(), value_offset::<H, T>() as u64).unwrap();

        sequence.fetch_add(1, Ordering::Release);
    }
}

impl<H: IntoBytes + Immutable, T: IntoBytes + Immutable> Drop for SeqLock<H, T> {
    fn drop(&mut self) {
        // SAFETY: `self` owns the mapping, and does not dispense any references
        // to it.
        unsafe {
            fuchsia_runtime::vmar_root_self()
                .unmap(self.map_addr, vmo_size::<H, T>())
                .expect("failed to unmap SeqLock");
        }
    }
}
