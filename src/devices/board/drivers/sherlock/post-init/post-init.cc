// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/board/drivers/sherlock/post-init/post-init.h"

#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>

namespace sherlock {

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

constexpr std::array<const char*, 1> kPanelVendorNodeNames = {
    // Corresponds to pin `ID0` on Sherlock display modules.
    "disp-soc-id1",
};

constexpr std::array<const char*, 1> kPanelDdicModelNodeNames = {
    // Corresponds to pin `ID1` on Sherlock display modules.
    "disp-soc-id2",
};

// Values of display module identification pins (ID0 / ID1) are defined in
// http://goto.google.com/sherlock-display-detection-logic

// Values map to GPIO pin semantics for the display module ID0 pin.
enum class PanelVendor : uint8_t {
  kBoe = 0,
  kInnolux = 1,
};

// Values map to GPIO pin semantics for the display module ID1 pin.
enum class PanelDdicModel : uint8_t {
  kFitipowerJd9365 = 0,
  kFitipowerJd9364 = 1,
};

}  // namespace

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

  if (zx::result result = IdentifyPanel(); result.is_error()) {
    return completer(result.take_error());
  }

  if (zx::result result = InitDisplay(); result.is_error()) {
    return completer(result.take_error());
  }

  if (zx::result result = InitTouch(); result.is_error()) {
    return completer(result.take_error());
  }

  if (zx::result result = InitBacklight(); result.is_error()) {
    return completer(result.take_error());
  }

  if (zx::result result = SetInspectProperties(); result.is_error()) {
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
  uint8_t value = 0;

  for (size_t i = 0; i < node_names.size(); i++) {
    zx::result gpio = incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(node_names[i]);
    if (gpio.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to GPIO node: %s", gpio.status_string());
      return gpio.take_error();
    }

    fidl::SyncClient<fuchsia_hardware_gpio::Gpio> gpio_client(*std::move(gpio));

    {
      fidl::Result<fuchsia_hardware_gpio::Gpio::SetBufferMode> result =
          gpio_client->SetBufferMode(fuchsia_hardware_gpio::BufferMode::kInput);
      if (result.is_error()) {
        if (result.error_value().is_framework_error()) {
          FDF_LOG(ERROR, "Call to SetBufferMode failed: %s",
                  result.error_value().framework_error().FormatDescription().c_str());
          return zx::error(result.error_value().framework_error().status());
        }
        if (result.error_value().is_domain_error()) {
          FDF_LOG(ERROR, "SetBufferMode failed: %s",
                  zx_status_get_string(result.error_value().domain_error()));
          return zx::error(result.error_value().domain_error());
        }

        FDF_LOG(ERROR, "Unknown error from call to SetBufferMode");
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

namespace {

zx::result<display::PanelType> GetPanelType(PanelVendor panel_vendor, PanelDdicModel ddic_model) {
  switch (panel_vendor) {
    case PanelVendor::kInnolux:
      switch (ddic_model) {
        case PanelDdicModel::kFitipowerJd9364:
          return zx::ok(display::PanelType::kInnoluxP070acbFitipowerJd9364);
        case PanelDdicModel::kFitipowerJd9365:
          FDF_LOG(ERROR, "Unsupported panel type detected: panel vendor: Innolux, DDIC: JD9365");
          return zx::error(ZX_ERR_NOT_SUPPORTED);
      }
      break;
    case PanelVendor::kBoe:
      switch (ddic_model) {
        case PanelDdicModel::kFitipowerJd9364:
          return zx::ok(display::PanelType::kBoeTv101wxmFitipowerJd9364);
        case PanelDdicModel::kFitipowerJd9365:
          return zx::ok(display::PanelType::kBoeTv101wxmFitipowerJd9365);
      }
      break;
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

}  // namespace

zx::result<> PostInit::IdentifyPanel() {
  zx::result<uint8_t> panel_vendor_result = ReadGpios(kPanelVendorNodeNames);
  if (panel_vendor_result.is_error()) {
    FDF_LOG(ERROR, "Failed to read display vendor GPIOs: %s", panel_vendor_result.status_string());
    return panel_vendor_result.take_error();
  }

  // The cast result will always be a valid member because the `ReadGpios()`
  // result for a single GPIO is guaranteed to be 0 or 1.
  PanelVendor panel_vendor = static_cast<PanelVendor>(*panel_vendor_result);

  zx::result<uint8_t> ddic_model_result = ReadGpios(kPanelDdicModelNodeNames);
  if (ddic_model_result.is_error()) {
    FDF_LOG(ERROR, "Failed to read DDIC version GPIOs: %s", ddic_model_result.status_string());
    return ddic_model_result.take_error();
  }

  // The cast result will always be a valid member because the `ReadGpios()`
  // result for a single GPIO is guaranteed to be 0 or 1.
  PanelDdicModel ddic_model = static_cast<PanelDdicModel>(*ddic_model_result);

  zx::result<display::PanelType> panel_type = GetPanelType(panel_vendor, ddic_model);
  if (panel_type.is_error()) {
    FDF_LOG(ERROR, "Failed to get panel type: %s", panel_type.status_string());
    return panel_type.take_error();
  }
  panel_type_ = *panel_type;
  return zx::ok();
}

zx::result<> PostInit::SetInspectProperties() {
  auto inspect_sink = incoming()->Connect<fuchsia_inspect::InspectSink>();
  if (inspect_sink.is_error() || !inspect_sink->is_valid()) {
    FDF_LOG(ERROR, "Failed to connect to InspectSink: %s", inspect_sink.status_string());
    return inspect_sink.take_error();
  }

  component_inspector_ = std::make_unique<inspect::ComponentInspector>(
      dispatcher(), inspect::PublishOptions{.inspector = inspector_,
                                            .client_end = std::move(inspect_sink.value())});

  root_ = inspector_.GetRoot().CreateChild("sherlock_board_driver");
  board_rev_property_ = root_.CreateUint("board_build", board_build_);
  board_option_property_ = root_.CreateUint("board_option", board_option_);
  panel_type_property_ = root_.CreateUint("panel_type", static_cast<uint32_t>(panel_type_));
  return zx::ok();
}

}  // namespace sherlock

FUCHSIA_DRIVER_EXPORT(sherlock::PostInit);
