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
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/focaltech/focaltech.h>
#include <limits.h>
#include <unistd.h>

#include <bind/fuchsia/amlogic/platform/s905d2/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/focaltech/platform/cpp/bind.h>
#include <bind/fuchsia/goodix/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <fbl/algorithm.h>
#include <soc/aml-s905d2/s905d2-gpio.h>
#include <soc/aml-s905d2/s905d2-hw.h>

#include "astro-gpios.h"
#include "astro.h"

namespace astro {
namespace fpbus = fuchsia_hardware_platform_bus;

const std::vector kFocaltechI2cRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_2),
    fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                            bind_fuchsia_focaltech_platform::BIND_I2C_ADDRESS_TOUCH),
};

const std::vector kFocaltechI2cProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS,
                      bind_fuchsia_focaltech_platform::BIND_I2C_ADDRESS_TOUCH),
};

const std::vector kFocaltechInterruptRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            bind_fuchsia_amlogic_platform_s905d2::GPIOZ_PIN_ID_PIN_4),
};

const std::vector kFocaltechInterruptProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_INTERRUPT)};

const std::vector kFocaltechResetRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            bind_fuchsia_amlogic_platform_s905d2::GPIOZ_PIN_ID_PIN_9),
};

const std::vector kFocaltechResetProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_RESET),
};

const ddk::BindRule kGoodixI2cRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_2),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                            bind_fuchsia_goodix_platform::BIND_I2C_ADDRESS_TOUCH),
};

const device_bind_prop_t kGoodixI2cProperties[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeProperty(bind_fuchsia::I2C_ADDRESS,
                      bind_fuchsia_goodix_platform::BIND_I2C_ADDRESS_TOUCH),
};

const ddk::BindRule kGoodixInterruptRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            bind_fuchsia_amlogic_platform_s905d2::GPIOZ_PIN_ID_PIN_4),
};

const device_bind_prop_t kGoodixInterruptProperties[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    ddk::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_INTERRUPT)};

const ddk::BindRule kGoodixResetRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            bind_fuchsia_amlogic_platform_s905d2::GPIOZ_PIN_ID_PIN_9),
};

const device_bind_prop_t kGoodixResetProperties[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    ddk::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_RESET),
};

zx_status_t AddFocaltechTouch(
    fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  static const FocaltechMetadata device_info = {
      .device_id = FOCALTECH_DEVICE_FT3X27,
      .needs_firmware = false,
  };

  fpbus::Node dev;
  dev.name() = "focaltech_touch";
  dev.vid() = PDEV_VID_GENERIC;
  dev.pid() = PDEV_PID_GENERIC;
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
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = kFocaltechI2cRules,
          .properties = kFocaltechI2cProperties,
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = kFocaltechInterruptRules,
          .properties = kFocaltechInterruptProperties,
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = kFocaltechResetRules,
          .properties = kFocaltechResetProperties,
      }},
  };

  auto composite_node_spec =
      fuchsia_driver_framework::CompositeNodeSpec{{.name = "focaltech_touch", .parents = parents}};

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('FOCL');
  fdf::WireUnownedResult result = pbus.buffer(arena)->AddCompositeNodeSpec(
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
                            .AddParentSpec(kGoodixInterruptRules, kGoodixInterruptProperties)
                            .AddParentSpec(kGoodixResetRules, kGoodixResetProperties));
    if (status != ZX_OK) {
      zxlogf(INFO, "gt92xx: DdkAddCompositeNodeSpec failed: %s", zx_status_get_string(status));
      return status;
    }
  } else {
    auto status = AddFocaltechTouch(pbus_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "ft3x27: DdkAddCompositeNodeSpec failed: %s", zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}

}  // namespace astro
