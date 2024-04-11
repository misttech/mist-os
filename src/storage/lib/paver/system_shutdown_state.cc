// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/system_shutdown_state.h"

#include <fidl/fuchsia.device.manager/cpp/fidl.h>

#include <sdk/lib/component/incoming/cpp/protocol.h>

#include "src/storage/lib/paver/pave-logging.h"

using fuchsia_device_manager::SystemPowerState;
using fuchsia_device_manager::SystemStateTransition;

namespace paver {

SystemPowerState GetShutdownSystemState(fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir) {
  constexpr SystemPowerState kDefaultShutdownSystemState = SystemPowerState::kFullyOn;

  zx::result client = component::ConnectAt<SystemStateTransition>(svc_dir);
  if (client.is_error()) {
    ERROR("Failed to connect to fuchsia.device.manager/SystemStateTransition: %s",
          client.status_string());
    return kDefaultShutdownSystemState;
  }

  fidl::WireResult result = fidl::WireCall(client.value())->GetTerminationSystemState();
  if (!result.ok()) {
    ERROR("Failed to get termination state: %s", result.status_string());
    return kDefaultShutdownSystemState;
  }

  return result->state;
}

}  // namespace paver
