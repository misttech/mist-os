// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::socket_tunnel::socket_tunnel_device_init;
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
        socket_tunnel_device_init(
            locked,
            current_task,
            device[0].into(),
            device[1].into(),
            device[2].into(),
        );
    }
}
