// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.pin/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/focaltech/focaltech.h>

#include <string>

#include <bind/fuchsia/amlogic/platform/s905d2/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/focaltech/platform/cpp/bind.h>
#include <bind/fuchsia/goodix/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/i2c/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>

#include "src/devices/board/drivers/astro/post-init/post-init.h"
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

}  // namespace

namespace astro {
namespace fpbus = fuchsia_hardware_platform_bus;

const std::vector kFocaltechI2cRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                            bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_2),
    fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                            bind_fuchsia_focaltech_platform::BIND_I2C_ADDRESS_TOUCH),
};

const std::vector kFocaltechI2cProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                      bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS,
                      bind_fuchsia_focaltech_platform::BIND_I2C_ADDRESS_TOUCH),
};

const std::vector kGoodixI2cRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                            bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_2),
    fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                            bind_fuchsia_goodix_platform::BIND_I2C_ADDRESS_TOUCH),
};

const std::vector kGoodixI2cProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                      bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS,
                      bind_fuchsia_goodix_platform::BIND_I2C_ADDRESS_TOUCH),
};

const std::vector kInterruptRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                            bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            bind_fuchsia_amlogic_platform_s905d2::GPIOZ_PIN_ID_PIN_4),
};

const std::vector kInterruptProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                      bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_INTERRUPT)};

const std::vector kResetRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                            bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            bind_fuchsia_amlogic_platform_s905d2::GPIOZ_PIN_ID_PIN_9),
};

const std::vector kResetProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                      bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_RESET),
};

const std::vector kGpioInitRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

const std::vector kGpioInitProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

zx::result<> AddFocaltechTouch(
    fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  static const FocaltechMetadata device_info = {
      .device_id = FOCALTECH_DEVICE_FT3X27,
      .needs_firmware = false,
  };

  fpbus::Node dev;
  dev.name() = "focaltech_touch";
  dev.vid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC;
  dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  dev.did() = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_FOCALTOUCH;
  dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .id = std::to_string(DEVICE_METADATA_PRIVATE),
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
          .bind_rules = kInterruptRules,
          .properties = kInterruptProperties,
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = kResetRules,
          .properties = kResetProperties,
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = kGpioInitRules,
          .properties = kGpioInitProperties,
      }},
  };

  auto composite_node_spec =
      fuchsia_driver_framework::CompositeNodeSpec{{.name = "focaltech_touch", .parents = parents}};

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('FOCL');
  fdf::WireUnownedResult result = pbus.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, dev), fidl::ToWire(fidl_arena, composite_node_spec));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send AddCompositeNodeSpec request: %s", result.status_string());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to add composite node spec: %s",
            zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  return zx::ok();
}

zx::result<> PostInit::InitTouch() {
  /* Two variants of display are supported, one with BOE display panel and
        ft3x27 touch controller, the other with INX panel and Goodix touch
        controller.  This GPIO input is used to identify each.
        Logic 0 for BOE/ft3x27 combination
        Logic 1 for Innolux/Goodix combination
  */

  if (display_id_) {
    // The Goodix touch driver expects the interrupt line to be pulled up and the reset line to be
    // pulled down.
    if (auto result = SetPull(incoming(), "touch-interrupt", fuchsia_hardware_pin::Pull::kUp);
        result.is_error()) {
      return result;
    }
    if (auto result = SetPull(incoming(), "touch-reset", fuchsia_hardware_pin::Pull::kDown);
        result.is_error()) {
      return result;
    }

    const std::vector<fuchsia_driver_framework::ParentSpec> goodix_parents{
        {{kGoodixI2cRules, kGoodixI2cProperties}},
        {{kInterruptRules, kInterruptProperties}},
        {{kResetRules, kResetProperties}},
    };

    const fuchsia_driver_framework::CompositeNodeSpec goodix_node_spec{{
        .name = "gt92xx_touch",
        .parents = goodix_parents,
    }};

    if (auto result = composite_manager_->AddSpec(goodix_node_spec); result.is_error()) {
      if (result.error_value().is_framework_error()) {
        FDF_LOG(ERROR, "Call to AddSpec failed: %s",
                result.error_value().framework_error().FormatDescription().c_str());
        return zx::error(result.error_value().framework_error().status());
      }
      if (result.error_value().is_domain_error()) {
        FDF_LOG(ERROR, "AddSpec failed");
        return zx::error(ZX_ERR_INTERNAL);
      }
    }
  } else {
    // The Focaltech touch driver expects the interrupt line to be driven by the touch controller.
    if (auto result = SetPull(incoming(), "touch-interrupt", fuchsia_hardware_pin::Pull::kNone);
        result.is_error()) {
      return result;
    }

    auto status = AddFocaltechTouch(pbus_);
    if (!status.is_ok()) {
      FDF_LOG(ERROR, "ft3x27: DdkAddCompositeNodeSpec failed: %s", status.status_string());
      return status;
    }
  }

  return zx::ok();
}

}  // namespace astro
