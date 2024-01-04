// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/board/drivers/sherlock/post-init/post-init.h"

#include <lib/driver/component/cpp/driver_export.h>

namespace sherlock {

void PostInit::Start(fdf::StartCompleter completer) {
  parent_.Bind(std::move(node()));

  zx::result pbus =
      incoming()->Connect<fuchsia_hardware_platform_bus::Service::PlatformBus>("pbus");
  if (pbus.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to PlatformBus: %s", pbus.status_string());
    return completer(pbus.take_error());
  }
  pbus_.Bind(*std::move(pbus));

  auto args = fuchsia_driver_framework::NodeAddArgs({.name = "post-init"});

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create controller endpoints: %s",
            controller_endpoints.status_string());
    return completer(controller_endpoints.take_error());
  }
  controller_.Bind(std::move(controller_endpoints->client));

  auto result = parent_->AddChild({std::move(args), std::move(controller_endpoints->server), {}});
  if (result.is_error()) {
    if (result.error_value().is_framework_error()) {
      FDF_LOG(ERROR, "Failed to add child: %s",
              result.error_value().framework_error().FormatDescription().c_str());
      return completer(zx::error(result.error_value().framework_error().status()));
    }
    if (result.error_value().is_domain_error()) {
      FDF_LOG(ERROR, "Failed to add child");
      return completer(zx::error(ZX_ERR_INTERNAL));
    }
  }

  return completer(zx::ok());
}

}  // namespace sherlock

FUCHSIA_DRIVER_EXPORT(sherlock::PostInit);
