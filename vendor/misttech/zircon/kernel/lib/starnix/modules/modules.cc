// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/starnix/modules/modules.h"

#include <lib/mistos/starnix/kernel/device/mem.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>

#include <fbl/ref_ptr.h>

namespace starnix_modules {

void init_common_devices(const starnix::CurrentTask& system_task) {
  // misc_device_init(locked, system_task);
  mem_device_init(system_task);
}

void register_common_file_systems(const fbl::RefPtr<starnix::Kernel>& kernel) {}

}  // namespace starnix_modules
