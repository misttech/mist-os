// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fuchsia_zircon::{self as zx, HandleBased},
    std::{
        cell::UnsafeCell,
        collections::{hash_map::Entry, HashMap},
        marker::PhantomData,
        mem::ManuallyDrop,
        ops::Deref,
        sync::{Arc, Mutex, OnceLock, Weak},
    },
};

/// A wrapper around zircon handles that allows them to be temporarily cloned. These temporary
/// clones can be used with `fuchsia_async::unblock` which requires callbacks with static lifetime.
/// This is similar to Arc<T>, except that whilst there are no clones, there is no memory overhead,
/// and there's no performance overhead to use them just as you would without the wrapper, except
/// for a small overhead when they are dropped. The wrapper ensures that the handle is only dropped
/// when there are no references.
pub struct TempClonable<T: HandleBased>(ManuallyDrop<T>);

impl<T: HandleBased> TempClonable<T> {
    /// Returns a new handle that can be temporarily cloned.
    pub fn new(handle: T) -> Self {
        Self(ManuallyDrop::new(handle))
    }
}

impl<T: HandleBased> Deref for TempClonable<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: HandleBased> TempClonable<T> {
    /// Creates a temporary clone of the handle. The clone should only exist temporarily.
    ///
    /// # Panics
    ///
    /// Panics if the handle is invalid.
    pub fn temp_clone(&self) -> TempClone<T> {
        assert!(!self.is_invalid_handle());
        let mut clones = clones().lock().unwrap();
        let raw_handle = self.0.raw_handle();
        TempClone {
            handle: match clones.entry(raw_handle) {
                Entry::Occupied(mut o) => {
                    if let Some(clone) = o.get().upgrade() {
                        clone
                    } else {
                        // The last strong reference was dropped but the entry hasn't been removed
                        // yet. This must be racing with `TempHandle::drop`. Replace the
                        // `TempHandle`.
                        let clone =
                            Arc::new(TempHandle { raw_handle, tombstone: UnsafeCell::new(false) });
                        *o.get_mut() = Arc::downgrade(&clone);
                        clone
                    }
                }
                Entry::Vacant(v) => {
                    let clone =
                        Arc::new(TempHandle { raw_handle, tombstone: UnsafeCell::new(false) });
                    v.insert(Arc::downgrade(&clone));
                    clone
                }
            },
            marker: PhantomData,
        }
    }
}

impl<T: HandleBased> Drop for TempClonable<T> {
    fn drop(&mut self) {
        if let Some(handle) =
            clones().lock().unwrap().remove(&self.0.raw_handle()).and_then(|c| c.upgrade())
        {
            // There are still some temporary clones alive, so mark the handle with a tombstone.

            // SAFETY: This is the only unsafe place where we access `tombstone`. We're are holding
            // the clones lock which ensures no other thread is concurrently accessing it, but it
            // wouldn't normally happen anyway because it would mean there were multiple
            // TempClonable instances wrapping the same handle, which shouldn't happen.
            unsafe { *handle.tombstone.get() = true };
            return;
        }

        // SAFETY: There are no temporary clones, so we can drop the handle now. No more clones can
        // be made and it should be clear we meet the safety requirements of ManuallyDrop.
        unsafe { ManuallyDrop::drop(&mut self.0) }
    }
}

type Clones = Mutex<HashMap<zx::sys::zx_handle_t, Weak<TempHandle>>>;

/// Returns the global instance which keeps track of temporary clones.
fn clones() -> &'static Clones {
    static CLONES: OnceLock<Clones> = OnceLock::new();
    CLONES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct TempClone<T> {
    handle: Arc<TempHandle>,
    marker: PhantomData<T>,
}

impl<T> Deref for TempClone<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: T is repr(transparent) and stores zx::sys::zx_handle_t.
        unsafe { std::mem::transmute::<&zx::sys::zx_handle_t, &T>(&self.handle.raw_handle) }
    }
}

struct TempHandle {
    raw_handle: zx::sys::zx_handle_t,
    tombstone: UnsafeCell<bool>,
}

unsafe impl Send for TempHandle {}
unsafe impl Sync for TempHandle {}

impl Drop for TempHandle {
    fn drop(&mut self) {
        if *self.tombstone.get_mut() {
            // SAFETY: The primary handle has been dropped and it is our job to clean up the
            // handle. There are no memory safety issues here.
            unsafe { zx::Handle::from_raw(self.raw_handle) };
        } else {
            if let Entry::Occupied(o) = clones().lock().unwrap().entry(self.raw_handle) {
                // There's a small window where another TempHandle could have been inserted, so
                // before removing this entry, check for a match.
                if std::ptr::eq(o.get().as_ptr(), self) {
                    o.remove_entry();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{clones, TempClonable},
        fuchsia_zircon as zx,
        std::sync::Arc,
    };

    #[test]
    fn test_temp_clone() {
        let parent_vmo = zx::Vmo::create(100).expect("create failed");

        {
            let temp_clone = {
                let vmo = TempClonable::new(
                    parent_vmo
                        .create_child(zx::VmoChildOptions::REFERENCE, 0, 0)
                        .expect("create_child failed"),
                );

                vmo.write(b"foo", 0).expect("write failed");
                {
                    // Create and read from a temporary clone.
                    let temp_clone2 = vmo.temp_clone();
                    assert_eq!(&temp_clone2.read_to_vec(0, 3).expect("read_to_vec failed"), b"foo");
                }

                // We should still be able to read from the primary handle.
                assert_eq!(&vmo.read_to_vec(0, 3).expect("read_to_vec failed"), b"foo");

                // Create another vmo which should get cleaned up when the primary handle is
                // dropped.
                let vmo2 = TempClonable::new(
                    parent_vmo
                        .create_child(zx::VmoChildOptions::REFERENCE, 0, 0)
                        .expect("create_child failed"),
                );
                // Create and immediately drop a temporary clone.
                vmo2.temp_clone();

                // Take another clone that will get dropped after we take the clone below.
                let _clone1 = vmo.temp_clone();

                // And return another clone.
                vmo.temp_clone()
            };

            // The primary handle has been dropped, but we should still be able to
            // read via temp_clone.
            assert_eq!(&temp_clone.read_to_vec(0, 3).expect("read_to_vec failed"), b"foo");
        }

        // Make sure that all the VMOs got properly cleaned up.
        assert_eq!(parent_vmo.info().expect("info failed").num_children, 0);
        assert!(clones().lock().unwrap().is_empty());
    }

    #[test]
    fn test_race() {
        let parent_vmo = zx::Vmo::create(100).expect("create failed");

        {
            let vmo = Arc::new(TempClonable::new(
                parent_vmo
                    .create_child(zx::VmoChildOptions::REFERENCE, 0, 0)
                    .expect("create_child failed"),
            ));
            vmo.write(b"foo", 0).expect("write failed");

            let vmo_clone = vmo.clone();

            let t1 = std::thread::spawn(move || {
                for _ in 0..1000 {
                    assert_eq!(
                        &vmo.temp_clone().read_to_vec(0, 3).expect("read_to_vec failed"),
                        b"foo"
                    );
                }
            });

            let t2 = std::thread::spawn(move || {
                for _ in 0..1000 {
                    assert_eq!(
                        &vmo_clone.temp_clone().read_to_vec(0, 3).expect("read_to_vec failed"),
                        b"foo"
                    );
                }
            });

            let _ = t1.join();
            let _ = t2.join();
        }

        // Make sure that all the VMOs got properly cleaned up.
        assert_eq!(parent_vmo.info().expect("info failed").num_children, 0);
        assert!(clones().lock().unwrap().is_empty());
    }
}
