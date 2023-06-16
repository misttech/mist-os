// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>

#include <bind/fuchsia/amlogic/platform/a311d/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/pwm/cpp/bind.h>
#include <bind/fuchsia/sysmem/cpp/bind.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "src/devices/board/drivers/vim3/vim3-gpios.h"
#include "src/devices/board/drivers/vim3/vim3.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace vim3 {

zx_status_t Vim3::BacklightInit() {
  if (!HasLcd()) {
    return ZX_OK;
  }

  const ddk::BindRule gpio_lcd_reset_bind_rules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              static_cast<uint32_t>(VIM3_LCD_BACKLIGHT_ENABLE)),
  };

  const device_bind_prop_t gpio_lcd_reset_properties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_LCD_BACKLIGHT_ENABLE),
  };

  const ddk::BindRule pwm_bind_rules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::PWM_ID,
                              bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_AO_C)};

  const device_bind_prop_t pwm_properties[] = {
      ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION,
                        bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION_LCD_BRIGHTNESS),
  };

  auto status = DdkAddCompositeNodeSpec(
      "backlight", ddk::CompositeNodeSpec(gpio_lcd_reset_bind_rules, gpio_lcd_reset_properties)
                       .AddParentSpec(pwm_bind_rules, pwm_properties));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAddCompositeNodeSpec failed: %d", status);
    return status;
  }
  return ZX_OK;
}

}  // namespace vim3
