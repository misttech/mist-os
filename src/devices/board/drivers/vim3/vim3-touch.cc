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
#include <lib/focaltech/focaltech.h>

#include <vector>

#include <bind/fuchsia/amlogic/platform/a311d/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/khadas/platform/cpp/bind.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "src/devices/board/drivers/vim3/vim3-gpios.h"
#include "src/devices/board/drivers/vim3/vim3.h"

namespace vim3 {

zx_status_t Vim3::TouchInit() {
  const ddk::BindRule kI2cRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_3),
      ddk::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                              bind_fuchsia_i2c::BIND_I2C_ADDRESS_FOCALTECH_TOUCH),
  };

  const device_bind_prop_t kI2cProperties[] = {
      ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia::I2C_ADDRESS,
                        bind_fuchsia_i2c::BIND_I2C_ADDRESS_FOCALTECH_TOUCH),
  };

  const ddk::BindRule kInterruptRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              static_cast<uint32_t>(VIM3_TOUCH_PANEL_INTERRUPT)),
  };

  const device_bind_prop_t kInterruptProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_TOUCH_INTERRUPT)};

  const ddk::BindRule kResetRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              static_cast<uint32_t>(VIM3_TOUCH_PANEL_RESET)),
  };

  const device_bind_prop_t kResetProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_TOUCH_RESET),
  };

  static const FocaltechMetadata device_info = {
      .device_id = FOCALTECH_DEVICE_FT5336,
      .needs_firmware = false,
  };

  static const device_metadata_t ft5336_khadas_ts050_touch_metadata[] = {
      {.type = DEVICE_METADATA_PRIVATE, .data = &device_info, .length = sizeof(device_info)},
  };

  auto status = DdkAddCompositeNodeSpec("ft5336_touch",
                                        ddk::CompositeNodeSpec(kI2cRules, kI2cProperties)
                                            .AddParentSpec(kInterruptRules, kInterruptProperties)
                                            .AddParentSpec(kResetRules, kResetProperties)
                                            .set_metadata(ft5336_khadas_ts050_touch_metadata));
  if (status != ZX_OK) {
    zxlogf(ERROR, "ft5336: DdkAddCompositeNodeSpec failed: %s", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

}  // namespace vim3

#endif  // SRC_DEVICES_BOARD_DRIVERS_VIM3_VIM3_TOUCH_H_
