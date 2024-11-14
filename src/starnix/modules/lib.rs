// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_core::bpf::fs::BpfFs;
use starnix_core::device::binder::BinderFs;
use starnix_core::device::kobject::DeviceMetadata;
use starnix_core::device::mem::{mem_device_init, DevRandom};
use starnix_core::device::{simple_device_ops, DeviceMode};
use starnix_core::fs::devpts::{dev_pts_fs, tty_device_init};
use starnix_core::fs::devtmpfs::dev_tmp_fs;
use starnix_core::fs::fuchsia::new_remote_fs;
use starnix_core::fs::nmfs::nmfs;
use starnix_core::fs::proc::proc_fs;
use starnix_core::fs::sysfs::{sys_fs, DeviceDirectory};
use starnix_core::fs::tmpfs::tmp_fs;
use starnix_core::task::{CurrentTask, Kernel};
use starnix_core::vfs::fs_registry::FsRegistry;
use starnix_modules_cgroup::{CgroupV1Fs, CgroupV2Fs};
use starnix_modules_device_mapper::{create_device_mapper, device_mapper_init};
use starnix_modules_ext4::ExtFilesystem;
use starnix_modules_functionfs::FunctionFs;
use starnix_modules_fuse::{new_fuse_fs, new_fusectl_fs, open_fuse_device};
use starnix_modules_loop::{create_loop_control_device, loop_device_init};
use starnix_modules_overlayfs::new_overlay_fs;
use starnix_modules_selinuxfs::selinux_fs;
use starnix_modules_tracefs::trace_fs;
use starnix_modules_tun::DevTun;
use starnix_modules_zram::zram_device_init;
use starnix_sync::{Locked, Unlocked};
use starnix_uapi::device_type::DeviceType;
use std::sync::Arc;

fn misc_device_init(locked: &mut Locked<'_, Unlocked>, current_task: &CurrentTask) {
    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;
    let misc_class = registry.objects.misc_class();
    registry.register_device(
        locked,
        current_task,
        // TODO(https://fxbug.dev/322365477) consider making this configurable
        "hw_random".into(),
        DeviceMetadata::new("hw_random".into(), DeviceType::HW_RANDOM, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        simple_device_ops::<DevRandom>,
    );
    registry.register_device(
        locked,
        current_task,
        "fuse".into(),
        DeviceMetadata::new("fuse".into(), DeviceType::FUSE, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        open_fuse_device,
    );
    registry.register_device(
        locked,
        current_task,
        "device-mapper".into(),
        DeviceMetadata::new("mapper/control".into(), DeviceType::DEVICE_MAPPER, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        create_device_mapper,
    );
    registry.register_device(
        locked,
        current_task,
        "loop-control".into(),
        DeviceMetadata::new("loop-control".into(), DeviceType::LOOP_CONTROL, DeviceMode::Char),
        misc_class.clone(),
        DeviceDirectory::new,
        create_loop_control_device,
    );
    registry.register_device(
        locked,
        current_task,
        "tun".into(),
        DeviceMetadata::new("tun".into(), DeviceType::TUN, DeviceMode::Char),
        misc_class,
        DeviceDirectory::new,
        simple_device_ops::<DevTun>,
    );
}

/// Initializes common devices in `Kernel`.
///
/// Adding device nodes to devtmpfs requires the current running task. The `Kernel` constructor does
/// not create an initial task, so this function should be triggered after a `CurrentTask` has been
/// initialized.
pub fn init_common_devices(locked: &mut Locked<'_, Unlocked>, system_task: &CurrentTask) {
    misc_device_init(locked, system_task);
    mem_device_init(locked, system_task);
    tty_device_init(locked, system_task);
    loop_device_init(locked, system_task);
    device_mapper_init(system_task);
    zram_device_init(locked, system_task);
}

pub fn register_common_file_systems(_locked: &mut Locked<'_, Unlocked>, kernel: &Arc<Kernel>) {
    let registry = kernel.expando.get::<FsRegistry>();
    registry.register(b"binder".into(), BinderFs::new_fs);
    registry.register(b"bpf".into(), BpfFs::new_fs);
    registry.register(b"cgroup".into(), CgroupV1Fs::new_fs);
    registry.register(b"cgroup2".into(), CgroupV2Fs::new_fs);
    // Cpusets use the generic cgroup (v1) subsystem.
    // From https://docs.kernel.org/admin-guide/cgroup-v1/cpusets.html
    registry.register(b"cpuset".into(), CgroupV1Fs::new_fs);
    registry.register(b"devpts".into(), dev_pts_fs);
    registry.register(b"devtmpfs".into(), dev_tmp_fs);
    registry.register(b"ext4".into(), ExtFilesystem::new_fs);
    registry.register(b"functionfs".into(), FunctionFs::new_fs);
    registry.register(b"fuse".into(), new_fuse_fs);
    registry.register(b"fusectl".into(), new_fusectl_fs);
    registry.register(b"nmfs".into(), nmfs);
    registry.register(b"overlay".into(), new_overlay_fs);
    registry.register(b"proc".into(), proc_fs);
    registry.register(b"remotefs".into(), new_remote_fs);
    registry.register(b"selinuxfs".into(), selinux_fs);
    registry.register(b"sysfs".into(), sys_fs);
    registry.register(b"tmpfs".into(), tmp_fs);
    registry.register(b"tracefs".into(), trace_fs);
}
