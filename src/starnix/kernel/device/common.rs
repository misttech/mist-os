// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        loop_device::loop_device_init, mem::mem_device_init, misc::misc_device_init,
        remote_block_device::remote_block_device_init, zram::zram_device_init,
    },
    fs::devpts::tty_device_init,
    task::CurrentTask,
};
use starnix_sync::{Locked, Unlocked};

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
    remote_block_device_init(locked, system_task);
    zram_device_init(locked, system_task);
}
