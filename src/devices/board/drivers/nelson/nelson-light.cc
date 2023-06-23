// Copyright 2018 The Fuchsia Authors. All rights reserved.
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
#include <bind/fuchsia/ams/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/pwm/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <ddk/metadata/lights.h>
#include <ddktl/metadata/light-sensor.h>
#include <soc/aml-s905d2/s905d2-gpio.h>
#include <soc/aml-s905d3/s905d3-pwm.h>

#include "nelson-gpios.h"
#include "nelson.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

// Composite binding rules for focaltech touch driver.

using LightName = char[ZX_MAX_NAME_LEN];
constexpr LightName kLightNames[] = {"AMBER_LED"};
constexpr LightsConfig kConfigs[] = {
    {.brightness = true, .rgb = false, .init_on = true, .group_id = -1},
};

static const std::vector<fpbus::Metadata> light_metadata{
    {{
        .type = DEVICE_METADATA_NAME,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&kLightNames),
            reinterpret_cast<const uint8_t*>(&kLightNames) + sizeof(kLightNames)),
    }},
    {{
        .type = DEVICE_METADATA_LIGHTS,
        .data =
            std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&kConfigs),
                                 reinterpret_cast<const uint8_t*>(&kConfigs) + sizeof(kConfigs)),
    }},
};

static const fpbus::Node light_dev = []() {
  fpbus::Node result = {};
  result.name() = "gpio-light";
  result.vid() = PDEV_VID_AMLOGIC;
  result.pid() = PDEV_PID_GENERIC;
  result.did() = PDEV_DID_GPIO_LIGHT;
  result.metadata() = light_metadata;
  return result;
}();

zx_status_t Nelson::LightInit() {
  metadata::LightSensorParams params = {};
  // TODO(kpt): Insert the right parameters here.
  params.integration_time_us = 711'680;
  params.gain = 64;
  params.polling_time_us = 700'000;
  device_metadata_t metadata[] = {
      {
          .type = DEVICE_METADATA_PRIVATE,
          .data = &params,
          .length = sizeof(params),
      },
  };

  const ddk::BindRule kI2cRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_A0_0),
      ddk::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                              bind_fuchsia_i2c::BIND_I2C_ADDRESS_AMBIENTLIGHT),
  };
  const device_bind_prop_t kI2cProperties[] = {
      ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_A0_0),
      ddk::MakeProperty(bind_fuchsia::I2C_ADDRESS, bind_fuchsia_i2c::BIND_I2C_ADDRESS_AMBIENTLIGHT),
  };

  const ddk::BindRule kGpioLightInterruptRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_s905d3::GPIOAO_PIN_ID_PIN_5),
  };
  const device_bind_prop_t kGpioLightInterruptProperties[] = {
      ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
      ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_LIGHT_INTERRUPT),
  };

  zx_status_t status = DdkAddCompositeNodeSpec(
      "tcs3400_light", ddk::CompositeNodeSpec(kI2cRules, kI2cProperties)
                           .AddParentSpec(kGpioLightInterruptRules, kGpioLightInterruptProperties)
                           .set_metadata(metadata));

  if (status != ZX_OK) {
    zxlogf(ERROR, "%s(tcs-3400): DdkAddCompositeNodeSpec failed: %d", __func__, status);
    return status;
  }

  // Enable the Amber LED so it will be controlled by PWM.
  status = gpio_impl_.SetAltFunction(GPIO_AMBER_LED_PWM, 3);  // Set as PWM.
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Configure mute LED GPIO failed %d", __func__, status);
  }

  // GPIO must be set to default out otherwise could cause light to not work
  // on certain reboots.
  status = gpio_impl_.ConfigOut(GPIO_AMBER_LED_PWM, 1);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Configure mute LED GPIO on failed %d", __func__, status);
  }

  auto amber_led_gpio_bind_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_s905d3::GPIOAO_PIN_ID_PIN_11),
  };

  auto amber_led_gpio_properties = std::vector{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_GPIO_AMBER_LED),
  };

  auto amber_led_pwm_bind_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::PWM_ID,
                              bind_fuchsia_amlogic_platform_s905d3::BIND_PWM_ID_PWM_AO_A),
  };

  auto amber_led_pwm_properties = std::vector{
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION,
                        bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION_AMBER_LED),
  };

  auto parents = std::vector{
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = amber_led_gpio_bind_rules,
          .properties = amber_led_gpio_properties,
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = amber_led_pwm_bind_rules,
          .properties = amber_led_pwm_properties,
      }},
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('LIGH');
  auto aml_light_spec =
      fuchsia_driver_framework::CompositeNodeSpec{{.name = "aml_light", .parents = parents}};
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, light_dev),
                                                          fidl::ToWire(fidl_arena, aml_light_spec));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Light(aml_light) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Light(aml_light) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace nelson
