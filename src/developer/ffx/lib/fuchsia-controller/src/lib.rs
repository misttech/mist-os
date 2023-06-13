// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// LINT.IfChange

// Given this is exposing a C ABI, it is clear that most of the functionality is going to be
// accessing raw pointers unsafely. This disables the individual unsafe function checks for a
// `# Safety` section.
#![allow(clippy::missing_safety_doc)]
// Shows more granularity over unsafe operations inside unsafe functions.
#![deny(unsafe_op_in_unsafe_fn)]

use crate::commands::{LibraryCommand, ReadResponse};
use crate::env_context::EnvContext;
use crate::ext_buffer::ExtBuffer;
use crate::lib_context::LibContext;
use fidl::HandleBased;
use fuchsia_zircon_status as zx_status;
use fuchsia_zircon_types as zx_types;
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::sync::{mpsc, Arc};

mod commands;
mod env_context;
mod ext_buffer;
mod lib_context;
mod waker;

#[no_mangle]
pub unsafe extern "C" fn create_ffx_lib_context(
    ctx: *mut *const LibContext,
    error_scratch: *mut u8,
    len: u64,
) {
    let buf = ExtBuffer::new(error_scratch, len as usize);
    let ctx_out = LibContext::new(buf);
    let ptr = Arc::into_raw(Arc::new(ctx_out));

    unsafe { *ctx = ptr };
}

#[derive(Debug)]
#[repr(C)]
pub struct FfxConfig {
    pub key: *const i8,
    pub value: *const i8,
}

unsafe fn get_arc<T>(ptr: *const T) -> Arc<T> {
    unsafe { Arc::increment_strong_count(ptr) };
    unsafe { Arc::from_raw(ptr) }
}

#[no_mangle]
pub unsafe extern "C" fn create_ffx_env_context(
    env_ctx: *mut *const EnvContext,
    lib_ctx: *const LibContext,
    _config: *mut FfxConfig,
    _config_len: u64,
) -> zx_status::Status {
    let lib = unsafe { get_arc(lib_ctx) };
    let (responder, rx) = mpsc::sync_channel(1);
    lib.run(LibraryCommand::CreateEnvContext { lib: lib.clone(), responder });
    match rx.recv().unwrap() {
        Ok(env) => {
            unsafe { *env_ctx = Arc::into_raw(env) };
            zx_status::Status::OK
        }
        Err(e) => e,
    }
}

#[no_mangle]
pub unsafe extern "C" fn ffx_connect_daemon_protocol(
    ctx: *mut EnvContext,
    protocol: *const i8,
    handle: *mut zx_types::zx_handle_t,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let protocol =
        unsafe { CStr::from_ptr(protocol) }.to_str().expect("valid protocol string").to_owned();
    let (responder, rx) = mpsc::sync_channel(1);
    ctx.lib_ctx().run(LibraryCommand::OpenDaemonProtocol { env: ctx.clone(), protocol, responder });
    match rx.recv().unwrap() {
        Ok(r) => {
            unsafe { *handle = r };
            zx_status::Status::OK
        }
        Err(e) => e,
    }
}

#[no_mangle]
pub unsafe extern "C" fn ffx_connect_device_proxy(
    ctx: *mut EnvContext,
    moniker: *const i8,
    capability_name: *const i8,
    handle: *mut zx_types::zx_handle_t,
) -> zx_status::Status {
    let moniker = unsafe { CStr::from_ptr(moniker) }.to_str().expect("valid moniker").to_owned();
    let capability_name = unsafe { CStr::from_ptr(capability_name) }
        .to_str()
        .expect("valid capability name")
        .to_owned();
    let ctx = unsafe { get_arc(ctx) };
    let (responder, rx) = mpsc::sync_channel(1);
    ctx.lib_ctx().run(LibraryCommand::OpenDeviceProxy {
        env: ctx.clone(),
        moniker,
        capability_name,
        responder,
    });
    match rx.recv().unwrap() {
        Ok(h) => {
            unsafe { *handle = h };
            zx_status::Status::OK
        }
        Err(e) => e,
    }
}

/// This function isn't really necessary. It can be opened via connect_daemon_protocol from the
/// target.
#[no_mangle]
pub unsafe extern "C" fn ffx_connect_target_proxy(
    ctx: *mut EnvContext,
    handle: *mut zx_types::zx_handle_t,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (responder, rx) = mpsc::sync_channel(1);
    ctx.lib_ctx().run(LibraryCommand::OpenTargetProxy { env: ctx.clone(), responder });
    match rx.recv().unwrap() {
        Ok(h) => {
            unsafe { *handle = h };
            zx_status::Status::OK
        }
        Err(e) => e,
    }
}

#[no_mangle]
pub unsafe extern "C" fn ffx_connect_remote_control_proxy(
    ctx: *mut EnvContext,
    handle: *mut zx_types::zx_handle_t,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (responder, rx) = mpsc::sync_channel(1);
    ctx.lib_ctx().run(LibraryCommand::OpenRemoteControlProxy { env: ctx.clone(), responder });
    match rx.recv().unwrap() {
        Ok(h) => {
            unsafe { *handle = h };
            zx_status::Status::OK
        }
        Err(e) => e,
    }
}

#[no_mangle]
pub unsafe extern "C" fn destroy_ffx_lib_context(ctx: *const LibContext) {
    if ctx != std::ptr::null() {
        let ctx = unsafe { Arc::from_raw(ctx) };
        ctx.shutdown_cmd_thread();
    }
}

#[no_mangle]
pub unsafe extern "C" fn destroy_ffx_env_context(ctx: *const EnvContext) {
    if ctx != std::ptr::null() {
        drop(unsafe { Arc::from_raw(ctx) });
    }
}

#[no_mangle]
pub unsafe extern "C" fn ffx_close_handle(hdl: zx_types::zx_handle_t) {
    drop(unsafe { fidl::Handle::from_raw(hdl) });
}

fn safe_write<T>(dest: *mut T, value: T) {
    let dest = unsafe { dest.as_mut() };
    dest.map(|d| *d = value);
}

#[no_mangle]
pub unsafe extern "C" fn ffx_channel_write(
    ctx: *const LibContext,
    handle: zx_types::zx_handle_t,
    out_buf: *mut u8,
    out_len: u64,
    hdls: *mut zx_types::zx_handle_t,
    hdls_len: u64,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (responder, rx) = mpsc::sync_channel(1);
    let handle = unsafe { fidl::Handle::from_raw(handle) };
    let channel = fidl::Channel::from_handle(handle);
    ctx.run(LibraryCommand::ChannelWrite {
        channel,
        buf: ExtBuffer::new(out_buf, out_len as usize),
        handles: ExtBuffer::new(hdls as *mut fidl::Handle, hdls_len as usize),
        responder,
    });
    rx.recv().unwrap()
}

#[no_mangle]
pub unsafe extern "C" fn ffx_channel_read(
    ctx: *const LibContext,
    handle: zx_types::zx_handle_t,
    out_buf: *mut u8,
    out_len: u64,
    hdls: *mut zx_types::zx_handle_t,
    hdls_len: u64,
    actual_bytes_count: *mut u64,
    actual_hdls_count: *mut u64,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (responder, rx) = mpsc::sync_channel(1);
    let handle = unsafe { fidl::Handle::from_raw(handle) };
    let channel = fidl::Channel::from_handle(handle);
    ctx.run(LibraryCommand::ChannelRead {
        lib: ctx.clone(),
        channel,
        out_buf: ExtBuffer::new(out_buf, out_len as usize),
        out_handles: ExtBuffer::new(hdls as *mut MaybeUninit<fidl::Handle>, hdls_len as usize),
        responder,
    });
    let ReadResponse {
        actual_bytes_count: bytes_count_recv,
        actual_handles_count: handles_count_recv,
        result,
    } = rx.recv().unwrap();
    safe_write(actual_bytes_count, bytes_count_recv as u64);
    safe_write(actual_hdls_count, handles_count_recv as u64);
    result
}

#[no_mangle]
pub unsafe extern "C" fn ffx_socket_write(
    ctx: *const LibContext,
    handle: zx_types::zx_handle_t,
    buf: *mut u8,
    buf_len: u64,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (responder, rx) = mpsc::sync_channel(1);
    let handle = unsafe { fidl::Handle::from_raw(handle) };
    let socket = fidl::Socket::from_handle(handle);
    ctx.run(LibraryCommand::SocketWrite {
        socket,
        buf: ExtBuffer::new(buf, buf_len as usize),
        responder,
    });
    rx.recv().unwrap()
}

#[no_mangle]
pub unsafe extern "C" fn ffx_socket_read(
    ctx: *const LibContext,
    handle: zx_types::zx_handle_t,
    out_buf: *mut u8,
    out_len: u64,
    bytes_read: *mut u64,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (responder, rx) = mpsc::sync_channel(1);
    let handle = unsafe { fidl::Handle::from_raw(handle) };
    let socket = fidl::Socket::from_handle(handle);
    ctx.run(LibraryCommand::SocketRead {
        lib: ctx.clone(),
        socket,
        out_buf: ExtBuffer::new(out_buf, out_len as usize),
        responder,
    });
    let ReadResponse { actual_bytes_count: bytes_count_recv, result, .. } = rx.recv().unwrap();
    safe_write(bytes_read, bytes_count_recv as u64);
    result
}

#[no_mangle]
pub unsafe extern "C" fn ffx_connect_handle_notifier(ctx: *const LibContext) -> i32 {
    let ctx = unsafe { get_arc(ctx) };
    let (tx, rx) = mpsc::sync_channel(1);
    ctx.run(LibraryCommand::GetNotificationDescriptor { lib: ctx.clone(), responder: tx });
    rx.recv().unwrap()
}

#[cfg(test)]
mod test {
    use super::*;
    use byteorder::{NativeEndian, ReadBytesExt};
    use fidl::AsHandleRef;
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{FromRawFd, RawFd};

    static mut SCRATCH: [u8; 1024] = [0; 1024];
    fn testing_lib_context() -> *const LibContext {
        // SAFETY: This is unsafe because it is a static location, which can
        // then be potentially accessed by multiple threads. So far this is not
        // actually read by anything and any data clobbering should not be an
        // issue. If it comes to the point that these values need to be read in
        // the tests, this must be changed so that each data buffer is declared
        // in each individual test (either that or just a re-design of the
        // library context).
        let raw = unsafe { &mut SCRATCH as *mut u8 };
        let mut ctx: *const LibContext = std::ptr::null_mut();
        unsafe {
            create_ffx_lib_context(&mut ctx, raw, 1024);
        }
        ctx
    }

    #[test]
    fn channel_read_empty() {
        let lib_ctx = testing_lib_context();
        let (a, _b) = fidl::Channel::create();
        let mut buf = [0u8; 2];
        let mut handles = [0u32; 2];
        let result = unsafe {
            ffx_channel_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                handles.as_mut_ptr(),
                handles.len() as u64,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, zx_status::Status::SHOULD_WAIT);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn socket_read_empty() {
        let lib_ctx = testing_lib_context();
        let (a, _b) = fidl::Socket::create_stream();
        let mut buf = [0u8; 2];
        let result = unsafe {
            ffx_socket_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, zx_status::Status::SHOULD_WAIT);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn channel_read_some_data_null_out_params() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Channel::create();
        let (c, d) = fidl::Channel::create();
        let mut buf = [0u8; 2];
        let mut handles = [0u32; 2];
        let c_handle = c.raw_handle();
        let d_handle = d.raw_handle();
        b.write(&[1, 2], &mut vec![c.into(), d.into()]).unwrap();
        let result = unsafe {
            ffx_channel_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                handles.as_mut_ptr(),
                handles.len() as u64,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        assert_eq!(&buf, &[1, 2]);
        assert_eq!(&handles, &[c_handle, d_handle]);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn channel_read_some_data_too_small_byte_buffer() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Channel::create();
        let (c, d) = fidl::Channel::create();
        let mut buf = [0u8; 1];
        let mut handles = [0u32; 2];
        b.write(&[1, 2], &mut vec![c.into(), d.into()]).unwrap();
        let result = unsafe {
            ffx_channel_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                handles.as_mut_ptr(),
                handles.len() as u64,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, zx_status::Status::BUFFER_TOO_SMALL);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn socket_read_some_data_too_large_byte_buffer() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Socket::create_stream();
        let mut buf = [0u8; 3];
        b.write(&[1, 2]).unwrap();
        let mut bytes_len = 0u64;
        let result = unsafe {
            ffx_socket_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                &mut bytes_len,
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        assert_eq!(bytes_len, 2);
        assert_eq!(&buf[0..2], &[1, 2]);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn channel_read_some_data_too_small_handle_buffer() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Channel::create();
        let (c, d) = fidl::Channel::create();
        let mut buf = [0u8; 2];
        let mut handles = [0u32; 1];
        let mut read_bytes = 0;
        let mut read_handles = 0;
        b.write(&[1, 2], &mut vec![c.into(), d.into()]).unwrap();
        let result = unsafe {
            ffx_channel_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                handles.as_mut_ptr(),
                handles.len() as u64,
                &mut read_bytes,
                &mut read_handles,
            )
        };
        assert_eq!(read_bytes, 2);
        assert_eq!(read_handles, 2);
        assert_eq!(result, zx_status::Status::BUFFER_TOO_SMALL);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn channel_read_some_data_nonnull_out_params() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Channel::create();
        let (c, d) = fidl::Channel::create();
        let mut buf = [0u8; 2];
        let mut handles = [0u32; 2];
        let c_handle = c.raw_handle();
        let d_handle = d.raw_handle();
        b.write(&[1, 2], &mut vec![c.into(), d.into()]).unwrap();
        let mut read_bytes = 0;
        let mut read_handles = 0;
        let result = unsafe {
            ffx_channel_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                handles.as_mut_ptr(),
                handles.len() as u64,
                &mut read_bytes,
                &mut read_handles,
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        assert_eq!(read_bytes, 2);
        assert_eq!(read_handles, 2);
        assert_eq!(&buf, &[1, 2]);
        assert_eq!(&handles, &[c_handle, d_handle]);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn channel_write_then_read_some_data() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Channel::create();
        let (c, d) = fidl::Channel::create();
        let c_handle = c.raw_handle();
        let d_handle = d.raw_handle();
        let mut write_buf = [1u8, 2u8];
        let mut handles_buf: [fidl::Handle; 2] = [c.into(), d.into()];
        let result = unsafe {
            ffx_channel_write(
                lib_ctx,
                b.raw_handle(),
                write_buf.as_mut_ptr(),
                2,
                std::mem::transmute(handles_buf.as_mut_ptr()),
                2,
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        let mut buf = [0u8; 2];
        let mut handles = [0u32; 2];
        let result = unsafe {
            ffx_channel_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                handles.as_mut_ptr(),
                handles.len() as u64,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        assert_eq!(&buf, &[1, 2]);
        assert_eq!(&handles, &[c_handle, d_handle]);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn socket_write_then_read_some_data() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Socket::create_stream();
        let mut write_buf = [1u8, 2u8];
        let result = unsafe {
            ffx_socket_write(
                lib_ctx,
                b.raw_handle(),
                write_buf.as_mut_ptr(),
                write_buf.len() as u64,
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        let mut buf = [0u8; 2];
        let result = unsafe {
            ffx_socket_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        assert_eq!(&buf, &[1, 2]);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn channel_read_peer_closed() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Channel::create();
        drop(b);
        let mut buf = [0u8; 2];
        let mut handles = [0u32; 2];
        let result = unsafe {
            ffx_channel_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                handles.as_mut_ptr(),
                handles.len() as u64,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, zx_status::Status::PEER_CLOSED);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn channel_write_peer_closed() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Channel::create();
        drop(b);
        let mut buf = [0u8; 2];
        let mut handles = [0u32; 2];
        let result = unsafe {
            ffx_channel_write(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                handles.as_mut_ptr(),
                handles.len() as u64,
            )
        };
        assert_eq!(result, zx_status::Status::PEER_CLOSED);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn socket_read_peer_closed() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Socket::create_datagram();
        drop(b);
        let mut buf = [0u8];
        let result = unsafe {
            ffx_socket_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, zx_status::Status::PEER_CLOSED);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn socket_write_peer_closed() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Socket::create_stream();
        drop(b);
        let mut buf = [0u8];
        let result = unsafe {
            ffx_socket_write(lib_ctx, a.raw_handle(), buf.as_mut_ptr(), buf.len() as u64)
        };
        assert_eq!(result, zx_status::Status::PEER_CLOSED);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn handle_ready_notification() {
        let lib_ctx = testing_lib_context();
        let fd: RawFd = unsafe { ffx_connect_handle_notifier(lib_ctx) };
        assert!(fd > 0);
        let mut notifier_file = unsafe { File::from_raw_fd(fd) };
        let (a, b) = fidl::Channel::create();
        let mut buf = [0u8; 2];
        let mut handles = [0u32; 2];
        let result = unsafe {
            ffx_channel_read(
                lib_ctx,
                a.raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u64,
                handles.as_mut_ptr(),
                handles.len() as u64,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, zx_status::Status::SHOULD_WAIT);
        let (c, d) = fidl::Channel::create();
        let mut write_buf = [1u8, 2u8];
        let mut handles_buf: [fidl::Handle; 2] = [c.into(), d.into()];
        let result = unsafe {
            ffx_channel_write(
                lib_ctx,
                b.raw_handle(),
                write_buf.as_mut_ptr(),
                2,
                std::mem::transmute(handles_buf.as_mut_ptr()),
                2,
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        let mut notifier_buf = [0u8; 4];
        let bytes_read = notifier_file.read(&mut notifier_buf).unwrap();
        let mut notifier_buf_reader = std::io::Cursor::new(notifier_buf);
        assert_eq!(bytes_read, 4);
        let read_handle = notifier_buf_reader.read_u32::<NativeEndian>().unwrap();
        assert_eq!(read_handle, a.raw_handle());
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }
}

// LINT.ThenChange(../abi/fuchsia_controller.h)
