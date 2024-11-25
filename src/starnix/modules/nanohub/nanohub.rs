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
    let devices = [
        ["/dev/nanohub_brightness", "nanohub_brightness", "nanohub"],
        ["/dev/nanohub_render", "nanohub_render", "nanohub"],
        ["/dev/nanohub_display", "nanohub_display", "nanohub"],
        ["/dev/nanohub_pele", "nanohub_pele", "nanohub"],
    ];

    for device in devices {
        register_socket_tunnel_device(
            locked,
            current_task,
            device[0].into(),
            device[1].into(),
            device[2].into(),
            DeviceDirectory::new,
        );
    }

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
