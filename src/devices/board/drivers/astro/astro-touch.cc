// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/focaltech/focaltech.h>
#include <limits.h>
#include <unistd.h>

#include <bind/fuchsia/amlogic/platform/s905d2/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <fbl/algorithm.h>
#include <soc/aml-s905d2/s905d2-gpio.h>
#include <soc/aml-s905d2/s905d2-hw.h>

#include "astro-gpios.h"
#include "astro.h"

namespace astro {
namespace fpbus = fuchsia_hardware_platform_bus;

static const FocaltechMetadata device_info = {
    .device_id = FOCALTECH_DEVICE_FT3X27,
    .needs_firmware = false,
};
static const device_metadata_t ft3x27_touch_metadata[] = {
    {.type = DEVICE_METADATA_PRIVATE, .data = &device_info, .length = sizeof(device_info)},
};

const ddk::BindRule kFocaltechI2cRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_2),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                            bind_fuchsia_i2c::BIND_I2C_ADDRESS_FOCALTECH_TOUCH),
};

const device_bind_prop_t kFocaltechI2cProperties[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeProperty(bind_fuchsia::I2C_ADDRESS,
                      bind_fuchsia_i2c::BIND_I2C_ADDRESS_FOCALTECH_TOUCH),
};

const ddk::BindRule kGoodixI2cRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_2),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                            bind_fuchsia_i2c::BIND_I2C_ADDRESS_GOODIX_TOUCH),
};

const device_bind_prop_t kGoodixI2cProperties[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeProperty(bind_fuchsia::I2C_ADDRESS, bind_fuchsia_i2c::BIND_I2C_ADDRESS_GOODIX_TOUCH),
};

const ddk::BindRule kInterruptRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                            bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            bind_fuchsia_amlogic_platform_s905d2::GPIOZ_PIN_ID_PIN_4),
};

const device_bind_prop_t kInterruptProperties[] = {
    ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
    ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                      bind_fuchsia_hardware_gpio::FUNCTION_TOUCH_INTERRUPT)};

const ddk::BindRule kResetRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                            bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            bind_fuchsia_amlogic_platform_s905d2::GPIOZ_PIN_ID_PIN_9),
};

const device_bind_prop_t kResetProperties[] = {
    ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
    ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                      bind_fuchsia_hardware_gpio::FUNCTION_TOUCH_RESET),
};

zx_status_t Astro::TouchInit() {
  // Check the display ID pin to determine which driver device to add
  gpio_impl_.SetAltFunction(S905D2_GPIOH(5), 0);
  gpio_impl_.ConfigIn(S905D2_GPIOH(5), GPIO_NO_PULL);
  uint8_t gpio_state;
  /* Two variants of display are supported, one with BOE display panel and
        ft3x27 touch controller, the other with INX panel and Goodix touch
        controller.  This GPIO input is used to identify each.
        Logic 0 for BOE/ft3x27 combination
        Logic 1 for Innolux/Goodix combination
  */
  gpio_impl_.Read(S905D2_GPIOH(5), &gpio_state);

  if (gpio_state) {
    auto status = DdkAddCompositeNodeSpec(
        "gt92xx_touch", ddk::CompositeNodeSpec(kGoodixI2cRules, kGoodixI2cProperties)
                            .AddParentSpec(kInterruptRules, kInterruptProperties)
                            .AddParentSpec(kResetRules, kResetProperties));
    if (status != ZX_OK) {
      zxlogf(INFO, "gt92xx: DdkAddCompositeNodeSpec failed: %s", zx_status_get_string(status));
      return status;
    }
  } else {
    auto status = DdkAddCompositeNodeSpec(
        "focaltech_touch", ddk::CompositeNodeSpec(kFocaltechI2cRules, kFocaltechI2cProperties)
                               .AddParentSpec(kInterruptRules, kInterruptProperties)
                               .AddParentSpec(kResetRules, kResetProperties)
                               .set_metadata(ft3x27_touch_metadata));
    if (status != ZX_OK) {
      zxlogf(ERROR, "ft3x27: DdkAddCompositeNodeSpec failed: %s", zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}

}  // namespace astro
