// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use rutabaga_gfx::{RutabagaBuilder, RutabagaComponentType, RutabagaFenceHandler};
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{FileOps, FsNode};
use starnix_logging::log_error;
use starnix_sync::{DeviceOpen, FileOpsCore, LockEqualOrBefore, Locked};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;

fn create_gpu_device(
    _locked: &mut Locked<DeviceOpen>,
    _current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    log_error!("virtio-gpu unsupported");
    error!(ENOTSUP)
}

pub fn gpu_device_init<L>(locked: &mut Locked<L>, current_task: &CurrentTask)
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;

    let _ = RutabagaBuilder::new(RutabagaComponentType::Gfxstream, 0)
        .build(RutabagaFenceHandler::new(move |_| {}), std::option::Option::None);

    registry
        .register_dyn_device(
            locked,
            current_task,
            "virtio-gpu".into(),
            registry.objects.starnix_class(),
            create_gpu_device,
        )
        .expect("can register virtio-gpu");
}
