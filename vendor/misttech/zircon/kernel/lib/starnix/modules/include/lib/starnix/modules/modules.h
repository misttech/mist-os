// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_MODULES_INCLUDE_LIB_STARNIX_MODULES_MODULES_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_MODULES_INCLUDE_LIB_STARNIX_MODULES_MODULES_H_

#include <fbl/ref_ptr.h>

namespace starnix {
class CurrentTask;
class Kernel;
}  // namespace starnix

namespace starnix_modules {

void misc_device_init(const starnix::CurrentTask& current_task);

/// Initializes common devices in `Kernel`.
///
/// Adding device nodes to devtmpfs requires the current running task. The `Kernel` constructor does
/// not create an initial task, so this function should be triggered after a `CurrentTask` has been
/// initialized.
void init_common_devices(const starnix::CurrentTask& system_task);

void register_common_file_systems(const fbl::RefPtr<starnix::Kernel>& kernel);

}  // namespace starnix_modules

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_MODULES_INCLUDE_LIB_STARNIX_MODULES_MODULES_H_
