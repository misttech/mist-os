// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/devicetree/examples/example-board/example-board.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/devicetree/visitors/load-visitors.h>

namespace example_board {

zx::result<> ExampleBoard::Start() {
  node_.Bind(std::move(node()));

  auto manager = fdf_devicetree::Manager::CreateFromNamespace(*incoming());
  if (manager.is_error()) {
    FDF_LOG(ERROR, "Failed to create devicetree manager: %s", manager.status_string());
    return manager.take_error();
  }

  manager_.emplace(std::move(*manager));

  auto visitors = fdf_devicetree::LoadVisitors(symbols());
  if (visitors.is_error()) {
    FDF_LOG(ERROR, "Failed to create visitors: %s", visitors.status_string());
    return visitors.take_error();
  }

  visitors_ = std::move(*visitors);

  auto status = manager_->Walk(*visitors_);
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

  auto pbus_client = fdf::WireSyncClient(std::move(pbus.value()));
  status = manager_->PublishDevices(pbus_client, std::move(*group_manager), node_);
  if (status.is_error()) {
    FDF_LOG(ERROR, "Failed to publish devices: %s", status.status_string());
    return status.take_error();
  }

  return zx::ok();
}

}  // namespace example_board

FUCHSIA_DRIVER_EXPORT(example_board::ExampleBoard);
