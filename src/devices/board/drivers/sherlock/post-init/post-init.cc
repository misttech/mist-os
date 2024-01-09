// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/board/drivers/sherlock/post-init/post-init.h"

#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>

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

constexpr std::array<const char*, 1> kDisplayVendorNodeNames = {
    "disp-soc-id1",
};

constexpr std::array<const char*, 1> kDdicVersionNodeNames = {
    "disp-soc-id2",
};

}  // namespace

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

  if (zx::result result = InitBoardInfo(); result.is_error()) {
    return completer(result.take_error());
  }

  if (zx::result result = SetBoardInfo(); result.is_error()) {
    return completer(result.take_error());
  }

  if (zx::result result = InitDisplay(); result.is_error()) {
    return completer(result.take_error());
  }

  if (zx::result result = InitTouch(); result.is_error()) {
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
  if (zx::result<uint8_t> board_build = ReadGpios(kBoardBuildNodeNames); board_build.is_ok()) {
    board_build_ = static_cast<SherlockBoardBuild>(*board_build);
  } else {
    return board_build.take_error();
  }

  if (zx::result<uint8_t> board_option = ReadGpios(kBoardOptionNodeNames); board_option.is_ok()) {
    board_option_ = *board_option;
  } else {
    return board_option.take_error();
  }

  if (zx::result<uint8_t> display_vendor = ReadGpios(kDisplayVendorNodeNames);
      display_vendor.is_ok()) {
    display_vendor_ = *display_vendor;
  } else {
    return display_vendor.take_error();
  }

  if (zx::result<uint8_t> ddic_version = ReadGpios(kDdicVersionNodeNames); ddic_version.is_ok()) {
    ddic_version_ = *ddic_version;
  } else {
    return ddic_version.take_error();
  }

  return zx::ok();
}

zx::result<> PostInit::SetBoardInfo() {
  const uint32_t board_revision = board_build_ | (board_option_ << kBoardBuildNodeNames.size());

  fdf::Arena arena('PBUS');
  auto board_info = fuchsia_hardware_platform_bus::wire::BoardInfo::Builder(arena)
                        .board_revision(board_revision)
                        .Build();

  auto result = pbus_.buffer(arena)->SetBoardInfo(board_info);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Call to SetBoardInfo failed: %s", result.FormatDescription().c_str());
    return zx::error(result.error().status());
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "SetBoardInfo failed: %s", zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  return zx::ok();
}

zx::result<uint8_t> PostInit::ReadGpios(cpp20::span<const char* const> node_names) {
  constexpr uint64_t kAmlogicAltFunctionGpio = 0;

  uint8_t value = 0;

  for (size_t i = 0; i < node_names.size(); i++) {
    zx::result gpio = incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(node_names[i]);
    if (gpio.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to GPIO node: %s", gpio.status_string());
      return gpio.take_error();
    }

    fidl::SyncClient<fuchsia_hardware_gpio::Gpio> gpio_client(*std::move(gpio));

    {
      fidl::Result<fuchsia_hardware_gpio::Gpio::SetAltFunction> result =
          gpio_client->SetAltFunction(kAmlogicAltFunctionGpio);
      if (result.is_error()) {
        if (result.error_value().is_framework_error()) {
          FDF_LOG(ERROR, "Call to SetAltFunction failed: %s",
                  result.error_value().framework_error().FormatDescription().c_str());
          return zx::error(result.error_value().framework_error().status());
        }
        if (result.error_value().is_domain_error()) {
          FDF_LOG(ERROR, "SetAltFunction failed: %s",
                  zx_status_get_string(result.error_value().domain_error()));
          return zx::error(result.error_value().domain_error());
        }

        FDF_LOG(ERROR, "Unknown error from call to SetAltFunction");
        return zx::error(ZX_ERR_BAD_STATE);
      }
    }

    {
      fidl::Result<fuchsia_hardware_gpio::Gpio::ConfigIn> result =
          gpio_client->ConfigIn(fuchsia_hardware_gpio::GpioFlags::kNoPull);
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
      fidl::Result<fuchsia_hardware_gpio::Gpio::Read> result = gpio_client->Read();
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

}  // namespace sherlock

FUCHSIA_DRIVER_EXPORT(sherlock::PostInit);
