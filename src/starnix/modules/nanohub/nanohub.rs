// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::nanohub_comms_directory::NanohubCommsDirectory;
use crate::socket_tunnel_file::register_socket_tunnel_device;
use starnix_core::fs::sysfs::DeviceDirectory;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::FsString;
use starnix_sync::{FileOpsCore, LockBefore, Locked};

pub fn nanohub_device_init<L>(locked: &mut Locked<'_, L>, current_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    struct Descriptor {
        socket_label: FsString,
        dev_node_name: FsString,
    }

    let descriptors = vec![
        Descriptor { socket_label: b"/dev/nanohub".into(), dev_node_name: b"nanohub".into() },
        Descriptor {
            socket_label: b"/dev/nanohub_brightness".into(),
            dev_node_name: b"nanohub_brightness".into(),
        },
        Descriptor { socket_label: b"/dev/nanohub_bt".into(), dev_node_name: b"nanohub_bt".into() },
        Descriptor {
            socket_label: b"/dev/nanohub_console".into(),
            dev_node_name: b"nanohub_console".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_debug_log".into(),
            dev_node_name: b"nanohub_debug_log".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_display".into(),
            dev_node_name: b"nanohub_display".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_metrics".into(),
            dev_node_name: b"nanohub_metrics".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_pele".into(),
            dev_node_name: b"nanohub_pele".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_render".into(),
            dev_node_name: b"nanohub_render".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_rpc0".into(),
            dev_node_name: b"nanohub_rpc0".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_rpc1".into(),
            dev_node_name: b"nanohub_rpc1".into(),
        },
        Descriptor {
            socket_label: b"/dev/nanohub_touch".into(),
            dev_node_name: b"nanohub_touch".into(),
        },
    ];

    for descriptor in descriptors {
        register_socket_tunnel_device(
            locked,
            current_task,
            descriptor.socket_label.as_ref(),
            descriptor.dev_node_name.as_ref(),
            b"nanohub".into(),
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
