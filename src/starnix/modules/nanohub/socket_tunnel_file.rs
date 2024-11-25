// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ops::Deref;
use std::sync::Arc;

use fidl_fuchsia_hardware_sockettunnel::{DeviceMarker, DeviceRegisterSocketRequest};
use starnix_core::device::kobject::Device;
use starnix_core::device::DeviceOps;
use starnix_core::fs::fuchsia::new_remote_file_ops;
use starnix_core::fs_node_impl_not_dir;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{FileOps, FsNode, FsNodeOps, FsStr, FsString};
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use {fidl_fuchsia_hardware_sockettunnel, fuchsia_component as fcomponent};

#[derive(Clone)]
pub struct SocketTunnelFile {
    socket_label: Arc<FsString>,
}

impl SocketTunnelFile {
    pub fn new(socket_label: FsString) -> SocketTunnelFile {
        SocketTunnelFile { socket_label: Arc::new(socket_label) }
    }

    fn connect(&self) -> Result<Box<dyn FileOps>, Errno> {
        let device_proxy = fcomponent::client::connect_to_protocol_sync::<DeviceMarker>()
            .map_err(|_| errno!(ENOENT))?;

        // Create the socket pair for local/remote sides
        let (tx, rx) = zx::Socket::create_datagram();

        let register_socket_params = DeviceRegisterSocketRequest {
            server_socket: rx.into(),
            socket_label: Some(self.socket_label.deref().clone().to_string()),
            ..Default::default()
        };
        // Execute command, check if the FIDL connection succeeded, and extract the Status
        device_proxy
            .register_socket(register_socket_params, zx::MonotonicInstant::INFINITE)
            .map_err(|_| errno!(ENOENT))?
            .map_err(|_| errno!(ENOENT))?;

        // This will only be reached if the status was OK
        new_remote_file_ops(tx.into())
    }
}

impl DeviceOps for SocketTunnelFile {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        self.connect()
    }
}

impl FsNodeOps for SocketTunnelFile {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        self.connect()
    }
}

/// Create and register a device node backed by a SocketTunnelFile
pub fn register_socket_tunnel_device<F, N, L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    socket_label: &FsStr,
    dev_node_name: &FsStr,
    dev_class_name: &FsStr,
    create_device_sysfs_ops: F,
) where
    F: Fn(Device) -> N + Send + Sync + 'static,
    N: FsNodeOps,
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
            create_device_sysfs_ops,
            SocketTunnelFile::new(socket_label.to_owned()),
        )
        .expect("Can register socket tunnel file");
}
