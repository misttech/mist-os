// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vim3-gpio-buttons.h"

#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/logger.h>

#include <ddk/metadata/buttons.h>

namespace vim3_dt {

zx::result<> Vim3GpioButtonsVisitor::DriverVisit(fdf_devicetree::Node& node,
                                                 const devicetree::PropertyDecoder& decoder) {
  const buttons_button_config_t buttons[] = {
      {BUTTONS_TYPE_DIRECT, BUTTONS_ID_POWER, 0, 0, 0},
  };

  fuchsia_hardware_platform_bus::Metadata button_config = {
      {.type = DEVICE_METADATA_BUTTONS_BUTTONS,
       .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&buttons),
                                    reinterpret_cast<const uint8_t*>(&buttons) + sizeof(buttons))}};

  node.AddMetadata(button_config);

  const buttons_gpio_config_t gpios[] = {
      {BUTTONS_GPIO_TYPE_INTERRUPT,
       BUTTONS_GPIO_FLAG_INVERTED | BUTTONS_GPIO_FLAG_WAKE_VECTOR,
       {.interrupt = {static_cast<uint32_t>(fuchsia_hardware_gpio::GpioFlags::kNoPull)}}},
  };

  fuchsia_hardware_platform_bus::Metadata button_gpio_config = {
      {.type = DEVICE_METADATA_BUTTONS_GPIOS,
       .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&gpios),
                                    reinterpret_cast<const uint8_t*>(&gpios) + sizeof(gpios))}};

  node.AddMetadata(button_gpio_config);

  return zx::ok();
}

}  // namespace vim3_dt
