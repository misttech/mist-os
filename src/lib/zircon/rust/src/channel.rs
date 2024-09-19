// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon channel objects.

use crate::{
    ok, AsHandleRef, Handle, HandleBased, HandleDisposition, HandleInfo, HandleRef, MonotonicTime,
    ObjectType, Peered, Rights, Status,
};
use fuchsia_zircon_sys as sys;
use std::mem::{self, MaybeUninit};

/// An object representing a Zircon
/// [channel](https://fuchsia.dev/fuchsia-src/concepts/objects/channel.md).
///
/// As essentially a subtype of `Handle`, it can be freely interconverted.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Channel(Handle);
impl_handle_based!(Channel);
impl Peered for Channel {}

impl Channel {
    /// Create a channel, resulting in a pair of `Channel` objects representing both
    /// sides of the channel. Messages written into one may be read from the opposite.
    ///
    /// Wraps the
    /// [zx_channel_create](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_create.md)
    /// syscall.
    ///
    /// # Panics
    ///
    /// If the process' job policy denies channel creation or the kernel reports no memory
    /// available to create a new channel.
    pub fn create() -> (Self, Self) {
        unsafe {
            let mut handle0 = 0;
            let mut handle1 = 0;
            let opts = 0;
            ok(sys::zx_channel_create(opts, &mut handle0, &mut handle1)).expect(
                "channel creation always succeeds except with OOM or when job policy denies it",
            );
            (Self(Handle::from_raw(handle0)), Self(Handle::from_raw(handle1)))
        }
    }

    /// Read a message from a channel. Wraps the
    /// [zx_channel_read](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_read.md)
    /// syscall. Care should be taken to avoid handle leaks by either transferring the
    /// returned handles out to another type or dropping them explicitly.
    pub fn read_uninit(
        &self,
        bytes: &mut [MaybeUninit<u8>],
        handles: &mut [MaybeUninit<Handle>],
    ) -> ChannelReadResult<(&mut [u8], &mut [Handle])> {
        // SAFETY: bytes and handles are valid to write to for their lengths
        match unsafe {
            self.read_raw(
                bytes.as_mut_ptr() as *mut u8,
                bytes.len(),
                handles.as_mut_ptr() as *mut Handle,
                handles.len(),
            )
        } {
            ChannelReadResult::Ok((actual_bytes, actual_handles)) => {
                // SAFETY: if the above call succeeded, the buffers are initialized up to actual_*
                ChannelReadResult::Ok(unsafe {
                    (
                        std::slice::from_raw_parts_mut(
                            bytes.as_mut_ptr() as *mut u8,
                            actual_bytes as usize,
                        ),
                        std::slice::from_raw_parts_mut(
                            handles.as_mut_ptr() as *mut Handle,
                            actual_handles as usize,
                        ),
                    )
                })
            }
            ChannelReadResult::BufferTooSmall { bytes_avail, handles_avail } => {
                ChannelReadResult::BufferTooSmall { bytes_avail, handles_avail }
            }
            ChannelReadResult::Err(e) => ChannelReadResult::Err(e),
        }
    }

    /// Read a message from a channel. Wraps the
    /// [zx_channel_read](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_read.md)
    /// syscall.
    ///
    /// # Safety
    ///
    /// `bytes` must be valid to write to for `bytes_len` bytes. `handles` must be valid to write
    /// to for `handles_len` elements.
    pub unsafe fn read_raw(
        &self,
        bytes: *mut u8,
        bytes_len: usize,
        handles: *mut Handle,
        handles_len: usize,
    ) -> ChannelReadResult<(usize, usize)> {
        // SAFETY: invariants for these pointers are upheld by our caller.
        unsafe {
            let raw_handle = self.raw_handle();
            let mut actual_bytes = 0;
            let mut actual_handles = 0;
            let status = ok(sys::zx_channel_read(
                raw_handle,
                0, // opts
                bytes,
                handles as *mut _,
                bytes_len as u32,
                handles_len as u32,
                &mut actual_bytes,
                &mut actual_handles,
            ));
            match status {
                Ok(()) => ChannelReadResult::Ok((actual_bytes as usize, actual_handles as usize)),
                Err(Status::BUFFER_TOO_SMALL) => ChannelReadResult::BufferTooSmall {
                    bytes_avail: actual_bytes as usize,
                    handles_avail: actual_handles as usize,
                },
                Err(e) => ChannelReadResult::Err(e),
            }
        }
    }

    /// Read a message from a channel.
    ///
    /// Note that this method can cause internal reallocations in the `MessageBuf`
    /// if it is lacks capacity to hold the full message. If such reallocations
    /// are not desirable, use `read_raw` instead.
    pub fn read(&self, buf: &mut MessageBuf) -> Result<(), Status> {
        let (bytes, handles) = buf.split_mut();
        self.read_split(bytes, handles)
    }

    /// Read a message from a channel into a separate byte vector and handle vector.
    ///
    /// If the provided `handles` has any elements, they will be dropped before reading from the
    /// channel.
    ///
    /// Note that this method can cause internal reallocations in the `Vec`s
    /// if they lacks capacity to hold the full message. If such reallocations
    /// are not desirable, use `read_uninit` instead.
    pub fn read_split(&self, bytes: &mut Vec<u8>, handles: &mut Vec<Handle>) -> Result<(), Status> {
        loop {
            // Ensure the capacity slices are the entire `Vec`s.
            bytes.truncate(0);
            handles.truncate(0);
            match self.read_uninit(bytes.spare_capacity_mut(), handles.spare_capacity_mut()) {
                ChannelReadResult::Ok((byte_slice, handle_slice)) => {
                    // SAFETY: the kernel has initialized the vecs up to the length of these slices.
                    unsafe {
                        bytes.set_len(byte_slice.len());
                        handles.set_len(handle_slice.len());
                    }
                    return Ok(());
                }
                ChannelReadResult::BufferTooSmall { bytes_avail, handles_avail } => {
                    ensure_capacity(bytes, bytes_avail);
                    ensure_capacity(handles, handles_avail);
                }
                ChannelReadResult::Err(e) => return Err(e),
            }
        }
    }

    /// Read a message from a channel. Wraps the
    /// [zx_channel_read](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_read.md)
    /// syscall.
    pub fn read_etc_uninit(
        &self,
        bytes: &mut [MaybeUninit<u8>],
        handles: &mut [MaybeUninit<HandleInfo>],
    ) -> ChannelReadResult<(&mut [u8], &mut [HandleInfo])> {
        // SAFETY: bytes and handles are valid to write to for their lengths
        match unsafe {
            self.read_etc_raw(
                bytes.as_mut_ptr() as *mut u8,
                bytes.len(),
                handles.as_mut_ptr() as *mut HandleInfo,
                handles.len(),
            )
        } {
            ChannelReadResult::Ok((actual_bytes, actual_handles)) => {
                // SAFETY: if the above call succeeded, the buffers are initialized up to actual_*
                ChannelReadResult::Ok(unsafe {
                    (
                        std::slice::from_raw_parts_mut(
                            bytes.as_mut_ptr() as *mut u8,
                            actual_bytes as usize,
                        ),
                        std::slice::from_raw_parts_mut(
                            handles.as_mut_ptr() as *mut HandleInfo,
                            actual_handles as usize,
                        ),
                    )
                })
            }
            ChannelReadResult::BufferTooSmall { bytes_avail, handles_avail } => {
                ChannelReadResult::BufferTooSmall { bytes_avail, handles_avail }
            }
            ChannelReadResult::Err(e) => ChannelReadResult::Err(e),
        }
    }

    /// Read a message from a channel.
    /// Wraps the [zx_channel_read_etc](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_read_etc.md)
    /// syscall.
    ///
    /// This differs from `read_raw` in that it returns extended information on
    /// the handles.
    ///
    /// On success, returns the number of bytes and handles read.
    ///
    /// # Safety
    ///
    /// `bytes` must be valid to write to for `bytes_len` bytes.
    ///
    /// `handles` must be valid to write to for `handles_len` elements.
    pub unsafe fn read_etc_raw(
        &self,
        bytes: *mut u8,
        bytes_len: usize,
        handles: *mut HandleInfo,
        handles_len: usize,
    ) -> ChannelReadResult<(usize, usize)> {
        // SAFETY: our caller upholds the require invariants. It is sound to interpret
        // HandleInfo as zx_handle_info_t as they have identical layouts.
        unsafe {
            let mut actual_bytes = 0;
            let mut actual_handles = 0;
            let status = ok(sys::zx_channel_read_etc(
                self.raw_handle(),
                0, // options
                bytes,
                handles as *mut sys::zx_handle_info_t,
                bytes_len as u32,
                handles_len as u32,
                &mut actual_bytes,
                &mut actual_handles,
            ));
            match status {
                Ok(()) => ChannelReadResult::Ok((actual_bytes as usize, actual_handles as usize)),
                Err(Status::BUFFER_TOO_SMALL) => ChannelReadResult::BufferTooSmall {
                    bytes_avail: actual_bytes as usize,
                    handles_avail: actual_handles as usize,
                },
                Err(e) => ChannelReadResult::Err(e),
            }
        }
    }

    /// Read a message from a channel.
    ///
    /// This differs from `read` in that it returns extended information on
    /// the handles.
    ///
    /// Note that this method can cause internal reallocations in the `MessageBufEtc`
    /// if it is lacks capacity to hold the full message. If such reallocations
    /// are not desirable, use `read_etc_uninit` or `read_etc_raw` instead.
    pub fn read_etc(&self, buf: &mut MessageBufEtc) -> Result<(), Status> {
        let (bytes, handles) = buf.split_mut();
        self.read_etc_split(bytes, handles)
    }

    /// Read a message from a channel into a separate byte vector and handle vector.
    ///
    /// This differs from `read_split` in that it returns extended information on
    /// the handles.
    ///
    /// Note that this method can cause internal reallocations in the `Vec`s
    /// if they lacks capacity to hold the full message. If such reallocations
    /// are not desirable, use `read_etc_uninit` or `read_etc_raw` instead.
    pub fn read_etc_split(
        &self,
        bytes: &mut Vec<u8>,
        handles: &mut Vec<HandleInfo>,
    ) -> Result<(), Status> {
        loop {
            bytes.clear();
            handles.clear();
            match self.read_etc_uninit(bytes.spare_capacity_mut(), handles.spare_capacity_mut()) {
                ChannelReadResult::Ok((byte_slice, handle_slice)) => {
                    // SAFETY: the kernel has initialized the vecs up to the length of these slices.
                    unsafe {
                        bytes.set_len(byte_slice.len());
                        handles.set_len(handle_slice.len());
                    }
                    return Ok(());
                }
                ChannelReadResult::BufferTooSmall { bytes_avail, handles_avail } => {
                    ensure_capacity(bytes, bytes_avail);
                    ensure_capacity(handles, handles_avail);
                }
                ChannelReadResult::Err(e) => return Err(e),
            }
        }
    }

    /// Write a message to a channel. Wraps the
    /// [zx_channel_write](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_write.md)
    /// syscall.
    ///
    /// On return, the elements pointed to by `handles` will have been zeroed to reflect
    /// the fact that the handles have been transferred.
    pub fn write(&self, bytes: &[u8], handles: &mut [Handle]) -> Result<(), Status> {
        // SAFETY: provided pointers are valid because they come from references.
        unsafe { self.write_raw(bytes.as_ptr(), bytes.len(), handles.as_mut_ptr(), handles.len()) }
    }

    /// Write a message to a channel. Wraps the
    /// [zx_channel_write](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_write.md)
    /// syscall.
    ///
    /// On return, the elements pointed to by `handles` will have been zeroed to reflect the
    /// fact that the handles have been transferred.
    ///
    /// # Safety
    ///
    /// `bytes` must be valid to read from for `bytes_len` elements.
    ///
    /// `handles` must be valid to read from and write to for `handles_len` elements.
    pub unsafe fn write_raw(
        &self,
        bytes: *const u8,
        bytes_len: usize,
        handles: *mut Handle,
        handles_len: usize,
    ) -> Result<(), Status> {
        // SAFETY: caller is responsible for upholding pointer invariants. `Handle` is a
        // `repr(transparent)` wrapper around `zx_handle_t`.
        let res = unsafe {
            ok(sys::zx_channel_write(
                self.raw_handle(),
                0, // options
                bytes,
                bytes_len as u32,
                handles as *const sys::zx_handle_t,
                handles_len as u32,
            ))
        };

        // Outgoing handles have been consumed by zx_channel_write_etc. Zero them to inhibit drop
        // implementations.
        // SAFETY: caller guarantees that `handles` is valid to write to for `handles_len`
        // elements. `Handle` is valid when it is all zeroes.
        unsafe {
            std::ptr::write_bytes(handles, 0, handles_len);
        }

        res
    }

    /// Write a message to a channel. Wraps the
    /// [zx_channel_write_etc](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_write_etc.md)
    /// syscall.
    ///
    /// On return, the elements pointed to by `handles` will have been zeroed to reflect the
    /// fact that the handles have been transferred.
    pub fn write_etc(
        &self,
        bytes: &[u8],
        handles: &mut [HandleDisposition<'_>],
    ) -> Result<(), Status> {
        // SAFETY: provided pointers come from valid references.
        unsafe {
            self.write_etc_raw(bytes.as_ptr(), bytes.len(), handles.as_mut_ptr(), handles.len())
        }
    }

    /// Write a message to a channel. Wraps the
    /// [zx_channel_write_etc](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_write_etc.md)
    /// syscall.
    ///
    /// On return, the elements pointed to by `handles` will have been zeroed to reflect the
    /// fact that the handles have been transferred.
    ///
    /// # Safety
    ///
    /// `bytes` must be valid to read from for `bytes_len` bytes.
    ///
    /// `handles` must be valid to read from and write to for `handles_len` elements.
    pub unsafe fn write_etc_raw(
        &self,
        bytes: *const u8,
        bytes_len: usize,
        handles: *mut HandleDisposition<'_>,
        handles_len: usize,
    ) -> Result<(), Status> {
        // SAFETY: caller is responsible for upholding pointer invariants. HandleDisposition is
        // ABI-compatible with zx_handle_disposition_t, we can treat them interchangeably.
        let res = unsafe {
            ok(sys::zx_channel_write_etc(
                self.raw_handle(),
                0, // options
                bytes,
                bytes_len as u32,
                handles as *mut sys::zx_handle_disposition_t,
                handles_len as u32,
            ))
        };

        // Outgoing handles are consumed by zx_channel_write_etc so prevent the destructor from
        // being called. Don't overwrite the status field so that callers can inspect it.
        // SAFETY: mutable slice invariants are upheld by this method's caller
        let handles = unsafe { std::slice::from_raw_parts_mut(handles, handles_len) };
        for disposition in handles {
            std::mem::forget(disposition.take_op());
        }

        res
    }

    /// Send a message consisting of the given bytes and handles to a channel and block until a
    /// reply is received or the timeout is reached.
    ///
    /// On return, the elements pointed to by `handles` will have been zeroed to reflect the
    /// fact that the handles have been transferred.
    ///
    /// The first four bytes of the written and read back messages are treated as a transaction ID
    /// of type `zx_txid_t`. The kernel generates a txid for the written message, replacing that
    /// part of the message as read from userspace. In other words, the first four bytes of
    /// `bytes` will be ignored, and the first four bytes of the response will contain a
    /// kernel-generated txid.
    ///
    /// In order to avoid dropping replies, the provided `MessageBuf` will be resized to accommodate
    /// the maximum channel message size. For performance-sensitive code, consider reusing
    /// `MessageBuf`s or calling `call_uninit` with a stack-allocated buffer.
    ///
    /// Wraps the
    /// [zx_channel_call](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_call.md)
    /// syscall.
    ///
    /// On failure returns the both the main and read status.
    pub fn call(
        &self,
        timeout: MonotonicTime,
        bytes: &[u8],
        handles: &mut [Handle],
        buf: &mut MessageBuf,
    ) -> Result<(), Status> {
        buf.clear();
        buf.ensure_capacity_bytes(sys::ZX_CHANNEL_MAX_MSG_BYTES as usize);
        buf.ensure_capacity_handles(sys::ZX_CHANNEL_MAX_MSG_HANDLES as usize);

        let (actual_bytes, actual_handles) = self.call_uninit(
            timeout,
            bytes,
            handles,
            buf.bytes.spare_capacity_mut(),
            buf.handles.spare_capacity_mut(),
        )?;

        // SAFETY: the kernel has initialized these slices with valid values after the call above
        // succeeded.
        unsafe {
            buf.bytes.set_len(actual_bytes.len());
            buf.handles.set_len(actual_handles.len());
        }

        Ok(())
    }

    /// Send a message consisting of the given bytes and handles to a channel and block until a
    /// reply is received or the timeout is reached. Returns initialized slices of byte message and
    /// handles on success. Care should be taken to avoid handle leaks by either transferring the
    /// returned handles out to another type or dropping them explicitly.
    ///
    /// On return, the elements pointed to by `handles_in` will have been zeroed to reflect the
    /// fact that the handles have been transferred.
    ///
    /// The first four bytes of the written and read back messages are treated as a transaction ID
    /// of type `zx_txid_t`. The kernel generates a txid for the written message, replacing that
    /// part of the message as read from userspace. In other words, the first four bytes of
    /// `bytes_in` will be ignored, and the first four bytes of the response will contain a
    /// kernel-generated txid.
    ///
    /// In order to avoid dropping replies, this wrapper requires that the provided out buffers have
    /// enough space to handle the largest channel messages possible.
    ///
    /// Wraps the
    /// [zx_channel_call](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_call.md)
    /// syscall.
    ///
    /// On failure returns the both the main and read status.
    pub fn call_uninit(
        &self,
        timeout: MonotonicTime,
        bytes_in: &[u8],
        handles_in: &mut [Handle],
        bytes_out: &mut [MaybeUninit<u8>],
        handles_out: &mut [MaybeUninit<Handle>],
    ) -> Result<(&mut [u8], &mut [Handle]), Status> {
        // SAFETY: in-pointers are both valid to read from for their provided lengths, and
        // out-pointers are both valid to write to for their provided lengths.
        let (actual_bytes, actual_handles) = unsafe {
            self.call_raw(
                timeout,
                bytes_in.as_ptr(),
                bytes_in.len(),
                handles_in.as_mut_ptr(),
                handles_in.len(),
                bytes_out.as_mut_ptr() as *mut u8,
                bytes_out.len(),
                handles_out.as_mut_ptr() as *mut Handle,
                handles_out.len(),
            )?
        };

        // SAFETY: the kernel has initialized these slices with valid values.
        unsafe {
            Ok((
                std::slice::from_raw_parts_mut(
                    bytes_out.as_mut_ptr() as *mut u8,
                    actual_bytes as usize,
                ),
                std::slice::from_raw_parts_mut(
                    handles_out.as_mut_ptr() as *mut Handle,
                    actual_handles as usize,
                ),
            ))
        }
    }

    /// Send a message consisting of the given bytes and handles to a channel and block until a
    /// reply is received or the timeout is reached. On success, returns the number of bytes and
    /// handles read from the reply, in that order.
    ///
    /// The first four bytes of the written and read back messages are treated as a transaction ID
    /// of type `zx_txid_t`. The kernel generates a txid for the written message, replacing that
    /// part of the message as read from userspace. In other words, the first four bytes of
    /// `bytes_in` will be ignored, and the first four bytes of the response will contain a
    /// kernel-generated txid.
    ///
    /// In order to avoid dropping replies, this wrapper requires that the provided out buffers have
    /// enough space to handle the largest channel messages possible.
    ///
    /// On return, the elements pointed to by `handles_in` will have been zeroed to reflect the
    /// fact that the handles have been transferred.
    ///
    /// Wraps the
    /// [zx_channel_call](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_call.md)
    /// syscall.
    ///
    /// On failure returns the both the main and read status.
    ///
    /// # Safety
    ///
    /// `bytes_in` must be valid to read from for `bytes_in_len` bytes.
    ///
    /// `handles_in` must be valid to read from and write to for `handles_in_len` elements.
    ///
    /// `bytes_out` must be valid to write to for `bytes_out_len` bytes.
    ///
    /// `handles_out` must be valid to write to for `handles_out_len` elements.
    ///
    /// `bytes_in` and `bytes_out` may overlap. `handles_in` and `handles_out` may not overlap.
    pub unsafe fn call_raw(
        &self,
        timeout: MonotonicTime,
        bytes_in: *const u8,
        bytes_in_len: usize,
        handles_in: *mut Handle,
        handles_in_len: usize,
        bytes_out: *mut u8,
        bytes_out_len: usize,
        handles_out: *mut Handle,
        handles_out_len: usize,
    ) -> Result<(usize, usize), Status> {
        // Don't let replies get silently dropped.
        if bytes_out_len < sys::ZX_CHANNEL_MAX_MSG_BYTES as usize
            || handles_out_len < sys::ZX_CHANNEL_MAX_MSG_HANDLES as usize
        {
            return Err(Status::BUFFER_TOO_SMALL);
        }

        let mut actual_read_bytes: u32 = 0;
        let mut actual_read_handles: u32 = 0;

        // SAFETY: pointer invariants are upheld by this method's caller, see Safety section in
        // docs. Handle is ABI-compatible with zx_handle_t, this allows the kernel to safely write
        // the latter and and for us to later interpret them as the former.
        let res = unsafe {
            ok(sys::zx_channel_call(
                self.raw_handle(),
                0, // options
                timeout.into_nanos(),
                &sys::zx_channel_call_args_t {
                    wr_bytes: bytes_in,
                    wr_num_bytes: bytes_in_len as u32,
                    wr_handles: handles_in as *mut sys::zx_handle_t,
                    wr_num_handles: handles_in_len as u32,
                    rd_bytes: bytes_out,
                    rd_num_bytes: bytes_out_len as u32,
                    rd_handles: handles_out as *mut sys::zx_handle_t,
                    rd_num_handles: handles_out_len as u32,
                },
                &mut actual_read_bytes,
                &mut actual_read_handles,
            ))
        };

        // Outgoing handles have been consumed by zx_channel_call_etc. Zero them to inhibit drop
        // implementations.
        // SAFETY: caller guarantees that `handles_in` is valid to write to for `handles_in_len`
        // elements. `HandleDisposition` is valid when it is all zeroes.
        unsafe {
            std::ptr::write_bytes(handles_in, 0, handles_in_len);
        }

        // Only error-return after zeroing out handles.
        res?;

        Ok((actual_read_bytes as usize, actual_read_handles as usize))
    }

    /// Send a message consisting of the given bytes and handles to a channel and block until a
    /// reply is received or the timeout is reached.
    ///
    /// On return, the elements pointed to by `handle_dispositions` will have been zeroed to reflect
    /// the fact that the handles have been transferred.
    ///
    /// The first four bytes of the written and read back messages are treated as a transaction ID
    /// of type `zx_txid_t`. The kernel generates a txid for the written message, replacing that
    /// part of the message as read from userspace. In other words, the first four bytes of
    /// `bytes` will be ignored, and the first four bytes of the response will contain a
    /// kernel-generated txid.
    ///
    /// In order to avoid dropping replies, the provided `MessageBufEtc` will be resized to
    /// accomodate the maximum channel message size. For performance-sensitive code, consider
    /// reusing `MessageBufEtc`s or calling `call_etc_uninit` with a stack-allocated buffer.
    ///
    /// This differs from `call`, in that it uses extended handle info.
    ///
    /// Wraps the
    /// [zx_channel_call_etc](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_call_etc.md)
    /// syscall.
    ///
    /// On failure returns the both the main and read status.
    pub fn call_etc(
        &self,
        timeout: MonotonicTime,
        bytes: &[u8],
        handle_dispositions: &mut [HandleDisposition<'_>],
        buf: &mut MessageBufEtc,
    ) -> Result<(), Status> {
        buf.clear();
        buf.ensure_capacity_bytes(sys::ZX_CHANNEL_MAX_MSG_BYTES as usize);
        buf.ensure_capacity_handle_infos(sys::ZX_CHANNEL_MAX_MSG_HANDLES as usize);

        let (actual_bytes, actual_handles) = self.call_etc_uninit(
            timeout,
            bytes,
            handle_dispositions,
            buf.bytes.spare_capacity_mut(),
            buf.handle_infos.spare_capacity_mut(),
        )?;

        // SAFETY: the kernel has initialized these slices with valid values after the call above
        // succeeded.
        unsafe {
            buf.bytes.set_len(actual_bytes.len());
            buf.handle_infos.set_len(actual_handles.len());
        }

        Ok(())
    }

    /// Send a message consisting of the given bytes and handles to a channel and block until a
    /// reply is received or the timeout is reached. Returns initialized slices of byte message and
    /// handles on success. Care should be taken to avoid handle leaks by either transferring the
    /// returned handles out to another type or dropping them explicitly.
    ///
    /// On return, the elements pointed to by `handles_in` will have been zeroed to reflect
    /// the fact that the handles have been transferred.
    ///
    /// The first four bytes of the written and read back messages are treated as a transaction ID
    /// of type `zx_txid_t`. The kernel generates a txid for the written message, replacing that
    /// part of the message as read from userspace. In other words, the first four bytes of
    /// `bytes_in` will be ignored, and the first four bytes of the response will contain a
    /// kernel-generated txid.
    ///
    /// This differs from `call`, in that it uses extended handle info.
    ///
    /// In order to avoid dropping replies, this wrapper requires that the provided out buffers have
    /// enough space to handle the largest channel messages possible.
    ///
    /// Wraps the
    /// [zx_channel_call_etc](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_call_etc.md)
    /// syscall.
    ///
    /// On failure returns the both the main and read status.
    pub fn call_etc_uninit(
        &self,
        timeout: MonotonicTime,
        bytes_in: &[u8],
        handles_in: &mut [HandleDisposition<'_>],
        bytes_out: &mut [MaybeUninit<u8>],
        handles_out: &mut [MaybeUninit<HandleInfo>],
    ) -> Result<(&mut [u8], &mut [HandleInfo]), Status> {
        // SAFETY: in-pointers are both valid to read from for their provided lengths, and
        // out-pointers are both valid to write to for their provided lengths.
        let (actual_bytes, actual_handles) = unsafe {
            self.call_etc_raw(
                timeout,
                bytes_in.as_ptr(),
                bytes_in.len(),
                handles_in.as_mut_ptr(),
                handles_in.len(),
                bytes_out.as_mut_ptr() as *mut u8,
                bytes_out.len(),
                handles_out.as_mut_ptr() as *mut HandleInfo,
                handles_out.len(),
            )?
        };

        // SAFETY: the kernel has initialized these slices with valid values.
        unsafe {
            Ok((
                std::slice::from_raw_parts_mut(
                    bytes_out.as_mut_ptr() as *mut u8,
                    actual_bytes as usize,
                ),
                std::slice::from_raw_parts_mut(
                    handles_out.as_mut_ptr() as *mut HandleInfo,
                    actual_handles as usize,
                ),
            ))
        }
    }

    /// Send a message consisting of the given bytes and handles to a channel and block until a
    /// reply is received or the timeout is reached. On success, returns the number of bytes and
    /// handles read from the reply, in that order.
    ///
    /// The first four bytes of the written and read back messages are treated as a transaction ID
    /// of type `zx_txid_t`. The kernel generates a txid for the written message, replacing that
    /// part of the message as read from userspace. In other words, the first four bytes of
    /// `bytes_in` will be ignored, and the first four bytes of the response will contain a
    /// kernel-generated txid.
    ///
    /// This differs from `call`, in that it uses extended handle info.
    ///
    /// In order to avoid dropping replies, this wrapper requires that the provided out buffers have
    /// enough space to handle the largest channel messages possible.
    ///
    /// On return, the elements pointed to by `handles_in` will have been zeroed to reflect the
    /// fact that the handles have been transferred.
    ///
    /// Wraps the
    /// [zx_channel_call_etc](https://fuchsia.dev/fuchsia-src/reference/syscalls/channel_call_etc.md)
    /// syscall.
    ///
    /// On failure returns the both the main and read status.
    ///
    /// # Safety
    ///
    /// `bytes_in` must be valid to read from for `bytes_in_len` bytes.
    ///
    /// `handles_in` must be valid to read from and write to for `handles_in_len` elements.
    ///
    /// `bytes_out` must be valid to write to for `bytes_out_len` bytes.
    ///
    /// `handles_out` must be valid to write to for `handles_out_len` elements.
    ///
    /// `bytes_in` and `bytes_out` may overlap.
    pub unsafe fn call_etc_raw(
        &self,
        timeout: MonotonicTime,
        bytes_in: *const u8,
        bytes_in_len: usize,
        handles_in: *mut HandleDisposition<'_>,
        handles_in_len: usize,
        bytes_out: *mut u8,
        bytes_out_len: usize,
        handles_out: *mut HandleInfo,
        handles_out_len: usize,
    ) -> Result<(usize, usize), Status> {
        // Don't let replies get silently dropped.
        if bytes_out_len < sys::ZX_CHANNEL_MAX_MSG_BYTES as usize
            || handles_out_len < sys::ZX_CHANNEL_MAX_MSG_HANDLES as usize
        {
            return Err(Status::BUFFER_TOO_SMALL);
        }

        let mut actual_read_bytes: u32 = 0;
        let mut actual_read_handles: u32 = 0;

        // SAFETY: pointer invariants are upheld by this method's caller, see Safety section in
        // docs. HandleDisposition is ABI-compatible with zx_handle_disposition_t and HandleInfo is
        // ABI-compatible with zx_handle_info_t, this allows the kernel to safely write the latter
        // and and for us to later interpret them as the former.
        let res = unsafe {
            ok(sys::zx_channel_call_etc(
                self.raw_handle(),
                0, // options
                timeout.into_nanos(),
                &mut sys::zx_channel_call_etc_args_t {
                    wr_bytes: bytes_in,
                    wr_num_bytes: bytes_in_len as u32,
                    wr_handles: handles_in as *mut sys::zx_handle_disposition_t,
                    wr_num_handles: handles_in_len as u32,
                    rd_bytes: bytes_out,
                    rd_num_bytes: bytes_out_len as u32,
                    rd_handles: handles_out as *mut sys::zx_handle_info_t,
                    rd_num_handles: handles_out_len as u32,
                },
                &mut actual_read_bytes,
                &mut actual_read_handles,
            ))
        };

        // Outgoing handles are consumed by zx_channel_call so prevent the destructor from being
        // called. Don't overwrite the status field so that callers can inspect it.
        // SAFETY: slice invariants must be upheld by this method's caller.
        let handles = unsafe { std::slice::from_raw_parts_mut(handles_in, handles_in_len) };
        for disposition in handles {
            std::mem::forget(disposition.take_op());
        }

        // Only error-return after zeroing out handles.
        res?;

        Ok((actual_read_bytes as usize, actual_read_handles as usize))
    }
}

impl AsRef<Channel> for Channel {
    fn as_ref(&self) -> &Self {
        &self
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum ChannelReadResult<T> {
    Ok(T),
    BufferTooSmall { bytes_avail: usize, handles_avail: usize },
    Err(Status),
}

impl<T: std::fmt::Debug> ChannelReadResult<T> {
    #[track_caller]
    pub fn unwrap(self) -> T {
        match self {
            Self::Ok(t) => t,
            other => panic!("unwrap() on {other:?}"),
        }
    }

    #[track_caller]
    pub fn expect(self, msg: &str) -> T {
        match self {
            Self::Ok(t) => t,
            other => panic!("expect() on {other:?}: {msg}"),
        }
    }
}

/// A buffer for _receiving_ messages from a channel.
///
/// A `MessageBuf` is essentially a byte buffer and a vector of
/// handles, but move semantics for "taking" handles requires special handling.
///
/// Note that for sending messages to a channel, the caller manages the buffers,
/// using a plain byte slice and `Vec<Handle>`.
#[derive(Debug, Default)]
pub struct MessageBuf {
    bytes: Vec<u8>,
    handles: Vec<Handle>,
}

impl MessageBuf {
    /// Create a new, empty, message buffer.
    pub fn new() -> Self {
        Default::default()
    }

    /// Create a new non-empty message buffer.
    pub fn new_with(v: Vec<u8>, h: Vec<Handle>) -> Self {
        Self { bytes: v, handles: h }
    }

    /// Splits apart the message buf into a vector of bytes and a vector of handles.
    pub fn split_mut(&mut self) -> (&mut Vec<u8>, &mut Vec<Handle>) {
        (&mut self.bytes, &mut self.handles)
    }

    /// Splits apart the message buf into a vector of bytes and a vector of handles.
    pub fn split(self) -> (Vec<u8>, Vec<Handle>) {
        (self.bytes, self.handles)
    }

    /// Ensure that the buffer has the capacity to hold at least `n_bytes` bytes.
    pub fn ensure_capacity_bytes(&mut self, n_bytes: usize) {
        ensure_capacity(&mut self.bytes, n_bytes);
    }

    /// Ensure that the buffer has the capacity to hold at least `n_handles` handles.
    pub fn ensure_capacity_handles(&mut self, n_handles: usize) {
        ensure_capacity(&mut self.handles, n_handles);
    }

    /// Ensure that at least n_bytes bytes are initialized (0 fill).
    pub fn ensure_initialized_bytes(&mut self, n_bytes: usize) {
        if n_bytes <= self.bytes.len() {
            return;
        }
        self.bytes.resize(n_bytes, 0);
    }

    /// Get a reference to the bytes of the message buffer, as a `&[u8]` slice.
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    /// The number of handles in the message buffer. Note this counts the number
    /// available when the message was received; `take_handle` does not affect
    /// the count.
    pub fn n_handles(&self) -> usize {
        self.handles.len()
    }

    /// Take the handle at the specified index from the message buffer. If the
    /// method is called again with the same index, it will return `None`, as
    /// will happen if the index exceeds the number of handles available.
    pub fn take_handle(&mut self, index: usize) -> Option<Handle> {
        self.handles.get_mut(index).and_then(|handle| {
            if handle.is_invalid() {
                None
            } else {
                Some(mem::replace(handle, Handle::invalid()))
            }
        })
    }

    /// Clear the bytes and handles contained in the buf. This will drop any
    /// contained handles, resulting in their resources being freed.
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.handles.clear();
    }
}

/// A buffer for _receiving_ messages from a channel.
///
/// This differs from `MessageBuf` in that it holds `HandleInfo` with
/// extended handle information.
///
/// A `MessageBufEtc` is essentially a byte buffer and a vector of handle
/// infos, but move semantics for "taking" handles requires special handling.
///
/// Note that for sending messages to a channel, the caller manages the buffers,
/// using a plain byte slice and `Vec<HandleDisposition>`.
#[derive(Debug, Default)]
pub struct MessageBufEtc {
    bytes: Vec<u8>,
    handle_infos: Vec<HandleInfo>,
}

impl MessageBufEtc {
    /// Create a new, empty, message buffer.
    pub fn new() -> Self {
        Default::default()
    }

    /// Create a new non-empty message buffer.
    pub fn new_with(v: Vec<u8>, h: Vec<HandleInfo>) -> Self {
        Self { bytes: v, handle_infos: h }
    }

    /// Splits apart the message buf into a vector of bytes and a vector of handle infos.
    pub fn split_mut(&mut self) -> (&mut Vec<u8>, &mut Vec<HandleInfo>) {
        (&mut self.bytes, &mut self.handle_infos)
    }

    /// Splits apart the message buf into a vector of bytes and a vector of handle infos.
    pub fn split(self) -> (Vec<u8>, Vec<HandleInfo>) {
        (self.bytes, self.handle_infos)
    }

    /// Ensure that the buffer has the capacity to hold at least `n_bytes` bytes.
    pub fn ensure_capacity_bytes(&mut self, n_bytes: usize) {
        ensure_capacity(&mut self.bytes, n_bytes);
    }

    /// Ensure that the buffer has the capacity to hold at least `n_handles` handle infos.
    pub fn ensure_capacity_handle_infos(&mut self, n_handle_infos: usize) {
        ensure_capacity(&mut self.handle_infos, n_handle_infos);
    }

    /// Ensure that at least n_bytes bytes are initialized (0 fill).
    pub fn ensure_initialized_bytes(&mut self, n_bytes: usize) {
        if n_bytes <= self.bytes.len() {
            return;
        }
        self.bytes.resize(n_bytes, 0);
    }

    /// Get a reference to the bytes of the message buffer, as a `&[u8]` slice.
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    /// The number of handles in the message buffer. Note this counts the number
    /// available when the message was received; `take_handle` does not affect
    /// the count.
    pub fn n_handle_infos(&self) -> usize {
        self.handle_infos.len()
    }

    /// Take the handle at the specified index from the message buffer. If the
    /// method is called again with the same index, it will return `None`, as
    /// will happen if the index exceeds the number of handles available.
    pub fn take_handle_info(&mut self, index: usize) -> Option<HandleInfo> {
        self.handle_infos.get_mut(index).and_then(|handle_info| {
            if handle_info.handle.is_invalid() {
                None
            } else {
                Some(mem::replace(
                    handle_info,
                    HandleInfo {
                        handle: Handle::invalid(),
                        object_type: ObjectType::NONE,
                        rights: Rights::NONE,
                        _unused: 0,
                    },
                ))
            }
        })
    }

    /// Clear the bytes and handles contained in the buf. This will drop any
    /// contained handles, resulting in their resources being freed.
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.handle_infos.clear();
    }
}

fn ensure_capacity<T>(vec: &mut Vec<T>, size: usize) {
    let len = vec.len();
    if size > len {
        vec.reserve(size - len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DurationNum, HandleOp, Port, Signals, Vmo};
    use std::thread;

    #[test]
    fn channel_basic() {
        let (p1, p2) = Channel::create();

        let mut empty = vec![];
        assert!(p1.write(b"hello", &mut empty).is_ok());

        let mut buf = MessageBuf::new();
        assert!(p2.read(&mut buf).is_ok());
        assert_eq!(buf.bytes(), b"hello");
    }

    #[test]
    fn channel_basic_etc() {
        let (p1, p2) = Channel::create();

        let mut empty = vec![];
        assert!(p1.write_etc(b"hello", &mut empty).is_ok());

        let mut buf = MessageBufEtc::new();
        assert!(p2.read_etc(&mut buf).is_ok());
        assert_eq!(buf.bytes(), b"hello");
    }

    #[test]
    fn channel_basic_etc_with_handle_move() {
        let (p1, p2) = Channel::create();

        let mut handles = vec![HandleDisposition::new(
            HandleOp::Move(Port::create().into()),
            ObjectType::PORT,
            Rights::TRANSFER,
            Status::OK,
        )];
        match p1.write_etc(b"", &mut handles) {
            Err(err) => {
                panic!("error: {}", err);
            }
            _ => {}
        }

        let mut buf = MessageBufEtc::new();
        assert!(p2.read_etc(&mut buf).is_ok());
        assert_eq!(buf.bytes(), b"");
        assert_eq!(buf.n_handle_infos(), 1);
        let out_handles = buf.handle_infos;
        assert_eq!(out_handles.len(), 1);
        assert_ne!(out_handles[0].handle, Handle::invalid());
        assert_eq!(out_handles[0].rights, Rights::TRANSFER);
        assert_eq!(out_handles[0].object_type, ObjectType::PORT);
    }

    #[test]
    fn channel_basic_etc_with_handle_duplicate() {
        let (p1, p2) = Channel::create();

        let port = Port::create();
        let mut handles = vec![HandleDisposition::new(
            HandleOp::Duplicate(port.as_handle_ref()),
            ObjectType::NONE,
            Rights::SAME_RIGHTS,
            Status::OK,
        )];
        p1.write_etc(b"", &mut handles).unwrap();

        let orig_port_info = port.basic_info().unwrap();
        let mut buf = MessageBufEtc::new();
        assert!(p2.read_etc(&mut buf).is_ok());
        assert_eq!(buf.bytes(), b"");
        assert_eq!(buf.n_handle_infos(), 1);
        let out_handles = buf.handle_infos;
        assert_eq!(out_handles.len(), 1);
        assert_ne!(out_handles[0].handle.raw_handle(), 0);
        assert_ne!(out_handles[0].handle.raw_handle(), port.raw_handle());
        assert_eq!(out_handles[0].rights, orig_port_info.rights);
        assert_eq!(out_handles[0].object_type, ObjectType::PORT);
    }

    #[test]
    fn channel_read_raw_too_small() {
        let (p1, p2) = Channel::create();

        let mut empty = vec![];
        assert!(p1.write(b"hello", &mut empty).is_ok());

        let result = p2.read_uninit(&mut vec![], &mut vec![]);
        assert_eq!(result, ChannelReadResult::BufferTooSmall { bytes_avail: 5, handles_avail: 0 });
    }

    #[test]
    fn channel_read_etc_raw_too_small() {
        let (p1, p2) = Channel::create();

        let mut empty = vec![];
        assert!(p1.write_etc(b"hello", &mut empty).is_ok());

        let result = p2.read_etc_uninit(&mut vec![], &mut vec![]);
        assert_eq!(result, ChannelReadResult::BufferTooSmall { bytes_avail: 5, handles_avail: 0 });
    }

    fn too_many_bytes() -> Vec<u8> {
        vec![b'A'; (sys::ZX_CHANNEL_MAX_MSG_BYTES + 1) as usize]
    }

    fn too_many_handles() -> Vec<Handle> {
        let mut handles = vec![];
        for _ in 0..sys::ZX_CHANNEL_MAX_MSG_HANDLES + 1 {
            handles.push(crate::Event::create().into());
        }
        handles
    }

    fn too_many_dispositions() -> Vec<HandleDisposition<'static>> {
        let mut handles = vec![];
        for _ in 0..sys::ZX_CHANNEL_MAX_MSG_HANDLES + 1 {
            handles.push(HandleDisposition::new(
                HandleOp::Move(crate::Event::create().into()),
                ObjectType::EVENT,
                Rights::TRANSFER,
                Status::OK,
            ));
        }
        handles
    }

    #[test]
    fn channel_write_too_many_bytes() {
        Channel::create().0.write(&too_many_bytes(), &mut vec![]).unwrap_err();
    }

    #[test]
    fn channel_write_too_many_handles() {
        Channel::create().0.write(&vec![], &mut too_many_handles()[..]).unwrap_err();
    }

    #[test]
    fn channel_write_consumes_handles_on_failure() {
        let (send, recv) = Channel::create();
        drop(recv);
        let mut handles = vec![crate::Event::create().into()];
        send.write(&[], &mut handles).unwrap_err();
        assert!(handles[0].is_invalid());
    }

    #[test]
    fn channel_write_etc_too_many_bytes() {
        Channel::create().0.write_etc(&too_many_bytes(), &mut []).unwrap_err();
    }

    #[test]
    fn channel_write_etc_too_many_handles() {
        Channel::create().0.write_etc(&vec![], &mut too_many_dispositions()[..]).unwrap_err();
    }

    #[test]
    fn channel_write_etc_consumes_moved_handles_on_failure() {
        let (send, recv) = Channel::create();
        drop(recv);
        let mut handles = vec![HandleDisposition::new(
            HandleOp::Move(crate::Event::create().into()),
            ObjectType::EVENT,
            Rights::NONE,
            Status::OK,
        )];
        send.write_etc(&[], &mut handles).unwrap_err();
        assert_eq!(handles[0].raw_handle(), sys::ZX_HANDLE_INVALID);
        assert_eq!(handles[0].result, Status::OK);
    }

    #[test]
    fn channel_write_etc_preserves_per_disposition_failures() {
        let (send, _recv) = Channel::create();

        let event = crate::Event::create();
        let event_no_rights = event.duplicate_handle(Rights::NONE).unwrap();

        let mut handles = vec![
            HandleDisposition::new(
                HandleOp::Move(event.into()),
                ObjectType::EVENT,
                Rights::SAME_RIGHTS,
                Status::OK,
            ),
            HandleDisposition::new(
                HandleOp::Move(event_no_rights.into()),
                ObjectType::EVENT,
                Rights::SAME_RIGHTS,
                Status::OK,
            ),
        ];

        send.write_etc(&[], &mut handles).unwrap_err();

        // Both handles should be moved.
        assert_eq!(handles[0].raw_handle(), sys::ZX_HANDLE_INVALID);
        assert_eq!(handles[1].raw_handle(), sys::ZX_HANDLE_INVALID);

        // Each handle should separately report the status of transferring/duplicating that handle.
        assert_eq!(handles[0].result, Status::OK);
        assert_ne!(handles[1].result, Status::OK, "must have transfer rights to succeed");
    }

    #[test]
    fn channel_call_too_many_bytes() {
        Channel::create()
            .0
            .call(MonotonicTime::INFINITE, &too_many_bytes(), &mut vec![], &mut MessageBuf::new())
            .unwrap_err();
    }

    #[test]
    fn channel_call_too_many_handles() {
        Channel::create()
            .0
            .call(
                MonotonicTime::INFINITE,
                &vec![],
                &mut too_many_handles()[..],
                &mut MessageBuf::new(),
            )
            .unwrap_err();
    }

    #[test]
    fn channel_call_etc_too_many_bytes() {
        Channel::create()
            .0
            .call_etc(
                MonotonicTime::INFINITE,
                &too_many_bytes(),
                &mut vec![],
                &mut MessageBufEtc::new(),
            )
            .unwrap_err();
    }

    #[test]
    fn channel_call_etc_too_many_handles() {
        Channel::create()
            .0
            .call_etc(
                MonotonicTime::INFINITE,
                &vec![],
                &mut too_many_dispositions()[..],
                &mut MessageBufEtc::new(),
            )
            .unwrap_err();
    }

    #[test]
    fn channel_send_handle() {
        let hello_length: usize = 5;

        // Create a pair of channels and a virtual memory object.
        let (p1, p2) = Channel::create();
        let vmo = Vmo::create(hello_length as u64).unwrap();

        // Duplicate VMO handle and send it down the channel.
        let duplicate_vmo_handle = vmo.duplicate_handle(Rights::SAME_RIGHTS).unwrap().into();
        let mut handles_to_send: Vec<Handle> = vec![duplicate_vmo_handle];
        assert!(p1.write(b"", &mut handles_to_send).is_ok());
        // The handle vector should only contain invalid handles.
        for handle in handles_to_send {
            assert!(handle.is_invalid());
        }

        // Read the handle from the receiving channel.
        let mut buf = MessageBuf::new();
        assert!(p2.read(&mut buf).is_ok());
        assert_eq!(buf.n_handles(), 1);
        // Take the handle from the buffer.
        let received_handle = buf.take_handle(0).unwrap();
        // Should not affect number of handles.
        assert_eq!(buf.n_handles(), 1);
        // Trying to take it again should fail.
        assert!(buf.take_handle(0).is_none());

        // Now to test that we got the right handle, try writing something to it...
        let received_vmo = Vmo::from(received_handle);
        assert!(received_vmo.write(b"hello", 0).is_ok());

        // ... and reading it back from the original VMO.
        let mut read_vec = vec![0; hello_length];
        assert!(vmo.read(&mut read_vec, 0).is_ok());
        assert_eq!(read_vec, b"hello");
    }

    #[test]
    fn channel_call_timeout() {
        let ten_ms = 10.millis();

        // Create a pair of channels and a virtual memory object.
        let (p1, p2) = Channel::create();
        let vmo = Vmo::create(0 as u64).unwrap();

        // Duplicate VMO handle and send it along with the call.
        let duplicate_vmo_handle = vmo.duplicate_handle(Rights::SAME_RIGHTS).unwrap().into();
        let mut handles_to_send: Vec<Handle> = vec![duplicate_vmo_handle];
        let mut buf = MessageBuf::new();
        assert_eq!(
            p1.call(MonotonicTime::after(ten_ms), b"0000call", &mut handles_to_send, &mut buf),
            Err(Status::TIMED_OUT)
        );
        // Despite not getting a response, the handles were sent so the handle slice
        // should only contain invalid handles.
        for handle in handles_to_send {
            assert!(handle.is_invalid());
        }

        // Should be able to read call even though it timed out waiting for a response.
        let mut buf = MessageBuf::new();
        assert!(p2.read(&mut buf).is_ok());
        assert_eq!(&buf.bytes()[4..], b"call");
        assert_eq!(buf.n_handles(), 1);
    }

    #[test]
    fn channel_call_etc_timeout() {
        let ten_ms = 10.millis();

        // Create a pair of channels and a virtual memory object.
        let (p1, p2) = Channel::create();

        // Duplicate VMO handle and send it along with the call.
        let mut empty: Vec<HandleDisposition<'_>> = vec![];
        let mut buf = MessageBufEtc::new();
        assert_eq!(
            p1.call_etc(MonotonicTime::after(ten_ms), b"0000call", &mut empty, &mut buf),
            Err(Status::TIMED_OUT)
        );

        // Should be able to read call even though it timed out waiting for a response.
        let mut buf = MessageBuf::new();
        assert!(p2.read(&mut buf).is_ok());
        assert_eq!(&buf.bytes()[4..], b"call");
        assert_eq!(buf.n_handles(), 0);
    }

    #[test]
    fn channel_call() {
        // Create a pair of channels
        let (p1, p2) = Channel::create();

        // create an mpsc channel for communicating the call data for later assertion
        let (tx, rx) = ::std::sync::mpsc::channel();

        // Start a new thread to respond to the call.
        thread::spawn(move || {
            let mut buf = MessageBuf::new();
            // if either the read or the write fail, this thread will panic,
            // resulting in tx being dropped, which will be noticed by the rx.
            p2.wait_handle(Signals::CHANNEL_READABLE, MonotonicTime::after(1.seconds()))
                .expect("callee wait error");
            p2.read(&mut buf).expect("callee read error");

            let (bytes, handles) = buf.split_mut();
            tx.send(bytes.clone()).expect("callee mpsc send error");
            assert_eq!(handles.len(), 0);

            bytes.truncate(4); // Drop the received message, leaving only the txid
            bytes.extend_from_slice(b"response");

            p2.write(bytes, handles).expect("callee write error");
        });

        // Make the call.
        let mut buf = MessageBuf::new();
        buf.ensure_capacity_bytes(12);
        // NOTE(raggi): CQ has been seeing some long stalls from channel call,
        // and it's as yet unclear why. The timeout here has been made much
        // larger in order to avoid that, as the issues are not issues with this
        // crate's concerns. The timeout is here just to prevent the tests from
        // stalling forever if a developer makes a mistake locally in this
        // crate. Tests of Zircon behavior or virtualization behavior should be
        // covered elsewhere. See https://fxbug.dev/42106187.
        p1.call(MonotonicTime::after(30.seconds()), b"txidcall", &mut vec![], &mut buf)
            .expect("channel call error");
        assert_eq!(&buf.bytes()[4..], b"response");
        assert_eq!(buf.n_handles(), 0);

        let sbuf = rx.recv().expect("mpsc channel recv error");
        assert_eq!(&sbuf[4..], b"call");
    }

    #[test]
    fn channel_call_etc() {
        // Create a pair of channels
        let (p1, p2) = Channel::create();

        // create an mpsc channel for communicating the call data for later assertion
        let (tx, rx) = ::std::sync::mpsc::channel();

        // Start a new thread to respond to the call.
        thread::spawn(move || {
            let mut buf = MessageBuf::new();
            // if either the read or the write fail, this thread will panic,
            // resulting in tx being dropped, which will be noticed by the rx.
            p2.wait_handle(Signals::CHANNEL_READABLE, MonotonicTime::after(1.seconds()))
                .expect("callee wait error");
            p2.read(&mut buf).expect("callee read error");

            let (bytes, handles) = buf.split_mut();
            tx.send(bytes.clone()).expect("callee mpsc send error");
            assert_eq!(handles.len(), 1);

            bytes.truncate(4); // Drop the received message, leaving only the txid
            bytes.extend_from_slice(b"response");

            p2.write(bytes, handles).expect("callee write error");
        });

        // Make the call.
        let mut buf = MessageBufEtc::new();
        buf.ensure_capacity_bytes(12);
        buf.ensure_capacity_handle_infos(1);
        let mut handle_dispositions = [HandleDisposition::new(
            HandleOp::Move(Port::create().into()),
            ObjectType::PORT,
            Rights::TRANSFER,
            Status::OK,
        )];
        // NOTE(raggi): CQ has been seeing some long stalls from channel call,
        // and it's as yet unclear why. The timeout here has been made much
        // larger in order to avoid that, as the issues are not issues with this
        // crate's concerns. The timeout is here just to prevent the tests from
        // stalling forever if a developer makes a mistake locally in this
        // crate. Tests of Zircon behavior or virtualization behavior should be
        // covered elsewhere. See https://fxbug.dev/42106187.
        p1.call_etc(
            MonotonicTime::after(30.seconds()),
            b"txidcall",
            &mut handle_dispositions,
            &mut buf,
        )
        .expect("channel call error");
        assert_eq!(&buf.bytes()[4..], b"response");
        assert_eq!(buf.n_handle_infos(), 1);
        assert_ne!(buf.handle_infos[0].handle.raw_handle(), 0);
        assert_eq!(buf.handle_infos[0].object_type, ObjectType::PORT);
        assert_eq!(buf.handle_infos[0].rights, Rights::TRANSFER);

        let sbuf = rx.recv().expect("mpsc channel recv error");
        assert_eq!(&sbuf[4..], b"call");
    }

    #[test]
    fn channel_call_etc_preserves_per_disposition_failures() {
        let (send, _recv) = Channel::create();

        let event = crate::Event::create();
        let event_no_rights = event.duplicate_handle(Rights::NONE).unwrap();

        let mut handles = vec![
            HandleDisposition::new(
                HandleOp::Move(event.into()),
                ObjectType::EVENT,
                Rights::SAME_RIGHTS,
                Status::OK,
            ),
            HandleDisposition::new(
                HandleOp::Move(event_no_rights.into()),
                ObjectType::EVENT,
                Rights::SAME_RIGHTS,
                Status::OK,
            ),
        ];

        send.call_etc(
            MonotonicTime::INFINITE,
            &[0, 0, 0, 0],
            &mut handles,
            &mut MessageBufEtc::default(),
        )
        .unwrap_err();

        // Both handles should be invalidated.
        assert_eq!(handles[0].raw_handle(), sys::ZX_HANDLE_INVALID);
        assert_eq!(handles[1].raw_handle(), sys::ZX_HANDLE_INVALID);

        // Each handle should separately report the status of transferring/duplicating that handle.
        assert_eq!(handles[0].result, Status::OK);
        assert_ne!(handles[1].result, Status::OK, "must have duplicate rights to succeed");
    }
}
