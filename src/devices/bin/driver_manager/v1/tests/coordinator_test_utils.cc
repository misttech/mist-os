// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "coordinator_test_utils.h"

#include <fidl/fuchsia.boot/cpp/wire.h>

#include <mock-boot-arguments/server.h>
#include <zxtest/zxtest.h>

CoordinatorConfig DefaultConfig(async_dispatcher_t* bootargs_dispatcher,
                                mock_boot_arguments::Server* boot_args,
                                fidl::WireSyncClient<fuchsia_boot::Arguments>* client) {
  // The DummyFsProvider is stateless.  Create a single static one here so that we don't need to
  // manage pointer lifetime for it below.
  static DummyFsProvider dummy_fs_provider;

  CoordinatorConfig config;

  if (boot_args != nullptr && client != nullptr) {
    *boot_args = mock_boot_arguments::Server{{{"key1", "new-value"}, {"key2", "value2"}}};
    zx::result result = boot_args->CreateClient(bootargs_dispatcher);
    if (result.is_ok()) {
      *client = std::move(result.value());
    } else {
      printf("Failed to create mock boot arguments client: %s\n", result.status_string());
    }
  }
  config.delay_fallback_until_base_drivers_indexed = false;
  config.boot_args = client;
  config.fs_provider = &dummy_fs_provider;
  config.suspend_timeout = zx::sec(2);
  config.resume_timeout = zx::sec(2);
  config.path_prefix = "/pkg/";
  config.default_shutdown_system_state = fuchsia_device_manager::wire::SystemPowerState::kMexec;
  return config;
}

void InitializeCoordinator(Coordinator* coordinator) {
  coordinator->InitCoreDevices(kSystemDriverPath);
  coordinator->set_running(true);
}
