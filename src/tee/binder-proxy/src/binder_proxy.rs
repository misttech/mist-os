// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use anyhow::Error;

use crate::microfuchsia_control;
use vsock_sys::{create_virtio_stream_socket, sockaddr_vm};

use rpcbinder;

pub struct BinderProxy {
    server: rpcbinder::RpcServer,
}

impl BinderProxy {
    pub fn new(port: u32) -> Result<Self, Error> {
        // Make a virtio stream socket.
        let mut socket_fd = 0;
        let status = zx::Status::from_raw(unsafe { create_virtio_stream_socket(&mut socket_fd) });
        if status != zx::Status::OK {
            anyhow::bail!("Could not create virtio stream socket: {status:?}");
        }
        let socket_fd = unsafe { OwnedFd::from_raw_fd(socket_fd) };
        // Bind the socket to listen for connections from the host on the specified port.
        let addr = sockaddr_vm {
            svm_family: libc::AF_VSOCK as u16,
            svm_port: port,
            svm_cid: 2, // VMADDR_CID_HOST
            ..Default::default()
        };
        let r = unsafe {
            let addr_ptr = &addr as *const sockaddr_vm as *const libc::sockaddr;
            libc::bind(
                socket_fd.as_raw_fd(),
                addr_ptr,
                std::mem::size_of::<sockaddr_vm>() as libc::socklen_t,
            )
        };
        if r == -1 {
            anyhow::bail!("Bind failed: {:?}", std::io::Error::last_os_error())
        }
        // Set up rpcbinder server bound to instance of microfuchsia_control.
        let service = microfuchsia_control::new_binder();
        let server = rpcbinder::RpcServer::new_bound_socket(service, socket_fd)?;
        Ok(Self { server })
    }

    pub fn run(&self) -> Result<(), Error> {
        self.server.join();
        Ok(())
    }
}
