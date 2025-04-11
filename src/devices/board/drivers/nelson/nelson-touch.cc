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

#include <bind/fuchsia/amlogic/platform/s905d3/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/goodix/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/i2c/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>

#include "nelson-gpios.h"
#include "nelson.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {
const std::vector kI2cRules = {
    fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_i2c::SERVICE,
                             bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule2(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_2),
    fdf::MakeAcceptBindRule2(bind_fuchsia::I2C_ADDRESS,
                             bind_fuchsia_goodix_platform::BIND_I2C_ADDRESS_TOUCH),
};

const std::vector kI2cProperties = {
    fdf::MakeProperty2(bind_fuchsia_hardware_i2c::SERVICE,
                       bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty2(bind_fuchsia::I2C_ADDRESS,
                       bind_fuchsia_goodix_platform::BIND_I2C_ADDRESS_TOUCH),
};

const std::vector kInterruptRules = {
    fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_gpio::SERVICE,
                             bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule2(bind_fuchsia::GPIO_PIN,
                             bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_4),
};

const std::vector kInterruptProperties = {
    fdf::MakeProperty2(bind_fuchsia_hardware_gpio::SERVICE,
                       bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty2(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_INTERRUPT)};

const std::vector kResetRules = {
    fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_gpio::SERVICE,
                             bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule2(bind_fuchsia::GPIO_PIN,
                             bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_9),
};

const std::vector kResetProperties = {
    fdf::MakeProperty2(bind_fuchsia_hardware_gpio::SERVICE,
                       bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty2(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_RESET),
};

const std::vector kGpioInitRules = {
    fdf::MakeAcceptBindRule2(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

const std::vector kGpioInitProperties = {
    fdf::MakeProperty2(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};
}  // namespace

static const std::vector<fpbus::BootMetadata> touch_boot_metadata{
    {{
        .zbi_type = DEVICE_METADATA_BOARD_PRIVATE,
        .zbi_extra = 0,
    }},
};

zx_status_t Nelson::TouchInit() {
  gpio_init_steps_.push_back(GpioPull(GPIO_TOUCH_SOC_INT_L, fuchsia_hardware_pin::Pull::kNone));

  fpbus::Node touch_dev;
  touch_dev.name() = "gt6853-touch";
  touch_dev.vid() = PDEV_VID_GOODIX;
  touch_dev.did() = PDEV_DID_GOODIX_GT6853;
  touch_dev.boot_metadata() = touch_boot_metadata;

  auto parents = std::vector{
      fuchsia_driver_framework::ParentSpec2{{
          .bind_rules = kI2cRules,
          .properties = kI2cProperties,
      }},
      fuchsia_driver_framework::ParentSpec2{{
          .bind_rules = kInterruptRules,
          .properties = kInterruptProperties,
      }},
      fuchsia_driver_framework::ParentSpec2{{
          .bind_rules = kResetRules,
          .properties = kResetProperties,
      }},
      fuchsia_driver_framework::ParentSpec2{{
          .bind_rules = kGpioInitRules,
          .properties = kGpioInitProperties,
      }},
  };

  auto composite_node_spec =
      fuchsia_driver_framework::CompositeNodeSpec{{.name = "gt6853_touch", .parents2 = parents}};

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('TOUC');
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, touch_dev), fidl::ToWire(fidl_arena, composite_node_spec));
  if (!result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Touch(touch_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Touch(touch_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

}  // namespace nelson
