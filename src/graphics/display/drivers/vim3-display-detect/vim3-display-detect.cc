// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vim3-display-detect.h"

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/zx/result.h>

#include <cstdint>

namespace vim3 {

void Vim3DisplayDetect::Start(fdf::StartCompleter completer) {
  parent_.Bind(std::move(node()));

  if (zx::result result = DetectDisplay(); result.is_error()) {
    completer(result.take_error());
    return;
  }

  completer(zx::ok());
}

zx::result<> Vim3DisplayDetect::DetectDisplay() {
  // detect RESET pin
  // if the LCD is connected, the RESET pin will be high
  // if the LCD is not connected, the RESET pin will be low
  auto lcd_detect = ReadGpio("gpio-display-detect");
  if (lcd_detect.is_error()) {
    fdf::error("Failed to determine display type");
    return lcd_detect.take_error();
  }

  fuchsia_driver_framework::NodeAddArgs args;

  if (*lcd_detect) {
    args.name() = "mipi-dsi-display";
    args.properties() = {
        fdf::MakeProperty(bind_fuchsia_display::OUTPUT, bind_fuchsia_display::OUTPUT_MIPI_DSI)};
  } else {
    args.name() = "hdmi-display";
    args.properties() = {
        fdf::MakeProperty(bind_fuchsia_display::OUTPUT, bind_fuchsia_display::OUTPUT_HDMI)};
  }

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    fdf::error("Failed to create controller endpoints: {}", controller_endpoints);
    return controller_endpoints.take_error();
  }
  controller_.Bind(std::move(controller_endpoints->client));

  auto result = parent_->AddChild({std::move(args), std::move(controller_endpoints->server), {}});
  if (result.is_error()) {
    if (result.error_value().is_framework_error()) {
      fdf::error("Failed to add child: {}",
                 result.error_value().framework_error().FormatDescription());
      return zx::error(result.error_value().framework_error().status());
    }
    if (result.error_value().is_domain_error()) {
      fdf::error("Failed to add child");
      return zx::error(ZX_ERR_INTERNAL);
    }
  }

  return zx::ok();
}

zx::result<bool> Vim3DisplayDetect::ReadGpio(std::string_view gpio_node_name) {
  zx::result gpio = incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(gpio_node_name);
  if (gpio.is_error()) {
    fdf::error("Failed to connect to GPIO node: {}", gpio);
    return gpio.take_error();
  }

  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> gpio_client(*std::move(gpio));

  auto result = gpio_client->SetBufferMode(fuchsia_hardware_gpio::BufferMode::kInput);
  if (!result.ok() || result->is_error()) {
    auto status = result.ok() ? result->error_value() : result.status();
    fdf::error("SetBufferMode failed: {}", zx::make_result(status));
    return zx::error(status);
  }

  auto read_byte = gpio_client->Read();
  if (!read_byte.ok() || read_byte->is_error()) {
    auto status = read_byte.ok() ? read_byte->error_value() : read_byte.status();
    fdf::error("Read failed: {}", zx::make_result(status));
    return zx::error(status);
  }

  return zx::ok(read_byte->value()->value);
}

}  // namespace vim3

FUCHSIA_DRIVER_EXPORT(vim3::Vim3DisplayDetect);
