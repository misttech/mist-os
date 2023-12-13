// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_PAVER_SYSTEM_SHUTDOWN_STATE_H_
#define SRC_STORAGE_LIB_PAVER_SYSTEM_SHUTDOWN_STATE_H_

#include <fidl/fuchsia.device.manager/cpp/common_types.h>
#include <fidl/fuchsia.io/cpp/wire.h>

namespace paver {

/// Retrieve current System Power State from `fuchsia.device.manager.SystemStateTransition`.
///
/// `fuchsia.device.manager.SystemPowerState.kFullyOn` is returned if state can't be retrieved.
fuchsia_device_manager::SystemPowerState GetShutdownSystemState(
    fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir);

}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_SYSTEM_SHUTDOWN_STATE_H_
