// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::nanohub_comms_directory::NanohubCommsDirectory;
use crate::socket_tunnel_file::register_socket_tunnel_device;
use starnix_core::fs::sysfs::DeviceDirectory;
use starnix_core::task::CurrentTask;
use starnix_sync::{FileOpsCore, LockBefore, Locked};

pub fn nanohub_device_init<L>(locked: &mut Locked<'_, L>, current_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub".into(),
        b"nanohub".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub_brightness".into(),
        b"nanohub_brightness".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub_bt".into(),
        b"nanohub_bt".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub_console".into(),
        b"nanohub_console".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub_debug_log".into(),
        b"nanohub_debug_log".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub_display".into(),
        b"nanohub_display".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub_metrics".into(),
        b"nanohub_metrics".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub_pele".into(),
        b"nanohub_pele".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub_render".into(),
        b"nanohub_render".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub_rpc0".into(),
        b"nanohub_rpc0".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub_rpc1".into(),
        b"nanohub_rpc1".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );
    register_socket_tunnel_device(
        locked,
        current_task,
        b"/dev/nanohub_touch".into(),
        b"nanohub_touch".into(),
        b"nanohub".into(),
        DeviceDirectory::new,
    );

    // /dev/nanohub_comms requires a set of additional sysfs nodes, so create this route
    // with a specialized NanohubCommsDirectory implementation.
    register_socket_tunnel_device(
        locked,
        current_task,
        "/dev/nanohub_comms".into(),
        "nanohub_comms".into(),
        "nanohub".into(),
        NanohubCommsDirectory::new,
    );
}
