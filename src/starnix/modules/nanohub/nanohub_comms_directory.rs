// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::nanohub_sysfs_files::{
    FirmwareNameSysFsOps, FirmwareVersionSysFsOps, NanohubSysFsNode, TimeSyncSysFsOps,
    WakeUpEventDuration,
};
use crate::socket_tunnel_file::{FirmwareFile, SocketTunnelSysfsFile};
use starnix_core::device::kobject::Device;
use starnix_core::fs::sysfs::build_device_directory;
use starnix_core::vfs::pseudo::simple_directory::SimpleDirectoryMutator;
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::file_mode::mode;

pub fn build_nanohub_comms_directory(device: &Device, dir: &SimpleDirectoryMutator) {
    build_device_directory(device, dir);
    // TODO(https://fxbug.dev/419041879): These are currently set to "system", but they
    // should be set to FsCred::root().
    let system_creds = FsCred::root();
    dir.entry_etc(
        "display_panel_name".into(),
        SocketTunnelSysfsFile::new(
            b"/sys/devices/virtual/nanohub/nanohub_comms/display_panel_name".into(),
        ),
        mode!(IFREG, 0o440),
        DeviceType::NONE,
        system_creds,
    );
    dir.entry_etc(
        "display_select".into(),
        SocketTunnelSysfsFile::new(
            b"/sys/devices/virtual/nanohub/nanohub_comms/display_select".into(),
        ),
        mode!(IFREG, 0o660),
        DeviceType::NONE,
        system_creds,
    );
    dir.entry_etc(
        "display_state".into(),
        SocketTunnelSysfsFile::new(
            b"/sys/devices/virtual/nanohub/nanohub_comms/display_state".into(),
        ),
        mode!(IFREG, 0o440),
        DeviceType::NONE,
        system_creds,
    );
    dir.entry("download_firmware", FirmwareFile::new(), mode!(IFREG, 0o220));
    dir.entry(
        "firmware_name",
        NanohubSysFsNode::<FirmwareNameSysFsOps>::new(),
        mode!(IFREG, 0o440),
    );
    dir.entry(
        "firmware_version",
        NanohubSysFsNode::<FirmwareVersionSysFsOps>::new(),
        mode!(IFREG, 0o440),
    );
    dir.entry(
        "hw_reset",
        SocketTunnelSysfsFile::new(b"/sys/devices/virtual/nanohub/nanohub_comms/hw_reset".into()),
        mode!(IFREG, 0o220),
    );
    dir.entry_etc(
        "time_sync".into(),
        NanohubSysFsNode::<TimeSyncSysFsOps>::new(),
        mode!(IFREG, 0o440),
        DeviceType::NONE,
        system_creds,
    );
    dir.entry(
        "wakeup_event_msec",
        NanohubSysFsNode::<WakeUpEventDuration>::new(),
        mode!(IFREG, 0o660),
    );
    dir.entry(
        "wake_lock",
        SocketTunnelSysfsFile::new(b"/sys/devices/virtual/nanohub/nanohub_comms/wake_lock".into()),
        mode!(IFREG, 0o440),
    );
}
