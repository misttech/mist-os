// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Safe bindings for the driver runtime arena stable ABI

use core::alloc::Layout;
use core::cmp::max;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr::{null_mut, slice_from_raw_parts_mut, NonNull};

use fuchsia_zircon::Status;

use crate::fdf_sys::*;

/// Implements a memory arena allocator to be used with the Fuchsia Driver
/// Runtime when sending and receiving from channels.
#[derive(Debug)]
pub struct Arena(pub(crate) NonNull<fdf_arena_t>);

// SAFETY: The api for `fdf_arena_t` is thread safe
unsafe impl Send for Arena {}
unsafe impl Sync for Arena {}

impl Arena {
    /// Allocates a new arena for use with the driver runtime
    pub fn new() -> Result<Self, Status> {
        let mut arena = null_mut();
        // SAFETY: the address we pass to fdf_arena_create is allocated on
        // the stack and appropriately sized.
        let res = unsafe { fdf_arena_create(0, 0, &mut arena) };
        if res == ZX_OK {
            // SAFETY: if fdf_arena_create returned ZX_OK, it will have placed
            // a non-null pointer.
            Ok(Arena(unsafe { NonNull::new_unchecked(arena) }))
        } else {
            Err(Status::from_raw(res))
        }
    }

    /// Allocates the appropriate amount of memory for the given layout and
    /// returns a pointer to `T` at the start of that memory.
    ///
    /// # Safety
    ///
    /// The caller is responsible for making sure that the `Layout` is laid out
    /// properly for one or more `T` to be stored at it. This may be a single
    /// object or a slice of them, but it must be a multiple of it.
    unsafe fn alloc_bytes_for<T>(&self, layout: Layout) -> NonNull<T> {
        // We make sure we allocate at least one byte so we return a unique
        // pointer that is within the arena, which will ensure that subsequent
        // verifications that the memory location is in the arena will pass.
        let bytes = max(layout.size(), 1);
        // SAFETY: Allocating a block of memory in the arena large enough to store
        // the object we're allocating.
        let storage =
            unsafe { NonNull::new_unchecked(fdf_arena_allocate(self.0.as_ptr(), bytes) as *mut T) };
        // TODO(b/352119228): when the arena allocator allows specifying alignment, use that
        // instead of asserting the alignment after the fact.
        assert_eq!(
            storage.align_offset(layout.align()),
            0,
            "Arena returned an improperly aligned pointer: {}",
            core::any::type_name::<T>(),
        );
        storage
    }

    /// Moves `obj` of type `T` into the arena and returns an [`ArenaBox`]
    /// containing the moved value.
    pub fn insert<T: Sized>(&self, obj: T) -> ArenaBox<'_, T> {
        // SAFETY: The layout we give to `alloc_bytes_for` is for storing one
        // object of type `T`, which is the pointer type we get from it.
        let storage = unsafe { self.alloc_bytes_for(Layout::for_value(&obj)) };
        // SAFETY: moves the object into the arena memory we
        // just allocated, without any additional drops.
        unsafe { core::ptr::write(storage.as_ptr(), obj) };
        // SAFETY: alloc_bytes_for is expected to return a valid pointer.
        ArenaBox::new(unsafe { NonNull::new_unchecked(storage.as_ptr()) })
    }

    /// Moves a [`Box`]ed slice into the arena and returns an [`ArenaBox`]
    /// containing the moved value.
    pub fn insert_boxed_slice<T: Sized>(&self, slice: Box<[T]>) -> ArenaBox<'_, [T]> {
        let layout = Layout::for_value(&*slice);
        let len = slice.len();
        // SAFETY: The layout we give `alloc_bytes_for` is for storing 0 or more
        // objects of type `T`, which is the pointer type we get from it.
        let storage = unsafe { self.alloc_bytes_for(layout) };
        let original_storage = Box::into_raw(slice);
        // SAFETY: Moving the object into the arena memory we just allocated by
        // first copying the bytes over and then deallocating the raw memory
        // we took from the box.
        let slice_ptr = unsafe {
            core::ptr::copy_nonoverlapping(original_storage as *mut T, storage.as_ptr(), len);
            NonNull::new_unchecked(slice_from_raw_parts_mut(storage.as_ptr(), len))
        };
        if layout.size() != 0 {
            // SAFETY: Since we have decomposed the Box we have to deallocate it,
            // but only if it's not dangling.
            unsafe {
                std::alloc::dealloc(original_storage as *mut u8, layout);
            }
        }
        ArenaBox::new(slice_ptr)
    }

    /// Copies the slice into the arena and returns an [`ArenaBox`] containing
    /// the copied values.
    pub fn insert_slice<T: Sized + Clone>(&self, slice: &[T]) -> ArenaBox<'_, [T]> {
        let len = slice.len();
        // SAFETY: The layout we're passing to `alloc_bytes_for` is for zero or
        // more objects of type `T`, which is the pointer type we get back from
        // it.
        let storage = unsafe { self.alloc_bytes_for(Layout::for_value(slice)) };
        let mut storage_pos = storage;
        for item in slice {
            // SAFETY: for each object in the original slice we clone it and
            // write the clone into the new slice's backing memory. Since
            // core::ptr::write 'forgets' the item we give it, this is
            // effectively a move.
            unsafe { core::ptr::write(storage_pos.as_ptr(), item.clone()) };
            // SAFETY: Since we allocated enough storage for all of the
            // elements, and we are iterating based on the original slice,
            // we should never dereference beyond the end of the new storage.
            storage_pos = unsafe { storage_pos.add(1) };
        }
        // At this point we have a `*mut T` but we need to return a `[T]`,
        // which is unsized. We need to use [`slice_from_raw_parts_mut`]
        // to construct the unsized pointer from the data and its length.
        let ptr = slice_from_raw_parts_mut(storage.as_ptr(), len);
        // SAFETY: alloc_bytes_for is expected to return a valid pointer.
        ArenaBox::new(unsafe { NonNull::new_unchecked(ptr) })
    }

    /// Returns an ArenaBox for the pointed to object, assuming that it is part
    /// of this arena.
    ///
    /// # Safety
    ///
    /// This does not verify that the pointer came from this arena and that it's
    /// not null, so the caller is responsible for verifying that.
    pub unsafe fn assume_unchecked<T: ?Sized>(&self, ptr: *mut T) -> ArenaBox<'_, T> {
        // SAFETY: Caller is responsible for ensuring this per safety doc section.
        ArenaBox::new(unsafe { NonNull::new_unchecked(ptr) })
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        // SAFETY: We own this arena reference and so we can drop it.
        unsafe { fdf_arena_drop_ref(self.0.as_ptr()) }
    }
}

/// Holds a reference to data of type `T` in an [`Arena`] with lifetime `'a`,
/// and ensures that the object is properly dropped before the [`Arena`] goes
/// out of scope.
pub struct ArenaBox<'a, T: ?Sized>(NonNull<T>, PhantomData<&'a Arena>);

impl<'a, T> ArenaBox<'a, T> {
    /// Moves the inner value of this ArenaBox out to owned storage.
    pub fn take(value: Self) -> T {
        // SAFETY: `Self::into_ptr` will forget `value` and prevent
        // calling its `drop`.
        unsafe { core::ptr::read(Self::into_ptr(value)) }
    }

    /// Moves the inner value of this ArenaBox out into a [`Box`] using the
    /// global allocator. Using this instead of `Box::new(ArenaBox::take(v))`
    /// helps to avoid any additional copies of the storage on its way to the
    /// box.
    ///
    /// Note: if you want to take a slice, you will need to use
    /// [`Self::take_boxed_slice`].
    pub fn take_boxed(value: Self) -> Box<T> {
        // SAFETY: we are allocating space for `T` with the layout of `T`, so
        // this is simple.
        let storage = unsafe { global_alloc(Layout::for_value(&*value)) };
        // SAFETY: storage is sufficiently large to store the value in `value`
        // and we used Layout to make sure that Box will be happy with its
        // layout.
        unsafe {
            core::ptr::write(storage.as_ptr(), Self::take(value));
            Box::from_raw(storage.as_ptr())
        }
    }
}

impl<'a, T> ArenaBox<'a, [T]> {
    /// Like [`Self::take_boxed`], this moves the inner value of this ArenaBox
    /// out into a [`Box`] using the global allocator, and using it avoids
    /// additional copies of the data, but this function works on slices of `T`,
    /// which are unsized and so require special handling.
    pub fn take_boxed_slice(value: Self) -> Box<[T]> {
        let len = value.len();
        // SAFETY: we are using the layout of the slice value of type `[T]` to
        // allocate a pointer to the first element of the storage for the new
        // slice, which is of type `T`.
        let storage = unsafe { global_alloc(Layout::for_value(&*value)) };
        // SAFETY: storage is sufficiently large to store the slice in `value`
        let slice_ptr = unsafe {
            core::ptr::copy_nonoverlapping(Self::into_ptr(value) as *mut T, storage.as_ptr(), len);
            core::ptr::slice_from_raw_parts_mut(storage.as_ptr(), len)
        };
        // SAFETY: we used Layout to make sure that Box will be happy with the
        // layout of the stored value.
        unsafe { Box::from_raw(slice_ptr) }
    }
}

impl<'a, T: ?Sized> ArenaBox<'a, T> {
    fn new(obj: NonNull<T>) -> ArenaBox<'a, T> {
        Self(obj, PhantomData)
    }

    /// Decomposes this arenabox into its pointer.
    ///
    /// # Safety
    ///
    /// This is unsafe because it loses the lifetime of the Arena it
    /// came from. The caller must make sure to not let the pointer outlive the
    /// arena. The caller is also responsible for making sure the object is
    /// dropped before the `Arena`, or it may leak resources.
    pub(crate) unsafe fn into_ptr(value: Self) -> *mut T {
        let res = value.0.as_ptr();
        core::mem::forget(value);
        res
    }
}

impl<'a, T: ?Sized> Drop for ArenaBox<'a, T> {
    fn drop(&mut self) {
        // SAFETY: Since this value is allocated in the arena, and the arena
        // will not drop the value, and ArenaBox can't be cloned, this ArenaBox
        // owns the value and can drop it.
        unsafe { core::ptr::drop_in_place(self.0.as_ptr()) }
    }
}

impl<T: ?Sized> Deref for ArenaBox<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        // SAFETY: As these methods are the only way to get a reference to the
        // contents of the ArenaBox, rust will enforce the aliasing rules
        // of the contents of the inner `NonZero` object.
        unsafe { self.0.as_ref() }
    }
}

impl<T: ?Sized> DerefMut for ArenaBox<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: As these methods are the only way to get a reference to the
        // contents of the ArenaBox, rust will enforce the aliasing rules
        // of the contents of the inner `NonZero` object.
        unsafe { self.0.as_mut() }
    }
}

/// Helper for allocating storage on the global heap appropriate for storing a
/// copy of `val`. This returns a pointer of a different type than the type
/// being referenced so that it can be used to allocate storage for unsized
/// slices of `T`. `ActualType` should either be the same as `T` or an array of
/// `T`.
///
/// This also correctly handles a zero sized type by returning
/// [`NonZero::dangling`].
///
/// # Safety
///
/// In addition to all the safety requirements of [`std::alloc::alloc`], the
/// caller must ensure that `T` is the type of elements of `ActualType`.
unsafe fn global_alloc<T>(layout: Layout) -> NonNull<T> {
    let storage = if layout.size() == 0 {
        NonNull::dangling()
    } else {
        let ptr = unsafe { std::alloc::alloc(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        unsafe { NonNull::new_unchecked(ptr as *mut T) }
    };
    storage
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::mpsc;

    use super::*;

    /// Implements a cloneable object that will send only one message
    /// on an [`mpsc::Sender`] when its 'last' clone is dropped. It will assert
    /// if an attempt to re-clone an already cloned [`DropSender`] happens,
    /// ensuring that the object is only cloned in a linear path.
    struct DropSender<T: Clone>(T, Cell<Option<mpsc::Sender<T>>>);
    impl<T: Clone> DropSender<T> {
        fn new(val: T, sender: mpsc::Sender<T>) -> Self {
            Self(val, Cell::new(Some(sender)))
        }
    }
    impl<T: Clone> Drop for DropSender<T> {
        fn drop(&mut self) {
            match self.1.get_mut() {
                Some(sender) => {
                    println!("dropping a drop sender");
                    sender.send(self.0.clone()).unwrap();
                }
                _ => {}
            }
        }
    }
    impl<T: Clone> Clone for DropSender<T> {
        fn clone(&self) -> Self {
            Self(
                self.0.clone(),
                Cell::new(Some(self.1.take().expect("Attempted to re-clone a `DropSender`"))),
            )
        }
    }

    #[test]
    fn arena_allocations() {
        let arena = Arena::new().unwrap();
        let _val = arena.insert(());
        let val = arena.insert(1);
        assert_eq!(*val, 1);
        let val = arena.insert(2);
        assert_eq!(*val, 2);
        let val = arena.insert_boxed_slice(Box::new([1, 2, 3, 4]));
        assert_eq!(&*val, &[1, 2, 3, 4]);
        let val: ArenaBox<'_, [()]> = arena.insert_boxed_slice(Box::new([]));
        assert_eq!(&*val, &[]);
        let val = arena.insert_slice(&[5, 6, 7, 8]);
        assert_eq!(&*val, &[5, 6, 7, 8]);
        let val: ArenaBox<'_, [()]> = arena.insert_slice(&[]);
        assert_eq!(&*val, &[]);
    }

    #[test]
    #[allow(clippy::unit_cmp)]
    fn arena_take() {
        let arena = Arena::new().unwrap();
        let val = arena.insert(());
        assert_eq!(ArenaBox::take(val), ());
        let val = arena.insert(1);
        assert_eq!(ArenaBox::take(val), 1);
    }

    #[test]
    #[allow(clippy::unit_cmp)]
    fn arena_take_boxed() {
        let arena = Arena::new().unwrap();
        let val = arena.insert(());
        assert_eq!(*ArenaBox::take_boxed(val), ());
        let val = arena.insert(1);
        assert_eq!(*ArenaBox::take_boxed(val), 1);
    }

    #[test]
    fn arena_take_boxed_slice() {
        let arena = Arena::new().unwrap();
        let val: ArenaBox<'_, [()]> = arena.insert_slice(&[]);
        assert_eq!(&*ArenaBox::take_boxed_slice(val), &[]);
        let val = arena.insert_slice(&[1, 2, 3, 4]);
        assert_eq!(&*ArenaBox::take_boxed_slice(val), &[1, 2, 3, 4]);
    }

    #[test]
    fn arena_drop() {
        let (tx, rx) = mpsc::channel();
        let arena = Arena::new().unwrap();
        let val = arena.insert(DropSender::new(1, tx.clone()));
        drop(val);
        assert_eq!(rx.try_recv().unwrap(), 1);

        let val = arena.insert_boxed_slice(Box::new([DropSender::new(2, tx.clone())]));
        drop(val);
        assert_eq!(rx.try_recv().unwrap(), 2);

        let val = arena.insert_slice(&[DropSender::new(3, tx.clone())]);
        drop(val);
        assert_eq!(rx.try_recv().unwrap(), 3);

        rx.try_recv().expect_err("no more drops");
    }

    #[test]
    fn arena_take_drop() {
        let (tx, rx) = mpsc::channel();
        let arena = Arena::new().unwrap();

        let val = arena.insert(DropSender::new(1, tx.clone()));
        let inner = ArenaBox::take(val);
        rx.try_recv().expect_err("shouldn't have dropped when taken");
        drop(inner);
        assert_eq!(rx.try_recv().unwrap(), 1);

        let val = arena.insert_slice(&[DropSender::new(2, tx.clone())]);
        let inner = ArenaBox::take_boxed_slice(val);
        rx.try_recv().expect_err("shouldn't have dropped when taken");
        drop(inner);
        assert_eq!(rx.try_recv().unwrap(), 2);

        rx.try_recv().expect_err("no more drops");
    }
}
