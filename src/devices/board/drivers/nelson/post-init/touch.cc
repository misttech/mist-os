// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.pin/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/zx/result.h>

#include <bind/fuchsia/amlogic/platform/s905d3/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/goodix/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/i2c/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>

#include "post-init.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

zx::result<> SetPull(std::shared_ptr<fdf::Namespace> incoming, std::string_view node_name,
                     fuchsia_hardware_pin::Pull pull) {
  zx::result pin = incoming->Connect<fuchsia_hardware_pin::Service::Device>(node_name);
  if (pin.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to pin node: %s", pin.status_string());
    return pin.take_error();
  }

  fidl::Arena arena;
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena).pull(pull).Build();
  fidl::WireResult result = fidl::WireCall(*pin)->Configure(config);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Call to Configure failed: %s", result.FormatDescription().c_str());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Configure failed: %s", result.FormatDescription().c_str());
    return result->take_error();
  }
  return zx::ok();
}

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

zx::result<> PostInit::InitTouch() {
  // The Goodix touch driver expects the interrupt line to be driven by the touch controller.
  if (auto result = SetPull(incoming(), "touch-interrupt", fuchsia_hardware_pin::Pull::kNone);
      result.is_error()) {
    return result;
  }

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
    FDF_LOG(ERROR, "AddCompositeNodeSpec Touch(touch_dev) request failed: %s",
            result.FormatDescription().data());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "AddCompositeNodeSpec Touch(touch_dev) failed: %s",
            zx_status_get_string(result->error_value()));
    return zx::error(result->error_value());
  }
  return zx::ok();
}

}  // namespace nelson
