// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use android_system_microfuchsia_vm_service::aidl::android::system::microfuchsia::vm_service::IMicrofuchsia::{
    IMicrofuchsia, GUEST_PORT,
};
use anyhow::{self, Error};
use rpcbinder::RpcSession;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use vsock_sys::{create_virtio_stream_socket, sockaddr_vm};
use binder::IBinder;

extern "C" {
    fn register_dev_urandom_compat() -> zx::sys::zx_status_t;
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let register_status = zx::Status::from_raw(unsafe { register_dev_urandom_compat() });
    if register_status != zx::Status::OK {
        anyhow::bail!("Could not register /dev/urandom compatibility device {register_status}");
    }
    let session = RpcSession::new();
    let service: binder::Strong<dyn IMicrofuchsia> = session.setup_preconnected_client(|| {
        // Make a virtio stream socket.
        let mut socket_fd = 0;
        let status = zx::Status::from_raw(unsafe { create_virtio_stream_socket(&mut socket_fd) });
        if status != zx::Status::OK {
            tracing::error!("Could not create virtio stream socket: {status:?}");
            return None;
        }
        let socket_fd = unsafe { OwnedFd::from_raw_fd(socket_fd) };

        // connect to loopback 5680
        let addr = sockaddr_vm {
            svm_family: libc::AF_VSOCK as u16,
            svm_port: GUEST_PORT as u32,
            svm_cid: 1, // VMADDR_CID_LOCAL
            ..Default::default()
        };

        let r = unsafe {
            let addr_ptr = &addr as *const sockaddr_vm as *const libc::sockaddr;
            libc::connect(
                socket_fd.as_raw_fd(),
                addr_ptr,
                std::mem::size_of::<sockaddr_vm>() as libc::socklen_t,
            )
        };
        if r == -1 {
            tracing::error!("connect failed: {:?}", std::io::Error::last_os_error());
            return None;
        }

        Some(socket_fd.into_raw_fd())
    })?;
    service.as_binder().ping_binder().unwrap();

    Ok(())
}
