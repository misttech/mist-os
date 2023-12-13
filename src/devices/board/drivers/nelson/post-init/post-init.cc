// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "post-init.h"

#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/inspect/cpp/reader.h>

#include <array>

namespace {

constexpr std::array<const char*, 3> kBoardBuildNodeNames = {
    "hw-id-0",
    "hw-id-1",
    "hw-id-2",
};

constexpr std::array<const char*, 2> kBoardOptionNodeNames = {
    "hw-id-3",
    "hw-id-4",
};

constexpr std::array<const char*, 2> kDisplayIdNodeNames = {
    "disp-soc-id0",
    "disp-soc-id1",
};

// Nelson Board Revs
enum {
  kBoardBuildP1 = 0,
  kBoardBuildP2 = 1,
  kBoardBuildP2Doe = 2,
  kBoardBuildPreEvt = 3,
  kBoardBuildEvt = 4,
  kBoardBuildDvt = 5,
  kBoardBuildDvt2 = 6,

  kMaxSupportedBuild,  // This must be last entry
};

}  // namespace

namespace nelson {

void PostInit::Start(fdf::StartCompleter completer) {
  component_inspector_ = std::make_unique<inspect::ComponentInspector>(
      dispatcher(), inspect::PublishOptions{.inspector = inspector_});

  parent_.Bind(std::move(node()));

  zx::result pbus =
      incoming()->Connect<fuchsia_hardware_platform_bus::Service::PlatformBus>("pbus");
  if (pbus.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to PlatformBus: %s", pbus.status_string());
    return completer(pbus.take_error());
  }
  pbus_.Bind(*std::move(pbus));

  zx::result composite_manager =
      incoming()->Connect<fuchsia_driver_framework::CompositeNodeManager>();
  if (composite_manager.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to CompositeNodeManager: %s",
            composite_manager.status_string());
    return completer(composite_manager.take_error());
  }
  composite_manager_ = fidl::SyncClient(*std::move(composite_manager));

  auto args = fuchsia_driver_framework::NodeAddArgs({.name = "post-init"});

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create controller endpoints: %s",
            controller_endpoints.status_string());
    return completer(controller_endpoints.take_error());
  }
  controller_.Bind(std::move(controller_endpoints->client));

  if (zx::result result = InitBoardInfo(); result.is_error()) {
    return completer(result.take_error());
  }

  if (zx::result result = SetInspectProperties(); result.is_error()) {
    return completer(result.take_error());
  }

  if (zx::result result = InitDisplay(); result.is_error()) {
    return completer(result.take_error());
  }

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

zx::result<> PostInit::InitBoardInfo() {
  zx::result<uint32_t> board_build =
      ReadGpios({kBoardBuildNodeNames.data(), kBoardBuildNodeNames.size()});
  if (board_build.is_error()) {
    return board_build.take_error();
  }
  if (*board_build >= kMaxSupportedBuild) {
    // We have detected a new board rev. Print this warning just in case the
    // new board rev requires additional support that we were not aware of
    FDF_LOG(INFO, "Unsupported board revision detected (%u)", *board_build);
  }
  board_build_ = *board_build;

  zx::result<uint32_t> board_option =
      ReadGpios({kBoardOptionNodeNames.data(), kBoardOptionNodeNames.size()});
  if (board_option.is_error()) {
    return board_option.take_error();
  }
  board_option_ = *board_option;

  zx::result<uint32_t> display_id =
      ReadGpios({kDisplayIdNodeNames.data(), kDisplayIdNodeNames.size()});
  if (display_id.is_error()) {
    return display_id.take_error();
  }
  display_id_ = *display_id;

  return zx::ok();
}

zx::result<> PostInit::SetInspectProperties() {
  root_ = inspector_.GetRoot().CreateChild("nelson_board_driver");
  board_build_property_ = root_.CreateUint("board_build", board_build_);
  board_option_property_ = root_.CreateUint("board_option", board_option_);
  display_id_property_ = root_.CreateUint("display_id", display_id_);

  return zx::ok();
}

zx::result<uint32_t> PostInit::ReadGpios(cpp20::span<const char* const> node_names) {
  uint32_t value = 0;

  for (size_t i = 0; i < node_names.size(); i++) {
    zx::result gpio = incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(node_names[i]);
    if (gpio.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to GPIO node: %s", gpio.status_string());
      return gpio.take_error();
    }

    fidl::SyncClient<fuchsia_hardware_gpio::Gpio> gpio_client(*std::move(gpio));

    {
      auto result = gpio_client->ConfigIn(fuchsia_hardware_gpio::GpioFlags::kNoPull);
      if (result.is_error()) {
        if (result.error_value().is_framework_error()) {
          FDF_LOG(ERROR, "Call to ConfigIn failed: %s",
                  result.error_value().framework_error().FormatDescription().c_str());
          return zx::error(result.error_value().framework_error().status());
        }
        if (result.error_value().is_domain_error()) {
          FDF_LOG(ERROR, "ConfigIn failed: %s",
                  zx_status_get_string(result.error_value().domain_error()));
          return zx::error(result.error_value().domain_error());
        }

        FDF_LOG(ERROR, "Unknown error from call to ConfigIn");
        return zx::error(ZX_ERR_BAD_STATE);
      }
    }

    {
      auto result = gpio_client->Read();
      if (result.is_error()) {
        if (result.error_value().is_framework_error()) {
          FDF_LOG(ERROR, "Call to Read failed: %s",
                  result.error_value().framework_error().FormatDescription().c_str());
          return zx::error(result.error_value().framework_error().status());
        }
        if (result.error_value().is_domain_error()) {
          FDF_LOG(ERROR, "Read failed: %s",
                  zx_status_get_string(result.error_value().domain_error()));
          return zx::error(result.error_value().domain_error());
        }

        FDF_LOG(ERROR, "Unknown error from call to Read");
        return zx::error(ZX_ERR_BAD_STATE);
      }

      if (result->value()) {
        value |= 1 << i;
      }
    }
  }

  return zx::ok(value);
}

}  // namespace nelson

FUCHSIA_DRIVER_EXPORT(nelson::PostInit);
