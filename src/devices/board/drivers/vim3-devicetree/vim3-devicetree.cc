// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/board/drivers/vim3-devicetree/vim3-devicetree.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>

namespace vim3_dt {

zx::result<> Vim3Devicetree::Start() {
  node_.Bind(std::move(node()));

  zx::result manager = fdf_devicetree::Manager::CreateFromNamespace(*incoming());
  if (manager.is_error()) {
    FDF_LOG(ERROR, "Failed to create devicetree manager: %d", manager.error_value());
    return manager.take_error();
  }

  manager_.emplace(std::move(*manager));

  zx::result<> status = manager_->Walk(manager_->default_visitor());
  if (status.is_error()) {
    FDF_LOG(ERROR, "Failed to walk the device tree: %s", status.status_string());
    return status.take_error();
  }

  zx::result pbus = incoming()->Connect<fuchsia_hardware_platform_bus::Service::PlatformBus>();
  if (pbus.is_error() || !pbus->is_valid()) {
    FDF_LOG(ERROR, "Failed to connect to pbus: %s", pbus.status_string());
    return pbus.take_error();
  }

  zx::result group_manager = incoming()->Connect<fuchsia_driver_framework::CompositeNodeManager>();
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

}  // namespace vim3_dt

FUCHSIA_DRIVER_EXPORT(vim3_dt::Vim3Devicetree);
