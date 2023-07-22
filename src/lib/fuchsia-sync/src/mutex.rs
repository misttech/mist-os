// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::sys;

extern "C" {
    fn sync_mutex_lock(lock: *const sys::zx_futex_t);
    fn sync_mutex_trylock(lock: *const sys::zx_futex_t) -> sys::zx_status_t;
    fn sync_mutex_unlock(lock: *const sys::zx_futex_t);
}

// See SYNC_MUTEX_INIT in lib/sync/mutex.h
const SYNC_MUTEX_INIT: i32 = 0;

#[repr(transparent)]
pub struct RawSyncMutex(sys::zx_futex_t);

impl RawSyncMutex {
    fn as_futex_ptr(&self) -> *const sys::zx_futex_t {
        std::ptr::addr_of!(self.0)
    }
}

// SAFETY: This trait requires that "[i]mplementations of this trait must ensure
// that the mutex is actually exclusive: a lock can't be acquired while the mutex
// is already locked." This guarantee is provided by libsync's APIs.
unsafe impl lock_api::RawMutex for RawSyncMutex {
    const INIT: RawSyncMutex = RawSyncMutex(sys::zx_futex_t::new(SYNC_MUTEX_INIT));

    // libsync does not require the lock / unlock operations to happen on the same thread.
    type GuardMarker = lock_api::GuardSend;

    fn lock(&self) {
        // SAFETY: This call requires we pass a non-null pointer to a valid futex.
        // This is guaranteed by using `self` through a shared reference.
        unsafe {
            sync_mutex_lock(self.as_futex_ptr());
        }
    }

    fn try_lock(&self) -> bool {
        // SAFETY: This call requires we pass a non-null pointer to a valid futex.
        // This is guaranteed by using `self` through a shared reference.
        unsafe { sync_mutex_trylock(self.as_futex_ptr()) == sys::ZX_OK }
    }

    unsafe fn unlock(&self) {
        sync_mutex_unlock(self.as_futex_ptr())
    }
}

pub type Mutex<T> = lock_api::Mutex<RawSyncMutex, T>;
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawSyncMutex, T>;
pub type MappedMutexGuard<'a, T> = lock_api::MappedMutexGuard<'a, RawSyncMutex, T>;

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_lock_and_unlock() {
        let value = Mutex::<u32>::new(5);
        let mut guard = value.lock();
        assert_eq!(*guard, 5);
        *guard = 6;
        assert_eq!(*guard, 6);
        std::mem::drop(guard);
    }

    #[test]
    fn test_try_lock() {
        let value = Mutex::<u32>::new(5);
        let _guard = value.lock();
        assert!(value.try_lock().is_none());
    }
}
