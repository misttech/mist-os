// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon object information.

use crate::{ok, sys, HandleRef, Status};
use std::mem::MaybeUninit;
use std::ops::Deref;
use zerocopy::{FromBytes, Immutable};

// Tuning constants for get_info_vec(). pub(crate) to support unit tests.
pub(crate) const INFO_VEC_SIZE_INITIAL: usize = 16;
const INFO_VEC_SIZE_PAD: usize = 2;

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct Topic(sys::zx_object_info_topic_t);

impl Deref for Topic {
    type Target = sys::zx_object_info_topic_t;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A query to get info about a zircon object.
///
/// # Safety
///
/// `InfoTy` must be the same size as what the kernel expects to return for the provided topic.
pub(crate) unsafe trait ObjectQuery {
    /// A `Topic` identifying this query.
    const TOPIC: Topic;
    /// The datatype returned by querying for Self::TOPIC.
    type InfoTy: FromBytes + Immutable;
}

assoc_values!(Topic, [
    NONE = sys::ZX_INFO_NONE;
    HANDLE_VALID = sys::ZX_INFO_HANDLE_VALID;
    HANDLE_BASIC = sys::ZX_INFO_HANDLE_BASIC;
    PROCESS = sys::ZX_INFO_PROCESS;
    PROCESS_THREADS = sys::ZX_INFO_PROCESS_THREADS;
    VMAR = sys::ZX_INFO_VMAR;
    VMAR_MAPS = sys::ZX_INFO_VMAR_MAPS;
    JOB_CHILDREN = sys::ZX_INFO_JOB_CHILDREN;
    JOB_PROCESSES = sys::ZX_INFO_JOB_PROCESSES;
    THREAD = sys::ZX_INFO_THREAD;
    THREAD_EXCEPTION_REPORT = sys::ZX_INFO_THREAD_EXCEPTION_REPORT;
    TASK_STATS = sys::ZX_INFO_TASK_STATS;
    TASK_RUNTIME = sys::ZX_INFO_TASK_RUNTIME;
    PROCESS_MAPS = sys::ZX_INFO_PROCESS_MAPS;
    PROCESS_VMOS = sys::ZX_INFO_PROCESS_VMOS;
    THREAD_STATS = sys::ZX_INFO_THREAD_STATS;
    CPU_STATS = sys::ZX_INFO_CPU_STATS;
    KMEM_STATS = sys::ZX_INFO_KMEM_STATS;
    KMEM_STATS_EXTENDED = sys::ZX_INFO_KMEM_STATS_EXTENDED;
    KMEM_STATS_COMPRESSION = sys::ZX_INFO_KMEM_STATS_COMPRESSION;
    RESOURCE = sys::ZX_INFO_RESOURCE;
    HANDLE_COUNT = sys::ZX_INFO_HANDLE_COUNT;
    BTI = sys::ZX_INFO_BTI;
    PROCESS_HANDLE_STATS = sys::ZX_INFO_PROCESS_HANDLE_STATS;
    SOCKET = sys::ZX_INFO_SOCKET;
    VMO = sys::ZX_INFO_VMO;
    JOB = sys::ZX_INFO_JOB;
    IOB = sys::ZX_INFO_IOB;
    IOB_REGIONS = sys::ZX_INFO_IOB_REGIONS;
]);

/// Query information about a zircon object. Returns a valid slice and any remaining capacity on
/// success, along with a count of how many infos the kernel had available.
pub(crate) fn object_get_info<'a, Q: ObjectQuery>(
    handle: HandleRef<'_>,
    out: &'a mut [MaybeUninit<Q::InfoTy>],
) -> Result<(&'a mut [Q::InfoTy], &'a mut [MaybeUninit<Q::InfoTy>], usize), Status>
where
    Q::InfoTy: FromBytes + Immutable,
{
    let mut actual = 0;
    let mut avail = 0;

    // SAFETY: The slice pointer is known valid to write to for `size_of_val` because it came from
    // a mutable reference.
    let status = unsafe {
        sys::zx_object_get_info(
            handle.raw_handle(),
            *Q::TOPIC,
            out.as_mut_ptr().cast::<u8>(),
            std::mem::size_of_val(out),
            &mut actual,
            &mut avail,
        )
    };
    ok(status)?;

    let (initialized, uninit) = out.split_at_mut(actual);

    // TODO(https://fxbug.dev/352398385) switch to MaybeUninit::slice_assume_init_mut
    // SAFETY: these values have been initialized by the kernel and implement the right zerocopy
    // traits to be instantiated from arbitrary bytes.
    let initialized: &mut [Q::InfoTy] = unsafe {
        std::slice::from_raw_parts_mut(
            initialized.as_mut_ptr().cast::<Q::InfoTy>(),
            initialized.len(),
        )
    };

    Ok((initialized, uninit, avail))
}

/// Query information about a zircon object, expecting only a single info in the return.
pub(crate) fn object_get_info_single<Q: ObjectQuery>(
    handle: HandleRef<'_>,
) -> Result<Q::InfoTy, Status>
where
    Q::InfoTy: Copy + FromBytes + Immutable,
{
    let mut info = MaybeUninit::<Q::InfoTy>::uninit();
    let (info, _, _) = object_get_info::<Q>(handle, std::slice::from_mut(&mut info))?;
    Ok(info[0])
}

/// Query multiple records of information about a zircon object.
/// Returns a vec of Q::InfoTy on success.
/// Intended for calls that return multiple small objects.
pub(crate) fn object_get_info_vec<Q: ObjectQuery>(
    handle: HandleRef<'_>,
) -> Result<Vec<Q::InfoTy>, Status> {
    // Start with a few slots
    let mut out = Vec::<Q::InfoTy>::with_capacity(INFO_VEC_SIZE_INITIAL);
    loop {
        let (init, _uninit, avail) =
            object_get_info::<Q>(handle.clone(), out.spare_capacity_mut())?;
        let num_initialized = init.len();
        if num_initialized == avail {
            // SAFETY: the kernel has initialized all of these values.
            unsafe { out.set_len(num_initialized) };
            return Ok(out);
        } else {
            // The number of records may increase between retries; reserve space for that.
            let needed_space = avail * INFO_VEC_SIZE_PAD;
            if let Some(to_grow) = needed_space.checked_sub(out.capacity()) {
                out.reserve_exact(to_grow);
            }
        }
    }
}
