// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async as fasync;
use futures::channel::mpsc as async_mpsc;
use futures::StreamExt;
use std::sync::mpsc as sync_mpsc;
use wayland_bridge::dispatcher::WaylandDispatcher;
use zx::{self as zx, HandleBased};

pub enum WaylandCommand {
    PushClient(zx::Channel),
}

#[repr(C)]
#[allow(non_camel_case_types)]
pub struct wayland_server_handle_t {
    pub sender: async_mpsc::UnboundedSender<WaylandCommand>,
}

fn start_wayland_server() -> Result<(fasync::LocalExecutor, WaylandDispatcher), zx::Status> {
    let mut executor = fasync::LocalExecutor::new();
    let mut dispatcher = WaylandDispatcher::new().map_err(|e| {
        log::error!("Failed to create wayland dispatcher: {}", e);
        zx::Status::INTERNAL
    })?;
    // Try to get display properties before serving.
    let scenic = dispatcher.display.scenic().clone();
    match executor.run_singlethreaded(async { scenic.get_display_info().await }) {
        Ok(display_info) => dispatcher.display.set_display_info(&display_info),
        Err(e) => eprintln!("get_display_info error: {:?}", e),
    }
    Ok((executor, dispatcher))
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn wayland_server_create(
    out: *mut *mut wayland_server_handle_t,
) -> zx::sys::zx_status_t {
    let (sender, mut receiver) = async_mpsc::unbounded::<WaylandCommand>();
    let (result_sender, result_receiver) = sync_mpsc::sync_channel::<zx::sys::zx_status_t>(0);
    std::thread::spawn(move || {
        let (mut executor, dispatcher) = match start_wayland_server() {
            Ok(result) => {
                result_sender.send(zx::sys::ZX_OK).unwrap();
                result
            }
            Err(status) => {
                result_sender.send(status.into_raw()).unwrap();
                return;
            }
        };

        executor.run_singlethreaded(async move {
            while let Some(WaylandCommand::PushClient(channel)) = receiver.next().await {
                dispatcher
                    .display
                    .clone()
                    .spawn_new_client(fasync::Channel::from_channel(channel), false);
            }
        });
    });

    let result = match result_receiver.recv() {
        Ok(status) => status,
        Err(_) => zx::sys::ZX_ERR_INTERNAL,
    };

    if result == zx::sys::ZX_OK {
        // Allocate the type we provide to the caller on the heap. This will be owned by the client
        // so we leak the pointer here.
        let ptr = Box::leak(Box::new(wayland_server_handle_t { sender }));

        unsafe { *out = ptr };
    }
    result
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn wayland_server_push_client(
    server_handle: *mut wayland_server_handle_t,
    channel: zx::sys::zx_handle_t,
) {
    let handle = unsafe { zx::Handle::from_raw(channel) };
    // Unwrap here since this will only fail if we're provided with a null pointer, which is not
    // something we support.
    let server = unsafe { server_handle.as_mut().unwrap() };
    // Unwrap here since the only failure mode is if this is called after `wayland_server_destroy`,
    // which is not supported.
    server
        .sender
        .unbounded_send(WaylandCommand::PushClient(zx::Channel::from_handle(handle)))
        .unwrap();
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn wayland_server_destroy(server_handle: *mut wayland_server_handle_t) {
    unsafe {
        // Drop the boxed pointer.
        let _ = Box::from_raw(server_handle);
    }
}
