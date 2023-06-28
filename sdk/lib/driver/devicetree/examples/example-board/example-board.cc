// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/devicetree/examples/example-board/example-board.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>

namespace example_board {

zx::result<> ExampleBoard::Start() {
  node_.Bind(std::move(node()));

  auto manager = fdf_devicetree::Manager::CreateFromNamespace(*incoming());
  if (manager.is_error()) {
    FDF_LOG(ERROR, "Failed to create devicetree manager: %d", manager.error_value());
    return manager.take_error();
  }

  manager_.emplace(std::move(*manager));

  auto status = manager_->Walk(manager_->default_visitor());
  if (status.is_error()) {
    FDF_LOG(ERROR, "Failed to walk the device tree: %s", status.status_string());
    return status.take_error();
  }

  auto pbus = incoming()->Connect<fuchsia_hardware_platform_bus::Service::PlatformBus>();
  if (pbus.is_error() || !pbus->is_valid()) {
    FDF_LOG(ERROR, "Failed to connect to pbus: %s", pbus.status_string());
    return pbus.take_error();
  }

  auto group_manager = incoming()->Connect<fuchsia_driver_framework::CompositeNodeManager>();
  if (group_manager.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to device group manager: %s", group_manager.status_string());
    return group_manager.take_error();
  }

  status = manager_->PublishDevices(std::move(*pbus), std::move(*group_manager));
  if (status.is_error()) {
    FDF_LOG(ERROR, "Failed to publish devices: %s", status.status_string());
    return status.take_error();
  }

  return zx::ok();
}

}  // namespace example_board

FUCHSIA_DRIVER_EXPORT(example_board::ExampleBoard);
