// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Given this is exposing a C ABI, it is clear that most of the functionality is going to be
// accessing raw pointers unsafely. This disables the individual unsafe function checks for a
// `# Safety` section.
#![allow(clippy::missing_safety_doc)]
// Shows more granularity over unsafe operations inside unsafe functions.
#![deny(unsafe_op_in_unsafe_fn)]

use crate::commands::{LibraryCommand, ReadResponse};
use crate::env_context::{EnvContext, FfxConfigEntry};
use crate::ext_buffer::ExtBuffer;
use crate::lib_context::LibContext;
use fidl::HandleBased;
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use {zx_status, zx_types};

mod commands;
mod env_context;
mod ext_buffer;
mod lib_context;
mod waker;

// LINT.IfChange

#[no_mangle]
pub unsafe extern "C" fn create_ffx_lib_context(
    ctx: *mut *const LibContext,
    error_scratch: *mut u8,
    len: u64,
) {
    let buf = unsafe { ExtBuffer::new(error_scratch, len as usize) };
    let ctx_out = LibContext::new(buf);
    let ptr = Arc::into_raw(Arc::new(ctx_out));

    unsafe { *ctx = ptr };
}

#[derive(Debug)]
#[repr(C)]
pub struct FfxExternalConfigEntry {
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
    external_config: *const FfxExternalConfigEntry,
    config_len: u64,
    isolate_dir: *const i8,
) -> zx_status::Status {
    let lib = unsafe { get_arc(lib_ctx) };
    let isolate_dir = unsafe { isolate_dir.as_ref() }.map(|i| {
        PathBuf::from(unsafe {
            CStr::from_ptr(i).to_str().expect("value isolate dir string").to_owned()
        })
    });
    let (responder, rx) = mpsc::sync_channel(1);
    let mut config = Vec::new();
    if external_config != std::ptr::null_mut() {
        for i in 0..TryInto::<isize>::try_into(config_len).unwrap() {
            let config_entry: &FfxExternalConfigEntry = unsafe { &*external_config.offset(i) };
            let key = unsafe {
                CStr::from_ptr(config_entry.key).to_str().expect("valid config string").to_owned()
            };
            let value = unsafe {
                CStr::from_ptr(config_entry.value).to_str().expect("valid config string").to_owned()
            };
            config.push(FfxConfigEntry { key, value });
        }
    }
    lib.run(LibraryCommand::CreateEnvContext { lib: lib.clone(), responder, config, isolate_dir });
    match rx.recv().unwrap() {
        Ok(env) => {
            unsafe { *env_ctx = Arc::into_raw(env) };
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

#[no_mangle]
pub unsafe extern "C" fn ffx_target_wait(
    ctx: *mut EnvContext,
    timeout: u64,
    offline: bool,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (responder, rx) = mpsc::sync_channel(1);
    ctx.lib_ctx().run(LibraryCommand::TargetWait { env: ctx.clone(), timeout, responder, offline });
    rx.recv().unwrap()
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
        buf: unsafe { ExtBuffer::new(out_buf, out_len as usize) },
        handles: unsafe { ExtBuffer::new(hdls as *mut fidl::Handle, hdls_len as usize) },
        responder,
    });
    rx.recv().unwrap()
}

#[no_mangle]
pub unsafe extern "C" fn ffx_channel_write_etc(
    ctx: *const LibContext,
    handle: zx_types::zx_handle_t,
    out_buf: *mut u8,
    out_len: u64,
    hdls: *mut zx_types::zx_handle_disposition_t,
    hdls_len: u64,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (responder, rx) = mpsc::sync_channel(1);
    let handle = unsafe { fidl::Handle::from_raw(handle) };
    let channel = fidl::Channel::from_handle(handle);
    ctx.run(LibraryCommand::ChannelWriteEtc {
        channel,
        buf: unsafe { ExtBuffer::new(out_buf, out_len as usize) },
        // Construction of HandleDisposition structs has to happen in the main thread, as it
        // contains a lifetime bound.
        handles: unsafe { ExtBuffer::new(hdls, hdls_len as usize) },
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
        out_buf: unsafe { ExtBuffer::new(out_buf, out_len as usize) },
        out_handles: unsafe {
            ExtBuffer::new(hdls as *mut MaybeUninit<fidl::Handle>, hdls_len as usize)
        },
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
pub unsafe extern "C" fn ffx_socket_create(
    ctx: *const LibContext,
    options: u32,
    out0: *mut zx_types::zx_handle_t,
    out1: *mut zx_types::zx_handle_t,
) -> zx_status::Status {
    let socket_opts = match options {
        zx_types::ZX_SOCKET_STREAM => fidl::SocketOpts::STREAM,
        zx_types::ZX_SOCKET_DATAGRAM => fidl::SocketOpts::DATAGRAM,
        _ => return zx_status::Status::INVALID_ARGS,
    };
    let ctx = unsafe { get_arc(ctx) };
    let (tx, rx) = mpsc::sync_channel(1);
    ctx.run(LibraryCommand::SocketCreate { options: socket_opts, responder: tx });
    let (ch0, ch1) = rx.recv().unwrap();
    unsafe { *out0 = ch0.into_raw() };
    unsafe { *out1 = ch1.into_raw() };
    zx_status::Status::OK
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
        buf: unsafe { ExtBuffer::new(buf, buf_len as usize) },
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
        out_buf: unsafe { ExtBuffer::new(out_buf, out_len as usize) },
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

#[no_mangle]
pub unsafe extern "C" fn ffx_event_create(
    ctx: *const LibContext,
    _options: u32,
    out: *mut zx_types::zx_handle_t,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (tx, rx) = mpsc::sync_channel(1);
    ctx.run(LibraryCommand::EventCreate { responder: tx });
    let hdl = rx.recv().unwrap();
    unsafe { *out = hdl.into_raw() };
    zx_status::Status::OK
}

#[no_mangle]
pub unsafe extern "C" fn ffx_eventpair_create(
    ctx: *const LibContext,
    _options: u32,
    out0: *mut zx_types::zx_handle_t,
    out1: *mut zx_types::zx_handle_t,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (tx, rx) = mpsc::sync_channel(1);
    ctx.run(LibraryCommand::EventPairCreate { responder: tx });
    let (hdl0, hdl1) = rx.recv().unwrap();
    unsafe { *out0 = hdl0.into_raw() };
    unsafe { *out1 = hdl1.into_raw() };
    zx_status::Status::OK
}

#[no_mangle]
pub unsafe extern "C" fn ffx_object_signal(
    ctx: *const LibContext,
    hdl: zx_types::zx_handle_t,
    clear_mask: u32,
    set_mask: u32,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (tx, rx) = mpsc::sync_channel(1);
    let handle = unsafe { fidl::Handle::from_raw(hdl) };
    let clear_mask = fidl::Signals::from_bits_retain(clear_mask);
    let set_mask = fidl::Signals::from_bits_retain(set_mask);
    ctx.run(LibraryCommand::ObjectSignal { handle, clear_mask, set_mask, responder: tx });
    rx.recv().unwrap()
}

#[no_mangle]
pub unsafe extern "C" fn ffx_object_signal_peer(
    ctx: *const LibContext,
    hdl: zx_types::zx_handle_t,
    clear_mask: u32,
    set_mask: u32,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (tx, rx) = mpsc::sync_channel(1);
    let handle = unsafe { fidl::Handle::from_raw(hdl) };
    let clear_mask = fidl::Signals::from_bits_retain(clear_mask);
    let set_mask = fidl::Signals::from_bits_retain(set_mask);
    ctx.run(LibraryCommand::ObjectSignalPeer { handle, clear_mask, set_mask, responder: tx });
    rx.recv().unwrap()
}

#[no_mangle]
pub unsafe extern "C" fn ffx_object_signal_poll(
    ctx: *const LibContext,
    hdl: zx_types::zx_handle_t,
    signals: u32,
    signals_out: *mut u32,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (tx, rx) = mpsc::sync_channel(1);
    let handle = unsafe { fidl::Handle::from_raw(hdl) };
    let signals = fidl::Signals::from_bits_retain(signals);
    ctx.run(LibraryCommand::ObjectSignalPoll { lib: ctx.clone(), handle, signals, responder: tx });
    match rx.recv().unwrap() {
        Ok(sig) => safe_write(signals_out, sig.bits()),
        Err(status) => return status,
    }
    zx_status::Status::OK
}

#[no_mangle]
pub unsafe extern "C" fn ffx_channel_create(
    ctx: *const LibContext,
    _options: u32,
    out0: *mut zx_types::zx_handle_t,
    out1: *mut zx_types::zx_handle_t,
) {
    let ctx = unsafe { get_arc(ctx) };
    let (tx, rx) = mpsc::sync_channel(1);
    ctx.run(LibraryCommand::ChannelCreate { responder: tx });
    let (ch0, ch1) = rx.recv().unwrap();
    unsafe { *out0 = ch0.into_raw() };
    unsafe { *out1 = ch1.into_raw() };
}

#[no_mangle]
pub unsafe extern "C" fn ffx_handle_get_koid(
    ctx: *const LibContext,
    hdl: zx_types::zx_handle_t,
    out: *mut zx_types::zx_koid_t,
) -> zx_status::Status {
    let ctx = unsafe { get_arc(ctx) };
    let (tx, rx) = mpsc::sync_channel(1);
    let handle = unsafe { fidl::Handle::from_raw(hdl) };
    ctx.run(LibraryCommand::HandleGetKoid { handle, responder: tx });
    let koid = match rx.recv().unwrap() {
        Ok(k) => k,
        Err(e) => return e,
    };
    unsafe { *out = koid.raw_koid() };
    zx_status::Status::OK
}

#[no_mangle]
pub unsafe extern "C" fn ffx_config_get_string(
    ctx: *const EnvContext,
    config_key: *const u8,
    config_key_len: u64,
    out_buf: *mut u8,
    out_buf_len: *mut u64,
) -> zx_status::Status {
    if ctx == std::ptr::null()
        || config_key == std::ptr::null()
        || out_buf == std::ptr::null_mut()
        || out_buf_len == std::ptr::null_mut()
    {
        return zx_status::Status::INVALID_ARGS;
    }
    let ctx = unsafe { get_arc(ctx) };
    let (tx, rx) = mpsc::sync_channel(1);
    let config_key = unsafe { std::slice::from_raw_parts(config_key, config_key_len as usize) };
    let config_key_str = match std::str::from_utf8(config_key) {
        Ok(s) => s.to_owned(),
        Err(e) => {
            ctx.write_err(e);
            return zx_status::Status::INTERNAL;
        }
    };
    let out_buf_size = unsafe { *out_buf_len as usize };
    ctx.lib_ctx().run(LibraryCommand::ConfigGetString {
        responder: tx,
        env_ctx: ctx.clone(),
        config_key: config_key_str,
        out_buf: unsafe { ExtBuffer::new(out_buf, out_buf_size) },
    });
    match rx.recv().unwrap() {
        Ok(size) => {
            safe_write(out_buf_len, size as u64);
            zx_status::Status::OK
        }
        Err(e) => e,
    }
}

// LINT.ThenChange(../cpp/fuchsia_controller_internal/fuchsia_controller.h)

#[cfg(test)]
mod test {
    use super::*;
    use byteorder::{NativeEndian, ReadBytesExt};
    use fidl::AsHandleRef;
    use futures_test as _;
    use std::io::Read;
    use std::os::fd::{FromRawFd, RawFd};
    use std::os::unix::net::UnixStream;

    static mut SCRATCH: [u8; 1024] = [0; 1024];
    fn testing_lib_context() -> *const LibContext {
        // SAFETY: This is unsafe because it is a static location, which can
        // then be potentially accessed by multiple threads. So far this is not
        // actually read by anything and any data clobbering should not be an
        // issue. If it comes to the point that these values need to be read in
        // the tests, this must be changed so that each data buffer is declared
        // in each individual test (either that or just a re-design of the
        // library context).
        // TODO(357638987): Remove this allow once the Rust compiler rolls and
        // it recognizes the unsafe as unnecessary.
        #[allow(unused_unsafe)]
        let raw = unsafe { std::ptr::addr_of_mut!(SCRATCH) as *mut u8 };
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
        // For anyone reading these, the handles sent to the write and write_etc functions are
        // presumed to be owned by said functions when called. The behavior here is written under
        // the assumption that the tests are going to pass. If they don't there is a lot of global
        // state under the FIDL host-side handle emulation that is going to look very strange, and
        // will likely cause tests outside of this one to fail due to double-closing of channels.
        // Since tests run in parallel it is possible for another test to open a new channel with
        // the same raw handle number _before_ one of these channels is closed, thus causing a
        // double close error.
        //
        // Just something to keep in mind, especially if attempting to extend this code with more
        // potential failure cases.
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
                handles_buf.as_mut_ptr().cast(),
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
    fn channel_write_etc_then_read_some_data() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::Channel::create();
        let (c, d) = fidl::Channel::create();
        let c_handle = c.raw_handle();
        let d_handle = d.raw_handle();
        let mut write_buf = [1u8, 2u8];
        let mut handles_buf: [zx_types::zx_handle_disposition_t; 2] = [
            zx_types::zx_handle_disposition_t {
                operation: zx_types::ZX_HANDLE_OP_MOVE,
                handle: c.raw_handle(),
                type_: zx_types::ZX_OBJ_TYPE_CHANNEL,
                rights: zx_types::ZX_RIGHT_SAME_RIGHTS,
                result: zx_types::ZX_OK,
            },
            zx_types::zx_handle_disposition_t {
                operation: zx_types::ZX_HANDLE_OP_MOVE,
                handle: d.raw_handle(),
                type_: zx_types::ZX_OBJ_TYPE_CHANNEL,
                rights: zx_types::ZX_RIGHT_SAME_RIGHTS,
                result: zx_types::ZX_OK,
            },
        ];
        let result = unsafe {
            ffx_channel_write_etc(
                lib_ctx,
                b.raw_handle(),
                write_buf.as_mut_ptr(),
                2,
                handles_buf.as_mut_ptr().cast(),
                2,
            )
        };
        assert_eq!(handles_buf[0].handle, 0);
        assert_eq!(handles_buf[1].handle, 0);
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
    fn channel_write_etc_unsupported_op() {
        let lib_ctx = testing_lib_context();
        let (_a, b) = fidl::Channel::create();
        let (c, d) = fidl::Channel::create();
        let mut write_buf = [1u8, 2u8];
        let mut handles_buf: [zx_types::zx_handle_disposition_t; 2] = [
            zx_types::zx_handle_disposition_t {
                operation: zx_types::ZX_HANDLE_OP_DUPLICATE,
                handle: c.raw_handle(),
                type_: zx_types::ZX_OBJ_TYPE_CHANNEL,
                rights: zx_types::ZX_RIGHT_SAME_RIGHTS,
                result: zx_types::ZX_OK,
            },
            zx_types::zx_handle_disposition_t {
                operation: zx_types::ZX_HANDLE_OP_MOVE,
                handle: d.raw_handle(),
                type_: zx_types::ZX_OBJ_TYPE_CHANNEL,
                rights: zx_types::ZX_RIGHT_SAME_RIGHTS,
                result: zx_types::ZX_OK,
            },
        ];
        let result = unsafe {
            ffx_channel_write_etc(
                lib_ctx,
                b.raw_handle(),
                write_buf.as_mut_ptr(),
                2,
                handles_buf.as_mut_ptr().cast(),
                2,
            )
        };
        assert_eq!(result, zx_status::Status::NOT_SUPPORTED);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn channel_write_etc_invalid_arg() {
        let lib_ctx = testing_lib_context();
        let (_a, b) = fidl::Channel::create();
        let (c, d) = fidl::Channel::create();
        let c = c.into_raw();
        let d = d.into_raw();
        let mut write_buf = [1u8, 2u8];
        let mut handles_buf: [zx_types::zx_handle_disposition_t; 2] = [
            zx_types::zx_handle_disposition_t {
                operation: zx_types::ZX_HANDLE_OP_MOVE,
                handle: c,
                type_: zx_types::ZX_OBJ_TYPE_CHANNEL,
                rights: 1 << 30,
                result: zx_types::ZX_OK,
            },
            zx_types::zx_handle_disposition_t {
                operation: zx_types::ZX_HANDLE_OP_MOVE,
                handle: d,
                type_: zx_types::ZX_OBJ_TYPE_CHANNEL,
                rights: zx_types::ZX_RIGHT_SAME_RIGHTS,
                result: zx_types::ZX_OK,
            },
        ];
        let result = unsafe {
            ffx_channel_write_etc(
                lib_ctx,
                b.raw_handle(),
                write_buf.as_mut_ptr(),
                2,
                handles_buf.as_mut_ptr().cast(),
                2,
            )
        };
        assert_eq!(result, zx_status::Status::INVALID_ARGS);
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
    fn event_pair_signal_peer_peer_closed() {
        let lib_ctx = testing_lib_context();
        let (a, b) = fidl::EventPair::create();
        drop(b);
        let result = unsafe {
            ffx_object_signal_peer(
                lib_ctx,
                a.raw_handle(),
                fidl::Signals::empty().bits(),
                fidl::Signals::USER_0.bits(),
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
    fn user_signal_events_null_out() {
        let lib_ctx = testing_lib_context();
        let event = fidl::Event::create();
        let result = unsafe {
            ffx_object_signal(
                lib_ctx,
                event.raw_handle(),
                fidl::Signals::empty().bits(),
                fidl::Signals::USER_0.bits(),
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        let result = unsafe {
            ffx_object_signal_poll(
                lib_ctx,
                event.raw_handle(),
                fidl::Signals::USER_0.bits(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn user_signal_events_one_signal() {
        let lib_ctx = testing_lib_context();
        let event = fidl::Event::create();
        let result = unsafe {
            ffx_object_signal(
                lib_ctx,
                event.raw_handle(),
                fidl::Signals::empty().bits(),
                fidl::Signals::USER_0.bits(),
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        let mut out = 0u32;
        let result = unsafe {
            ffx_object_signal_poll(
                lib_ctx,
                event.raw_handle(),
                fidl::Signals::USER_0.bits(),
                &mut out,
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        assert_eq!(out, fidl::Signals::USER_0.bits());
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn user_signal_events_many_signals() {
        let lib_ctx = testing_lib_context();
        let event = fidl::Event::create();
        let result = unsafe {
            ffx_object_signal(
                lib_ctx,
                event.raw_handle(),
                fidl::Signals::empty().bits(),
                fidl::Signals::USER_0.bits(),
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        let mut out = 0u32;
        let signals = fidl::Signals::from_bits(
            fidl::Signals::USER_0.bits()
                | fidl::Signals::OBJECT_ALL.bits()
                | fidl::Signals::USER_2.bits(),
        )
        .unwrap();
        let result = unsafe {
            ffx_object_signal_poll(lib_ctx, event.raw_handle(), signals.bits(), &mut out)
        };
        assert_eq!(result, zx_status::Status::OK);
        assert_eq!(out, fidl::Signals::USER_0.bits());
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn user_signal_event_pair() {
        let lib_ctx = testing_lib_context();
        let (event, _otherevent) = fidl::EventPair::create();
        let result = unsafe {
            ffx_object_signal(
                lib_ctx,
                event.raw_handle(),
                fidl::Signals::empty().bits(),
                fidl::Signals::USER_0.bits(),
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        let mut out = 0u32;
        let signals = fidl::Signals::from_bits(
            fidl::Signals::USER_0.bits()
                | fidl::Signals::OBJECT_ALL.bits()
                | fidl::Signals::USER_2.bits(),
        )
        .unwrap();
        let result = unsafe {
            ffx_object_signal_poll(lib_ctx, event.raw_handle(), signals.bits(), &mut out)
        };
        assert_eq!(result, zx_status::Status::OK);
        assert_eq!(out, fidl::Signals::USER_0.bits());
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn user_signal_peer_event_pair() {
        let lib_ctx = testing_lib_context();
        let (tx, rx) = fidl::EventPair::create();
        let result = unsafe {
            ffx_object_signal_peer(
                lib_ctx,
                tx.raw_handle(),
                fidl::Signals::empty().bits(),
                fidl::Signals::USER_0.bits(),
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        let mut out = 0u32;
        let signals = fidl::Signals::from_bits(
            fidl::Signals::USER_0.bits()
                | fidl::Signals::OBJECT_ALL.bits()
                | fidl::Signals::USER_2.bits(),
        )
        .unwrap();
        let result =
            unsafe { ffx_object_signal_poll(lib_ctx, rx.raw_handle(), signals.bits(), &mut out) };
        assert_eq!(result, zx_status::Status::OK);
        assert_eq!(out, fidl::Signals::USER_0.bits());
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
        let mut notifier = unsafe { UnixStream::from_raw_fd(fd) };
        notifier.set_nonblocking(false).unwrap();
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
                handles_buf.as_mut_ptr().cast(),
                2,
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        let mut notifier_buf = [0u8; 4];
        let bytes_read = notifier.read(&mut notifier_buf).unwrap();
        let mut notifier_buf_reader = std::io::Cursor::new(notifier_buf);
        assert_eq!(bytes_read, 4);
        let read_handle = notifier_buf_reader.read_u32::<NativeEndian>().unwrap();
        assert_eq!(read_handle, a.raw_handle());
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn user_signal_pending() {
        let lib_ctx = testing_lib_context();
        let fd: RawFd = unsafe { ffx_connect_handle_notifier(lib_ctx) };
        let mut notifier = unsafe { UnixStream::from_raw_fd(fd) };
        notifier.set_nonblocking(false).unwrap();
        let event = fidl::Event::create();
        let mut out = 0u32;
        let signals = fidl::Signals::from_bits(
            fidl::Signals::USER_0.bits()
                | fidl::Signals::OBJECT_ALL.bits()
                | fidl::Signals::USER_2.bits(),
        )
        .unwrap();
        let result = unsafe {
            ffx_object_signal_poll(lib_ctx, event.raw_handle(), signals.bits(), &mut out)
        };
        assert_eq!(result, zx_status::Status::SHOULD_WAIT);
        let result = unsafe {
            ffx_object_signal(
                lib_ctx,
                event.raw_handle(),
                fidl::Signals::empty().bits(),
                fidl::Signals::USER_0.bits(),
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        let mut notifier_buf = [0u8; 4];
        let bytes_read = notifier.read(&mut notifier_buf).unwrap();
        let mut notifier_buf_reader = std::io::Cursor::new(notifier_buf);
        assert_eq!(bytes_read, 4);
        let read_handle = notifier_buf_reader.read_u32::<NativeEndian>().unwrap();
        assert_eq!(read_handle, event.raw_handle());
        let result = unsafe {
            ffx_object_signal_poll(lib_ctx, event.raw_handle(), signals.bits(), &mut out)
        };
        assert_eq!(result, zx_status::Status::OK);
        assert_eq!(out, fidl::Signals::USER_0.bits());
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }

    #[test]
    fn user_signal_peer_pending() {
        let lib_ctx = testing_lib_context();
        let fd: RawFd = unsafe { ffx_connect_handle_notifier(lib_ctx) };
        let mut notifier = unsafe { UnixStream::from_raw_fd(fd) };
        notifier.set_nonblocking(false).unwrap();
        let (tx, rx) = fidl::EventPair::create();
        let mut out = 0u32;
        let signals = fidl::Signals::from_bits(
            fidl::Signals::USER_0.bits()
                | fidl::Signals::OBJECT_ALL.bits()
                | fidl::Signals::USER_2.bits(),
        )
        .unwrap();
        let result =
            unsafe { ffx_object_signal_poll(lib_ctx, rx.raw_handle(), signals.bits(), &mut out) };
        assert_eq!(result, zx_status::Status::SHOULD_WAIT);
        let result = unsafe {
            ffx_object_signal_peer(
                lib_ctx,
                tx.raw_handle(),
                fidl::Signals::empty().bits(),
                fidl::Signals::USER_0.bits(),
            )
        };
        assert_eq!(result, zx_status::Status::OK);
        let mut notifier_buf = [0u8; 4];
        let bytes_read = notifier.read(&mut notifier_buf).unwrap();
        let mut notifier_buf_reader = std::io::Cursor::new(notifier_buf);
        assert_eq!(bytes_read, 4);
        let read_handle = notifier_buf_reader.read_u32::<NativeEndian>().unwrap();
        assert_eq!(read_handle, rx.raw_handle());
        let result =
            unsafe { ffx_object_signal_poll(lib_ctx, rx.raw_handle(), signals.bits(), &mut out) };
        assert_eq!(result, zx_status::Status::OK);
        assert_eq!(out, fidl::Signals::USER_0.bits());
        unsafe { destroy_ffx_lib_context(lib_ctx) }
    }
}
