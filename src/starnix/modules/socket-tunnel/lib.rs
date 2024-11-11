// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ops::Deref;
use std::sync::Arc;

use fidl_fuchsia_hardware_sockettunnel::{DeviceMarker, DeviceRegisterSocketRequest};
use starnix_core::device::DeviceOps;
use starnix_core::fs::fuchsia::new_remote_file_ops;
use starnix_core::fs::sysfs::DeviceDirectory;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{FileOps, FsNode, FsStr, FsString};
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errno;

use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use {fidl_fuchsia_hardware_sockettunnel, fuchsia_component as fcomponent};

#[derive(Clone)]
struct SocketTunnelDevice {
    socket_label: Arc<FsString>,
}

impl SocketTunnelDevice {
    fn new(socket_label: FsString) -> SocketTunnelDevice {
        SocketTunnelDevice { socket_label: Arc::new(socket_label) }
    }
}

impl DeviceOps for SocketTunnelDevice {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let device_proxy = fcomponent::client::connect_to_protocol_sync::<DeviceMarker>()
            .map_err(|_| errno!(ENOENT))?;

        // Create the socket pair for local/remote sides
        let (tx, rx) = zx::Socket::create_stream();

        let register_socket_params = DeviceRegisterSocketRequest {
            server_socket: rx.into(),
            socket_label: Some(self.socket_label.deref().clone().to_string()),
            ..Default::default()
        };
        // Execute command, check if the FIDL connection succeeded, and extract the Status
        device_proxy
            .register_socket(register_socket_params, zx::MonotonicInstant::INFINITE)
            .map_err(|_| errno!(ENOENT))?
            .ok_or_else(|| errno!(ENOENT))?;

        // This will only be reached if the status was OK
        new_remote_file_ops(tx.into())
    }
}

pub fn socket_tunnel_device_init<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    socket_label: &FsStr,
    dev_node_name: &FsStr,
    dev_class_name: &FsStr,
) where
    L: LockBefore<FileOpsCore>,
{
    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;

    let device_class =
        registry.objects.get_or_create_class(dev_class_name, registry.objects.virtual_bus());

    registry
        .register_dyn_device(
            locked,
            current_task,
            dev_node_name.into(),
            device_class,
            DeviceDirectory::new,
            SocketTunnelDevice::new(socket_label.to_owned()),
        )
        .expect("Can register socket tunnel file");
}
