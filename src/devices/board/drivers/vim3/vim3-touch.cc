// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_DRIVERS_VIM3_VIM3_TOUCH_H_
#define SRC_DEVICES_BOARD_DRIVERS_VIM3_VIM3_TOUCH_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/focaltech/focaltech.h>

#include <vector>

#include <bind/fuchsia/amlogic/platform/a311d/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/focaltech/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/i2c/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/khadas/platform/cpp/bind.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "src/devices/board/drivers/vim3/vim3-gpios.h"
#include "src/devices/board/drivers/vim3/vim3.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf
   //
namespace vim3 {

namespace fpbus = fuchsia_hardware_platform_bus;

zx_status_t Vim3::TouchInit() {
  const std::vector<fdf::BindRule> kI2cRules{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                              bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_3),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                              bind_fuchsia_focaltech_platform::BIND_I2C_ADDRESS_TOUCH),
  };

  const std::vector<fdf::NodeProperty> kI2cProperties{
      fdf::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                        bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS,
                        bind_fuchsia_focaltech_platform::BIND_I2C_ADDRESS_TOUCH),
  };

  const std::vector<fdf::BindRule> kInterruptRules{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, VIM3_GPIO_ID),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              static_cast<uint32_t>(VIM3_TOUCH_PANEL_INTERRUPT)),
  };

  const std::vector<fdf::NodeProperty> kInterruptProperties{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_INTERRUPT)};

  const std::vector<fdf::BindRule> kResetRules{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, VIM3_EXPANDER_GPIO_ID),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              static_cast<uint32_t>(VIM3_TOUCH_PANEL_RESET)),
  };

  const std::vector<fdf::NodeProperty> kResetProperties{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_RESET),
  };

  static const FocaltechMetadata device_info{
      .device_id = FOCALTECH_DEVICE_FT5336,
      .needs_firmware = false,
  };

  fpbus::Node dev;
  dev.name() = "ft5336_touch";
  dev.vid() = PDEV_VID_GENERIC;
  dev.pid() = PDEV_VID_GENERIC;
  dev.did() = PDEV_DID_FOCALTOUCH;
  dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&device_info),
              reinterpret_cast<const uint8_t*>(&device_info) + sizeof(device_info)),
      }},
  };
  auto parents = std::vector{
      fdf::ParentSpec{{kI2cRules, kI2cProperties}},
      fdf::ParentSpec{{kInterruptRules, kInterruptProperties}},
      fdf::ParentSpec{{kResetRules, kResetProperties}},
  };
  auto composite_node_spec = fdf::CompositeNodeSpec{{.name = "ft5336_touch", .parents = parents}};

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('FOCL');
  fdf::WireUnownedResult result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, dev), fidl::ToWire(fidl_arena, composite_node_spec));
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddCompositeNodeSpec request: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add composite node spec: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

}  // namespace vim3

#endif  // SRC_DEVICES_BOARD_DRIVERS_VIM3_VIM3_TOUCH_H_
