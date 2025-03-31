// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::vmar::AllocatedVmar;
use super::{MapError, MapImpl, MapKey};
use ebpf::MapSchema;
use fuchsia_sync::Mutex;
use linux_uapi::{
    BPF_RB_FORCE_WAKEUP, BPF_RB_NO_WAKEUP, BPF_RINGBUF_BUSY_BIT, BPF_RINGBUF_DISCARD_BIT,
    BPF_RINGBUF_HDR_SZ,
};
use static_assertions::const_assert;
use std::fmt::Debug;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock};
use zx::AsHandleRef;

static PAGE_SIZE: LazyLock<usize> = LazyLock::new(|| zx::system_get_page_size() as usize);

// Signal used on ring buffer VMOs to indicate that the buffer has
// incoming data.
pub const RINGBUF_SIGNAL: zx::Signals = zx::Signals::USER_0;

#[derive(Debug)]
struct RingBufferState {
    /// The mask corresponding to the size of the ring buffer. This is used to map back the
    /// position in the ringbuffer (that are always growing) to their actual position in the memory
    /// object.
    mask: u32,

    /// The never decreasing position of the read head of the ring buffer. This is updated
    /// exclusively from userspace.
    consumer_position: &'static AtomicU32,

    /// The never decreasing position of the writing head of the ring buffer. This is updated
    /// exclusively from the kernel.
    producer_position: &'static AtomicU32,

    /// Pointer to the start of the data of the ring buffer.
    data: usize,
}

impl RingBufferState {
    /// The pointer into `data` that corresponds to `position`.
    fn data_position(&self, position: u32) -> usize {
        self.data + ((position & self.mask) as usize)
    }

    fn is_consumer_position(&self, addr: usize) -> bool {
        let Some(position) = addr.checked_sub(self.data) else {
            return false;
        };
        let position = position as u32;
        let consumer_position = self.consumer_position.load(Ordering::Acquire) & self.mask;
        position == consumer_position
    }

    /// Access the memory at `position` as a `RingBufferRecordHeader`.
    fn header_mut(&mut self, position: u32) -> &mut RingBufferRecordHeader {
        // SAFETY
        //
        // Reading / writing to the header is safe because the access is exclusive thanks to the
        // mutable reference to `self` and userspace has only a read only access to this memory.
        unsafe { &mut *(self.data_position(position) as *mut RingBufferRecordHeader) }
    }
}

#[derive(Debug)]
pub struct RingBuffer {
    /// VMO used to store the map content. Reference-counted to make it possible to share the
    /// handle with Starnix kernel, particularly for the case when a process needs to wait for
    /// signals from the VMO (see RINGBUF_SIGNAL).
    vmo: Arc<zx::Vmo>,

    /// Mutable state protected with a lock.
    state: Mutex<RingBufferState>,

    /// The specific memory address space used to map the ring buffer. This is the last field in
    /// the struct so that all the data that conceptually points to it is destroyed before the
    /// memory is unmapped.
    _vmar: AllocatedVmar,
}

impl RingBuffer {
    /// Build a new storage of a ring buffer. `size` must be a non zero multiple of the page size
    /// and a power of 2.
    ///
    /// This will create a mapping in the kernel user space with the following layout:
    ///
    /// |T| |C| |P| |D| |D|
    ///
    /// where:
    /// - T is 1 page containing at its 0 index a pointer to the `RingBuffer` itself.
    /// - C is 1 page containing at its 0 index a atomic u32 for the consumer position
    /// - P is 1 page containing at its 0 index a atomic u32 for the producer position
    /// - D is size bytes and is the content of the ring buffer.
    ///
    /// The returns value is a `Pin<Box>`, because the structure is self referencing and is
    /// required never to move in memory.
    pub fn new(schema: &MapSchema) -> Result<Pin<Box<Self>>, MapError> {
        if schema.key_size != 0 || schema.value_size != 0 {
            return Err(MapError::InvalidParam);
        }

        let page_size = *PAGE_SIZE;
        // Size must be a power of 2 and a multiple of page_size.
        let size = schema.max_entries as usize;
        if size == 0 || size % page_size != 0 || size & (size - 1) != 0 {
            return Err(MapError::InvalidParam);
        }
        let mask: u32 = (size - 1).try_into().map_err(|_| MapError::InvalidParam)?;
        // Add the 2 control pages
        let vmo_size = 2 * page_size + size;
        let kernel_root_vmar = fuchsia_runtime::vmar_root_self();
        // SAFETY
        //
        // The returned value and all pointer to the allocated memory will be part of `Self` and
        // all pointers will be dropped before the vmar. This ensures the deallocated memory will
        // not be used after it has been freed.
        let (vmar, base) = unsafe {
            AllocatedVmar::allocate(
                &kernel_root_vmar,
                0,
                // Allocate for one technical page, the 2 control pages and twice the size.
                page_size + vmo_size + size,
                zx::VmarFlags::CAN_MAP_SPECIFIC
                    | zx::VmarFlags::CAN_MAP_READ
                    | zx::VmarFlags::CAN_MAP_WRITE,
            )
            .map_err(|_| MapError::Internal)?
        };
        let technical_vmo = zx::Vmo::create(page_size as u64).map_err(|_| MapError::Internal)?;
        technical_vmo.set_name(&zx::Name::new_lossy("starnix:bpf")).unwrap();
        vmar.map(
            0,
            &technical_vmo,
            0,
            page_size,
            zx::VmarFlags::SPECIFIC | zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )
        .map_err(|_| MapError::Internal)?;

        let vmo = zx::Vmo::create(vmo_size as u64).map_err(|_| MapError::Internal)?;
        vmo.set_name(&zx::Name::new_lossy("starnix:bpf")).unwrap();
        vmar.map(
            page_size,
            &vmo,
            0,
            vmo_size,
            zx::VmarFlags::SPECIFIC | zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )
        .map_err(|_| MapError::Internal)?;
        vmar.map(
            page_size + vmo_size,
            &vmo,
            (page_size * 2) as u64,
            size,
            zx::VmarFlags::SPECIFIC | zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )
        .map_err(|_| MapError::Internal)?;

        // SAFETY
        //
        // This is safe as long as the vmar mapping stays alive. This will be ensured by the
        // `RingBuffer` itself.
        let storage_position = unsafe { &mut *((base) as *mut *const Self) };
        let consumer_position = unsafe { &*((base + page_size) as *const AtomicU32) };
        let producer_position = unsafe { &*((base + 2 * page_size) as *const AtomicU32) };
        let data = base + 3 * page_size;
        let storage = Box::pin(Self {
            vmo: Arc::new(vmo),
            state: Mutex::new(RingBufferState { mask, consumer_position, producer_position, data }),
            _vmar: vmar,
        });
        // Store the pointer to the storage to the start of the technical vmo. This is required to
        // access the storage from the bpf methods that only get a pointer to the reserved memory.
        // This is safe as the returned referenced is Pinned.
        *storage_position = storage.deref();
        Ok(storage)
    }

    /// Commits the section of the ringbuffer represented by the `header`. This only consist in
    /// updating the header length with the correct state bits and signaling the map fd.
    fn commit(
        &self,
        header: &RingBufferRecordHeader,
        flags: RingBufferWakeupPolicy,
        discard: bool,
    ) {
        let mut new_length = header.length.load(Ordering::Acquire) & !BPF_RINGBUF_BUSY_BIT;
        if discard {
            new_length |= BPF_RINGBUF_DISCARD_BIT;
        }
        header.length.store(new_length, Ordering::Release);

        // Send a signal either if it is forced, or it is the default and the committed entry is
        // the next one the client will consume.
        let state = self.state.lock();
        if flags == RingBufferWakeupPolicy::ForceWakeup
            || (flags == RingBufferWakeupPolicy::DefaultWakeup
                && state.is_consumer_position(header as *const RingBufferRecordHeader as usize))
        {
            self.vmo
                .as_handle_ref()
                .signal(zx::Signals::empty(), RINGBUF_SIGNAL)
                .expect("Failed to set signal or a ring buffer VMO");
        }
    }

    /// Submit the data.
    ///
    /// # Safety
    ///
    /// `addr` must be the value returned by a previous call to `ringbuf_reserve`
    /// on a map that has not been dropped, otherwise the behaviour is UB.
    pub unsafe fn submit(addr: u64, flags: RingBufferWakeupPolicy) {
        let addr = addr as usize;
        let (ringbuf_storage, header) = Self::get_ringbug_and_header_by_addr(addr);
        ringbuf_storage.commit(header, flags, false);
    }

    /// Discard the data.
    ///
    /// # Safety
    ///
    /// `addr` must be the value returned by a previous call to `ringbuf_reserve`
    /// on a map that has not been dropped, otherwise the behaviour is UB.
    pub unsafe fn discard(addr: u64, flags: RingBufferWakeupPolicy) {
        let addr = addr as usize;
        let (ringbuf_storage, header) = Self::get_ringbug_and_header_by_addr(addr);
        ringbuf_storage.commit(header, flags, true);
    }

    /// Get the `RingBufferImpl` and the `RingBufferRecordHeader` associated with `addr`.
    ///
    /// # Safety
    ///
    /// `addr` must be the value returned from a previous call to `ringbuf_reserve` on a `Map` that
    /// has not been dropped and is kept alive as long as the returned value are used.
    unsafe fn get_ringbug_and_header_by_addr(
        addr: usize,
    ) -> (&'static RingBuffer, &'static RingBufferRecordHeader) {
        let page_size = *PAGE_SIZE;
        // addr is the data section. First access the header.
        let header = &*((addr - std::mem::size_of::<RingBufferRecordHeader>())
            as *const RingBufferRecordHeader);
        let addr_page = addr / page_size;
        let mapping_start_page = addr_page - header.page_count as usize;
        let mapping_start_address = mapping_start_page * page_size;
        let ringbuf_impl = &*(mapping_start_address as *const &RingBuffer);
        (ringbuf_impl, header)
    }
}

impl MapImpl for RingBuffer {
    fn get_raw(&self, _key: &[u8]) -> Option<*mut u8> {
        None
    }

    fn lookup(&self, _key: &[u8]) -> Option<Vec<u8>> {
        None
    }

    fn update(&self, _key: MapKey, _value: &[u8], _flags: u64) -> Result<(), MapError> {
        Err(MapError::InvalidParam)
    }

    fn delete(&self, _key: &[u8]) -> Result<(), MapError> {
        Err(MapError::InvalidParam)
    }

    fn get_next_key(&self, _key: Option<&[u8]>) -> Result<MapKey, MapError> {
        Err(MapError::InvalidParam)
    }

    fn vmo(&self) -> Option<Arc<zx::Vmo>> {
        Some(self.vmo.clone())
    }

    fn can_read(&self) -> Option<bool> {
        let mut state = self.state.lock();
        let consumer_position = state.consumer_position.load(Ordering::Acquire);
        let producer_position = state.producer_position.load(Ordering::Acquire);

        // Read the header at the consumer position, and check that the entry is not busy.
        let can_read = consumer_position < producer_position
            && ((*state.header_mut(producer_position).length.get_mut()) & BPF_RINGBUF_BUSY_BIT
                == 0);
        Some(can_read)
    }

    fn ringbuf_reserve(&self, size: u32, flags: u64) -> Result<usize, MapError> {
        if flags != 0 {
            return Err(MapError::InvalidParam);
        }

        //  The top two bits are used as special flags.
        if size & (BPF_RINGBUF_BUSY_BIT | BPF_RINGBUF_DISCARD_BIT) > 0 {
            return Err(MapError::InvalidParam);
        }

        let mut state = self.state.lock();
        let consumer_position = state.consumer_position.load(Ordering::Acquire);
        let producer_position = state.producer_position.load(Ordering::Acquire);
        let max_size = state.mask + 1;

        // Available size on the ringbuffer.
        let consumed_size =
            producer_position.checked_sub(consumer_position).ok_or(MapError::InvalidParam)?;
        let available_size = max_size.checked_sub(consumed_size).ok_or(MapError::InvalidParam)?;

        const HEADER_ALIGNMENT: u32 = std::mem::size_of::<u64>() as u32;

        // Total size of the message to write. This is the requested size + the header, rounded up
        // to align the next header.
        let total_size: u32 = (size + BPF_RINGBUF_HDR_SZ + HEADER_ALIGNMENT - 1) / HEADER_ALIGNMENT
            * HEADER_ALIGNMENT;

        if total_size > available_size {
            return Err(MapError::SizeLimit);
        }
        let data_position = state.data_position(producer_position + BPF_RINGBUF_HDR_SZ);
        let data_length = size | BPF_RINGBUF_BUSY_BIT;
        let page_count = ((data_position - state.data) / *PAGE_SIZE + 3)
            .try_into()
            .map_err(|_| MapError::SizeLimit)?;
        let header = state.header_mut(producer_position);
        *header.length.get_mut() = data_length;
        header.page_count = page_count;
        state.producer_position.store(producer_position + total_size, Ordering::Release);
        Ok(data_position)
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingBufferWakeupPolicy {
    DefaultWakeup = 0,
    NoWakeup = BPF_RB_NO_WAKEUP,
    ForceWakeup = BPF_RB_FORCE_WAKEUP,
}

impl From<u32> for RingBufferWakeupPolicy {
    fn from(v: u32) -> Self {
        match v {
            BPF_RB_NO_WAKEUP => Self::NoWakeup,
            BPF_RB_FORCE_WAKEUP => Self::ForceWakeup,
            // If flags is invalid, use the default value. This is necessary to prevent userspace
            // leaking ringbuf value by calling into the kernel with an incorrect flag value.
            _ => Self::DefaultWakeup,
        }
    }
}

#[repr(C)]
#[repr(align(8))]
#[derive(Debug)]
struct RingBufferRecordHeader {
    length: AtomicU32,
    page_count: u32,
}

const_assert!(std::mem::size_of::<RingBufferRecordHeader>() == BPF_RINGBUF_HDR_SZ as usize);

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia::test]
    fn test_ring_buffer_wakeup_policy() {
        assert_eq!(RingBufferWakeupPolicy::from(0), RingBufferWakeupPolicy::DefaultWakeup);
        assert_eq!(
            RingBufferWakeupPolicy::from(BPF_RB_NO_WAKEUP),
            RingBufferWakeupPolicy::NoWakeup
        );
        assert_eq!(
            RingBufferWakeupPolicy::from(BPF_RB_FORCE_WAKEUP),
            RingBufferWakeupPolicy::ForceWakeup
        );
        assert_eq!(RingBufferWakeupPolicy::from(42), RingBufferWakeupPolicy::DefaultWakeup);
    }
}
