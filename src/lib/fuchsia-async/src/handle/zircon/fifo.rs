// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::rwhandle::{RWHandle, ReadableHandle as _, WritableHandle as _};
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::ready;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::pin::Pin;
use std::task::{Context, Poll};
use zerocopy::{FromBytes, Immutable, IntoBytes};

/// Marker trait for types that can be read/written with a `Fifo`.
///
/// An implementation is provided for all types that implement
/// [`IntoBytes`], [`FromBytes`], and [`Immutable`].
pub trait FifoEntry: IntoBytes + FromBytes + Immutable {}

impl<O: IntoBytes + FromBytes + Immutable> FifoEntry for O {}

/// A buffer used to write `T` into [`Fifo`] objects.
///
///
/// # Safety
///
/// This trait is unsafe because the compiler cannot verify a correct
/// implementation of `as_bytes_ptr`. See [`FifoWriteBuffer::as_bytes_ptr`] for
/// safety notes.
pub unsafe trait FifoWriteBuffer<T> {
    /// Returns the number of entries to be written.
    fn count(&self) -> usize;
    /// Returns a byte pointer representation to be written into the underlying
    /// FIFO.
    ///
    /// # Safety
    ///
    /// The returned memory *must* be initialized and at least `count() *
    /// sizeof<T>()` bytes long.
    fn as_ptr(&self) -> *const T;
}

/// A buffer used to read `T` from [`Fifo`] objects.
///
/// # Safety
///
/// This trait is unsafe because the compiler cannot verify a correct
/// implementation of `as_bytes_ptr_mut`. See
/// [`FifoReadBuffer::as_bytes_ptr_mut`] for safety notes.
pub unsafe trait FifoReadBuffer<T> {
    /// Returns the number of slots available in the buffer to be rceived.
    fn count(&self) -> usize;
    /// Returns a mutable pointer to the buffer contents where FIFO entries must
    /// be written into.
    ///
    /// # Safety
    ///
    /// The returned memory *must* be at least `count() * sizeof<T>()` bytes
    /// long.
    fn as_mut_ptr(&mut self) -> *mut T;
}

unsafe impl<T: FifoEntry> FifoWriteBuffer<T> for [T] {
    fn count(&self) -> usize {
        self.len()
    }

    fn as_ptr(&self) -> *const T {
        self.as_ptr()
    }
}

unsafe impl<T: FifoEntry> FifoReadBuffer<T> for [T] {
    fn count(&self) -> usize {
        self.len()
    }

    fn as_mut_ptr(&mut self) -> *mut T {
        self.as_mut_ptr()
    }
}

unsafe impl<T: FifoEntry> FifoWriteBuffer<T> for T {
    fn count(&self) -> usize {
        1
    }

    fn as_ptr(&self) -> *const T {
        self as *const T
    }
}

unsafe impl<T: FifoEntry> FifoReadBuffer<T> for T {
    fn count(&self) -> usize {
        1
    }

    fn as_mut_ptr(&mut self) -> *mut T {
        self as *mut T
    }
}

unsafe impl<T: FifoEntry> FifoReadBuffer<T> for MaybeUninit<T> {
    fn count(&self) -> usize {
        1
    }

    fn as_mut_ptr(&mut self) -> *mut T {
        self.as_mut_ptr()
    }
}

unsafe impl<T: FifoEntry> FifoReadBuffer<T> for [MaybeUninit<T>] {
    fn count(&self) -> usize {
        self.len()
    }

    fn as_mut_ptr(&mut self) -> *mut T {
        // TODO(https://github.com/rust-lang/rust/issues/63569): Use
        // `MaybeUninit::slice_as_mut_ptr` once stable.
        self.as_mut_ptr() as *mut T
    }
}

/// A helper struct providing an implementation of [`FifoWriteBuffer`]
/// supporting [`WriteEntries`] to be able to write all entries in a buffer
/// instead of providing only partial writes.
struct OffsetWriteBuffer<'a, B: ?Sized, T> {
    buffer: &'a B,
    offset: usize,
    marker: PhantomData<T>,
}

impl<'a, B: ?Sized + FifoWriteBuffer<T>, T: FifoEntry> OffsetWriteBuffer<'a, B, T> {
    fn new(buffer: &'a B) -> Self {
        Self { buffer, offset: 0, marker: PhantomData }
    }

    fn advance(mut self, len: usize) -> Option<Self> {
        self.offset += len;
        if self.offset == self.buffer.count() {
            None
        } else {
            debug_assert!(self.offset < self.buffer.count());
            Some(self)
        }
    }
}

unsafe impl<'a, T: FifoEntry, B: ?Sized + FifoWriteBuffer<T>> FifoWriteBuffer<T>
    for OffsetWriteBuffer<'a, B, T>
{
    fn count(&self) -> usize {
        debug_assert!(self.offset <= self.buffer.count());
        self.buffer.count() - self.offset
    }

    fn as_ptr(&self) -> *const T {
        debug_assert!(self.offset <= self.buffer.count());
        // SAFETY: Protected by the debug assertion above and a correct
        // implementation of `FifoWriteBuffer` by `B`.
        unsafe { self.buffer.as_ptr().add(self.offset) }
    }
}

/// Identifies that the object may be used to write entries into a FIFO.
pub trait FifoWritable<W: FifoEntry>
where
    Self: Sized,
{
    /// Creates a future that transmits entries to be written.
    ///
    /// The returned future will return after an entry has been received on this
    /// fifo. The future will resolve to the fifo once all elements have been
    /// transmitted.
    fn write_entries<'a, B: ?Sized + FifoWriteBuffer<W>>(
        &'a self,
        entries: &'a B,
    ) -> WriteEntries<'a, Self, B, W> {
        WriteEntries::new(self, entries)
    }

    /// Writes entries to the fifo and registers this `Fifo` as needing a write
    /// on receiving a `zx::Status::SHOULD_WAIT`.
    ///
    /// Returns the number of elements processed.
    fn write<B: ?Sized + FifoWriteBuffer<W>>(
        &self,
        cx: &mut Context<'_>,
        entries: &B,
    ) -> Poll<Result<usize, zx::Status>>;
}

/// Identifies that the object may be used to read entries from a FIFO.
pub trait FifoReadable<R: FifoEntry>
where
    Self: Sized,
{
    /// Creates a future that receives entries into `entries`.
    ///
    /// The returned future will return after the FIFO becomes readable and up
    /// to `entries.len()` has been received. The future will resolve to the
    /// number of elements written into `entries`.
    ///
    fn read_entries<'a, B: ?Sized + FifoReadBuffer<R>>(
        &'a self,
        entries: &'a mut B,
    ) -> ReadEntries<'a, Self, B, R> {
        ReadEntries::new(self, entries)
    }

    /// Creates a future that receives a single entry.
    ///
    /// The returned future will return after the FIFO becomes readable and a
    /// single entry is available.
    fn read_entry<'a>(&'a self) -> ReadOne<'a, Self, R> {
        ReadOne::new(self)
    }

    /// Reads entries from the fifo and registers this `Fifo` as needing a read
    /// on receiving a `zx::Status::SHOULD_WAIT`.
    fn read<B: ?Sized + FifoReadBuffer<R>>(
        &self,
        cx: &mut Context<'_>,
        entries: &mut B,
    ) -> Poll<Result<usize, zx::Status>>;

    /// Reads a single entry and registers this `Fifo` as needing a read on
    /// receiving a `zx::Status::SHOULD_WAIT`.
    fn read_one(&self, cx: &mut Context<'_>) -> Poll<Result<R, zx::Status>> {
        let mut entry = MaybeUninit::uninit();
        self.read(cx, &mut entry).map_ok(|count| {
            debug_assert_eq!(count, 1);
            // SAFETY: The entry was initialized by the fulfilled FIFO read.
            unsafe { entry.assume_init() }
        })
    }
}

/// An I/O object representing a `Fifo`.
pub struct Fifo<R, W = R> {
    handle: RWHandle<zx::Fifo<R, W>>,
}

impl<R, W> AsRef<zx::Fifo<R, W>> for Fifo<R, W> {
    fn as_ref(&self) -> &zx::Fifo<R, W> {
        self.handle.get_ref()
    }
}

impl<R, W> AsHandleRef for Fifo<R, W> {
    fn as_handle_ref(&self) -> zx::HandleRef<'_> {
        self.handle.get_ref().as_handle_ref()
    }
}

impl<R, W> From<Fifo<R, W>> for zx::Fifo<R, W> {
    fn from(fifo: Fifo<R, W>) -> zx::Fifo<R, W> {
        fifo.handle.into_inner()
    }
}

impl<R: FifoEntry, W: FifoEntry> Fifo<R, W> {
    /// Creates a new `Fifo` from a previously-created `zx::Fifo`.
    ///
    /// # Panics
    ///
    /// If called on a thread that does not have a current async executor.
    pub fn from_fifo(fifo: impl Into<zx::Fifo<R, W>>) -> Self {
        Fifo { handle: RWHandle::new(fifo.into()) }
    }

    /// Writes entries to the fifo and registers this `Fifo` as
    /// needing a write on receiving a `zx::Status::SHOULD_WAIT`.
    ///
    /// Returns the number of elements processed.
    pub fn try_write<B: ?Sized + FifoWriteBuffer<W>>(
        &self,
        cx: &mut Context<'_>,
        entries: &B,
    ) -> Poll<Result<usize, zx::Status>> {
        ready!(self.handle.poll_writable(cx)?);

        let buf = entries.as_ptr();
        let count = entries.count();
        let fifo = self.as_ref();
        // SAFETY: Safety relies on the pointer returned by `B` being valid,
        // which itself depends on a correct implementation of `FifoEntry` for
        // `W`.
        loop {
            let result = unsafe { fifo.write_raw(buf, count) };
            match result {
                Err(zx::Status::SHOULD_WAIT) => ready!(self.handle.need_writable(cx)?),
                Err(e) => return Poll::Ready(Err(e)),
                Ok(count) => return Poll::Ready(Ok(count)),
            }
        }
    }

    /// Reads entries from the fifo into `entries` and registers this `Fifo` as
    /// needing a read on receiving a `zx::Status::SHOULD_WAIT`.
    pub fn try_read<B: ?Sized + FifoReadBuffer<R>>(
        &self,
        cx: &mut Context<'_>,
        entries: &mut B,
    ) -> Poll<Result<usize, zx::Status>> {
        ready!(self.handle.poll_readable(cx)?);

        let buf = entries.as_mut_ptr();
        let count = entries.count();
        let fifo = self.as_ref();

        loop {
            // SAFETY: Safety relies on the pointer returned by `B` being valid,
            // which itself depends on a correct implementation of `FifoEntry` for
            // `R`.
            let result = unsafe { fifo.read_raw(buf, count) };

            match result {
                Err(zx::Status::SHOULD_WAIT) => ready!(self.handle.need_readable(cx)?),
                Err(e) => return Poll::Ready(Err(e)),
                Ok(count) => return Poll::Ready(Ok(count)),
            }
        }
    }
}

impl<R: FifoEntry, W: FifoEntry> FifoReadable<R> for Fifo<R, W> {
    fn read<B: ?Sized + FifoReadBuffer<R>>(
        &self,
        cx: &mut Context<'_>,
        entries: &mut B,
    ) -> Poll<Result<usize, zx::Status>> {
        self.try_read(cx, entries)
    }
}

impl<R: FifoEntry, W: FifoEntry> FifoWritable<W> for Fifo<R, W> {
    fn write<B: ?Sized + FifoWriteBuffer<W>>(
        &self,
        cx: &mut Context<'_>,
        entries: &B,
    ) -> Poll<Result<usize, zx::Status>> {
        self.try_write(cx, entries)
    }
}

impl<R, W> fmt::Debug for Fifo<R, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.handle.get_ref().fmt(f)
    }
}

/// WriteEntries represents the future of one or more writes.
pub struct WriteEntries<'a, F, B: ?Sized, T> {
    fifo: &'a F,
    entries: Option<OffsetWriteBuffer<'a, B, T>>,
    marker: PhantomData<T>,
}

impl<'a, F, B: ?Sized, T> Unpin for WriteEntries<'a, F, B, T> {}

impl<'a, T: FifoEntry, F: FifoWritable<T>, B: ?Sized + FifoWriteBuffer<T>>
    WriteEntries<'a, F, B, T>
{
    /// Create a new WriteEntries, which borrows the `FifoWritable` type
    /// until the future completes.
    pub fn new(fifo: &'a F, entries: &'a B) -> Self {
        WriteEntries { fifo, entries: Some(OffsetWriteBuffer::new(entries)), marker: PhantomData }
    }
}

impl<'a, T: FifoEntry, F: FifoWritable<T>, B: ?Sized + FifoWriteBuffer<T>> Future
    for WriteEntries<'a, F, B, T>
{
    type Output = Result<(), zx::Status>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        while let Some(entries) = this.entries.as_ref() {
            let advance = ready!(this.fifo.write(cx, entries)?);
            // Unwrap is okay because we know entries is `Some`. This is cleaner
            // than taking from entries and having to put it back on failed
            // poll.
            this.entries = this.entries.take().unwrap().advance(advance);
        }
        Poll::Ready(Ok(()))
    }
}

/// ReadEntries represents the future of a single read with multiple entries.
pub struct ReadEntries<'a, F, B: ?Sized, T> {
    fifo: &'a F,
    entries: &'a mut B,
    marker: PhantomData<T>,
}

impl<'a, F, B: ?Sized, T> Unpin for ReadEntries<'a, F, B, T> {}

impl<'a, T: FifoEntry, F: FifoReadable<T>, B: ?Sized + FifoReadBuffer<T>> ReadEntries<'a, F, B, T> {
    /// Create a new ReadEntries, which borrows the `FifoReadable` type
    /// until the future completes.
    pub fn new(fifo: &'a F, entries: &'a mut B) -> Self {
        ReadEntries { fifo, entries, marker: PhantomData }
    }
}

impl<'a, T: FifoEntry, F: FifoReadable<T>, B: ?Sized + FifoReadBuffer<T>> Future
    for ReadEntries<'a, F, B, T>
{
    type Output = Result<usize, zx::Status>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        this.fifo.read(cx, this.entries)
    }
}

/// ReadOne represents the future of a single read yielding a single entry.
pub struct ReadOne<'a, F, T> {
    fifo: &'a F,
    marker: PhantomData<T>,
}

impl<'a, F, T> Unpin for ReadOne<'a, F, T> {}

impl<'a, T: FifoEntry, F: FifoReadable<T>> ReadOne<'a, F, T> {
    /// Create a new ReadOne, which borrows the `FifoReadable` type
    /// until the future completes.
    pub fn new(fifo: &'a F) -> Self {
        ReadOne { fifo, marker: PhantomData }
    }
}

impl<'a, T: FifoEntry, F: FifoReadable<T>> Future for ReadOne<'a, F, T> {
    type Output = Result<T, zx::Status>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        this.fifo.read_one(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DurationExt, TestExecutor, TimeoutExt, Timer};
    use fuchsia_zircon::prelude::*;
    use futures::future::try_join;
    use futures::prelude::*;
    use zerocopy::{Immutable, KnownLayout};

    #[derive(
        Copy, Clone, Debug, PartialEq, Eq, Default, IntoBytes, KnownLayout, FromBytes, Immutable,
    )]
    #[repr(C)]
    struct Entry {
        a: u32,
        b: u32,
    }

    #[derive(
        Clone, Debug, PartialEq, Eq, Default, IntoBytes, KnownLayout, FromBytes, Immutable,
    )]
    #[repr(C)]
    struct WrongEntry {
        a: u16,
    }

    #[test]
    fn can_read_write() {
        let mut exec = TestExecutor::new();
        let element = Entry { a: 10, b: 20 };

        let (tx, rx) = zx::Fifo::<Entry>::create(2).expect("failed to create zx fifo");
        let (tx, rx) = (Fifo::from_fifo(tx), Fifo::from_fifo(rx));

        let mut buffer = Entry::default();
        let receive_future = rx.read_entries(&mut buffer).map_ok(|count| {
            assert_eq!(count, 1);
        });

        // add a timeout to receiver so if test is broken it doesn't take forever
        let receiver = receive_future.on_timeout(300.millis().after_now(), || panic!("timeout"));

        // Sends an entry after the timeout has passed
        let sender = Timer::new(10.millis().after_now()).then(|()| tx.write_entries(&element));

        let done = try_join(receiver, sender);
        exec.run_singlethreaded(done).expect("failed to run receive future on executor");
        assert_eq!(buffer, element);
    }

    #[test]
    fn read_wrong_size() {
        let mut exec = TestExecutor::new();
        let elements = &[Entry { a: 10, b: 20 }][..];

        let (tx, rx) = zx::Fifo::<Entry>::create(2).expect("failed to create zx fifo");
        let wrong_rx = zx::Fifo::<WrongEntry>::from(rx.into_handle());
        let (tx, rx) = (Fifo::from_fifo(tx), Fifo::from_fifo(wrong_rx));

        let mut buffer = WrongEntry::default();
        let receive_future = rx
            .read_entries(&mut buffer)
            .map_ok(|count| panic!("read should have failed, got {}", count));

        // add a timeout to receiver so if test is broken it doesn't take forever
        let receiver = receive_future.on_timeout(300.millis().after_now(), || panic!("timeout"));

        // Sends an entry after the timeout has passed
        let sender = Timer::new(10.millis().after_now()).then(|()| tx.write_entries(elements));

        let done = try_join(receiver, sender);
        let res = exec.run_singlethreaded(done);
        match res {
            Err(zx::Status::OUT_OF_RANGE) => (),
            _ => panic!("did not get out-of-range error"),
        }
    }

    #[test]
    fn write_wrong_size() {
        let mut exec = TestExecutor::new();
        let elements = &[WrongEntry { a: 10 }][..];

        let (tx, rx) = zx::Fifo::<Entry>::create(2).expect("failed to create zx fifo");
        let wrong_tx = zx::Fifo::<WrongEntry>::from(tx.into_handle());
        let wrong_rx = zx::Fifo::<WrongEntry>::from(rx.into_handle());
        let (tx, _rx) = (Fifo::from_fifo(wrong_tx), Fifo::from_fifo(wrong_rx));

        let sender = Timer::new(10.millis().after_now()).then(|()| tx.write_entries(elements));

        let res = exec.run_singlethreaded(sender);
        match res {
            Err(zx::Status::OUT_OF_RANGE) => (),
            _ => panic!("did not get out-of-range error"),
        }
    }

    #[test]
    fn write_into_full() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let mut exec = TestExecutor::new();
        let elements =
            &[Entry { a: 10, b: 20 }, Entry { a: 30, b: 40 }, Entry { a: 50, b: 60 }][..];

        let (tx, rx) = zx::Fifo::<Entry>::create(2).expect("failed to create zx fifo");
        let (tx, rx) = (Fifo::from_fifo(tx), Fifo::from_fifo(rx));

        // Use `writes_completed` to verify that not all writes
        // are transmitted at once, and the last write is actually blocked.
        let writes_completed = AtomicUsize::new(0);
        let sender = async {
            tx.write_entries(&elements[..2]).await?;
            writes_completed.fetch_add(1, Ordering::SeqCst);
            tx.write_entries(&elements[2..]).await?;
            writes_completed.fetch_add(1, Ordering::SeqCst);
            Ok::<(), zx::Status>(())
        };

        // Wait 10 ms, then read the messages from the fifo.
        let receive_future = async {
            Timer::new(10.millis().after_now()).await;
            let mut buffer = Entry::default();
            let count = rx.read_entries(&mut buffer).await?;
            assert_eq!(writes_completed.load(Ordering::SeqCst), 1);
            assert_eq!(count, 1);
            assert_eq!(buffer, elements[0]);
            let count = rx.read_entries(&mut buffer).await?;
            // At this point, the last write may or may not have
            // been written.
            assert_eq!(count, 1);
            assert_eq!(buffer, elements[1]);
            let count = rx.read_entries(&mut buffer).await?;
            assert_eq!(writes_completed.load(Ordering::SeqCst), 2);
            assert_eq!(count, 1);
            assert_eq!(buffer, elements[2]);
            Ok::<(), zx::Status>(())
        };

        // add a timeout to receiver so if test is broken it doesn't take forever
        let receiver = receive_future.on_timeout(300.millis().after_now(), || panic!("timeout"));

        let done = try_join(receiver, sender);

        exec.run_singlethreaded(done).expect("failed to run receive future on executor");
    }

    #[test]
    fn write_more_than_full() {
        let mut exec = TestExecutor::new();
        let elements =
            &[Entry { a: 10, b: 20 }, Entry { a: 30, b: 40 }, Entry { a: 50, b: 60 }][..];

        let (tx, rx) = zx::Fifo::<Entry>::create(2).expect("failed to create zx fifo");
        let (tx, rx) = (Fifo::from_fifo(tx), Fifo::from_fifo(rx));

        let sender = tx.write_entries(elements);

        // Wait 10 ms, then read the messages from the fifo.
        let receive_future = async {
            Timer::new(10.millis().after_now()).await;
            for e in elements {
                let mut buffer = [Entry::default(); 1];
                let count = rx.read_entries(&mut buffer[..]).await?;
                assert_eq!(count, 1);
                assert_eq!(&buffer[0], e);
            }
            Ok::<(), zx::Status>(())
        };

        // add a timeout to receiver so if test is broken it doesn't take forever
        let receiver = receive_future.on_timeout(300.millis().after_now(), || panic!("timeout"));

        let done = try_join(receiver, sender);

        exec.run_singlethreaded(done).expect("failed to run receive future on executor");
    }

    #[test]
    fn read_multiple() {
        let mut exec = TestExecutor::new();
        let elements =
            &[Entry { a: 10, b: 20 }, Entry { a: 30, b: 40 }, Entry { a: 50, b: 60 }][..];
        let (tx, rx) = zx::Fifo::<Entry>::create(elements.len()).expect("failed to create zx fifo");
        let (tx, rx) = (Fifo::from_fifo(tx), Fifo::from_fifo(rx));

        let write_fut = async {
            tx.write_entries(&elements[..]).await.expect("failed write entries");
        };
        let read_fut = async {
            // Use a larger buffer to show partial reads.
            let mut buffer = [Entry::default(); 5];
            let count = rx.read_entries(&mut buffer[..]).await.expect("failed to read entries");
            assert_eq!(count, elements.len());
            assert_eq!(&buffer[..count], &elements[..]);
        };
        let ((), ()) = exec.run_singlethreaded(futures::future::join(write_fut, read_fut));
    }

    #[test]
    fn read_one() {
        let mut exec = TestExecutor::new();
        let elements =
            &[Entry { a: 10, b: 20 }, Entry { a: 30, b: 40 }, Entry { a: 50, b: 60 }][..];
        let (tx, rx) = zx::Fifo::<Entry>::create(elements.len()).expect("failed to create zx fifo");
        let (tx, rx) = (Fifo::from_fifo(tx), Fifo::from_fifo(rx));

        let write_fut = async {
            tx.write_entries(&elements[..]).await.expect("failed write entries");
        };
        let read_fut = async {
            for e in elements {
                let received = rx.read_entry().await.expect("failed to read entry");
                assert_eq!(&received, e);
            }
        };
        let ((), ()) = exec.run_singlethreaded(futures::future::join(write_fut, read_fut));
    }

    #[test]
    fn maybe_uninit_single() {
        let mut exec = TestExecutor::new();
        let element = Entry { a: 10, b: 20 };
        let (tx, rx) = zx::Fifo::<Entry>::create(1).expect("failed to create zx fifo");
        let (tx, rx) = (Fifo::from_fifo(tx), Fifo::from_fifo(rx));

        let write_fut = async {
            tx.write_entries(&element).await.expect("failed write entries");
        };
        let read_fut = async {
            let mut buffer = MaybeUninit::<Entry>::uninit();
            let count = rx.read_entries(&mut buffer).await.expect("failed to read entries");
            assert_eq!(count, 1);
            // SAFETY: We just read a new entry into the buffer.
            let read = unsafe { buffer.assume_init() };
            assert_eq!(read, element);
        };
        let ((), ()) = exec.run_singlethreaded(futures::future::join(write_fut, read_fut));
    }

    #[test]
    fn maybe_uninit_slice() {
        let mut exec = TestExecutor::new();
        let elements =
            &[Entry { a: 10, b: 20 }, Entry { a: 30, b: 40 }, Entry { a: 50, b: 60 }][..];
        let (tx, rx) = zx::Fifo::<Entry>::create(elements.len()).expect("failed to create zx fifo");
        let (tx, rx) = (Fifo::from_fifo(tx), Fifo::from_fifo(rx));

        let write_fut = async {
            tx.write_entries(&elements[..]).await.expect("failed write entries");
        };
        let read_fut = async {
            // Use a larger buffer to show partial reads.
            let mut buffer = [MaybeUninit::<Entry>::uninit(); 15];
            let count = rx.read_entries(&mut buffer[..]).await.expect("failed to read entries");
            assert_eq!(count, elements.len());
            let read = &mut buffer[..count];
            for (i, v) in read.iter_mut().enumerate() {
                // SAFETY: This is the read region of the buffer, initialized by
                // reading from the FIFO.
                let read = unsafe { v.assume_init_ref() };
                assert_eq!(read, &elements[i]);
                // SAFETY: The buffer was partially initialized by reading from
                // the FIFO, the correct thing to do here is to manually drop
                // the elements that were initialized.
                unsafe {
                    v.assume_init_drop();
                }
            }
        };
        let ((), ()) = exec.run_singlethreaded(futures::future::join(write_fut, read_fut));
    }
}
