// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpu.mali/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/incoming/cpp/service_member_watcher.h>

#include <filesystem>

#include "src/lib/fxl/command_line.h"

int main(int argc, char** argv) {
  auto command_line = fxl::CommandLineFromArgcArgv(argc, argv);

  static const char kPowerStateFlag[] = "power-state";

  component::SyncServiceMemberWatcher<fuchsia_hardware_gpu_mali::UtilsService::Device> watcher;
  zx::result client_end = watcher.GetNextInstance(true);
  if (client_end.is_error()) {
    printf("Failed to open magma device: %s\n", client_end.status_string());
    return -1;
  }

  std::string power_state_string;
  if (command_line.GetOptionValue(kPowerStateFlag, &power_state_string)) {
    uint32_t power_state = atoi(power_state_string.c_str());
    printf("Setting power state to %u\n", power_state);
    auto result = fidl::WireCall(client_end.value())->SetPowerState(power_state);
    if (!result.ok()) {
      fprintf(stderr, "magma SetPowerState transport failed: %d", result.status());
      return -1;
    }
    if (result->is_error()) {
      fprintf(stderr, "magma SetPowerState failed: %d", result->error_value());
      return -1;
    }
  } else {
    fprintf(stderr, "No request\n");
    return -1;
  }

  return 0;
}
