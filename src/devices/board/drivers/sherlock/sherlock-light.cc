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
#include <zircon/compiler.h>

#include <bind/fuchsia/amlogic/platform/t931/cpp/bind.h>
#include <bind/fuchsia/ams/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/pwm/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <ddk/metadata/lights.h>
#include <ddktl/metadata/light-sensor.h>
#include <soc/aml-t931/t931-pwm.h>

#include "sherlock-gpios.h"
#include "sherlock.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

zx_status_t Sherlock::LightInit() {
  metadata::LightSensorParams params = {};
  // TODO(kpt): Insert the right parameters here.
  params.integration_time_us = 711'680;
  params.gain = 64;
  params.polling_time_us = 700'000;
  const std::vector<fpbus::Metadata> kTcs3400Metadata{
      {{.type = DEVICE_METADATA_PRIVATE,
        .data = std::vector<uint8_t>(reinterpret_cast<uint8_t*>(&params),
                                     reinterpret_cast<uint8_t*>(&params) + sizeof(params))}},
  };

  fpbus::Node tcs3400_light_dev;
  tcs3400_light_dev.name() = "tcs3400_light";
  tcs3400_light_dev.vid() = PDEV_VID_GENERIC;
  tcs3400_light_dev.pid() = PDEV_PID_GENERIC;
  tcs3400_light_dev.did() = PDEV_DID_TCS3400_LIGHT;
  tcs3400_light_dev.metadata() = kTcs3400Metadata;

  const auto kI2cBindRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_A0_0),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                              bind_fuchsia_i2c::BIND_I2C_ADDRESS_AMBIENTLIGHT),
  };
  const auto kI2cProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_A0_0),
      fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS, bind_fuchsia_i2c::BIND_I2C_ADDRESS_AMBIENTLIGHT),
  };

  const auto kGpioLightInterruptRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_t931::GPIOAO_PIN_ID_PIN_5),
  };
  const auto kGpioLightInterruptProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_LIGHT_INTERRUPT),
  };

  auto kTcs3400LightParents = std::vector{
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = kI2cBindRules,
          .properties = kI2cProperties,
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = kGpioLightInterruptRules,
          .properties = kGpioLightInterruptProperties,
      }},
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena tcs3400_light_arena('TCS3');

  auto tcs3400_light_spec = fuchsia_driver_framework::CompositeNodeSpec{
      {.name = "tcs3400_light", .parents = kTcs3400LightParents}};
  fdf::WireUnownedResult tsc3400_light_result =
      pbus_.buffer(tcs3400_light_arena)
          ->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, tcs3400_light_dev),
                                 fidl::ToWire(fidl_arena, tcs3400_light_spec));
  if (!tsc3400_light_result.ok()) {
    zxlogf(ERROR, "Failed to send AddCompositeNodeSpec request to platform bus: %s",
           tsc3400_light_result.status_string());
    return tsc3400_light_result.status();
  }
  if (tsc3400_light_result->is_error()) {
    zxlogf(ERROR, "Failed to add tcs3400_light composite node spec to platform device: %s",
           zx_status_get_string(tsc3400_light_result->error_value()));
    return tsc3400_light_result->error_value();
  }

  // Lights
  // Instructions: include fragments in this order
  //     GPIO fragment
  //     BRIGHTNESS capable--include PWM fragment
  //     RGB capable--include RGB fragment
  //   Set GPIO alternative function here!
  using LightName = char[ZX_MAX_NAME_LEN];
  constexpr LightName kLightNames[] = {"AMBER_LED", "GREEN_LED"};
  constexpr LightsConfig kConfigs[] = {
      {.brightness = true, .rgb = false, .init_on = true, .group_id = -1},
      {.brightness = true, .rgb = false, .init_on = false, .group_id = -1},
  };
  std::vector<fpbus::Metadata> light_metadata{
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

  fpbus::Node light_dev;
  light_dev.name() = "gpio-light";
  light_dev.vid() = PDEV_VID_AMLOGIC;
  light_dev.pid() = PDEV_PID_GENERIC;
  light_dev.did() = PDEV_DID_GPIO_LIGHT;
  light_dev.metadata() = light_metadata;

  // Enable the Amber LED so it will be controlled by PWM.
  zx_status_t status = gpio_impl_.SetAltFunction(GPIO_AMBER_LED, 3);  // Set as GPIO.
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Configure mute LED GPIO failed %d", __func__, status);
  }
  status = gpio_impl_.ConfigOut(GPIO_AMBER_LED, 1);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Configure mute LED GPIO on failed %d", __func__, status);
  }
  // Enable the Green LED so it will be controlled by PWM.
  status = gpio_impl_.SetAltFunction(GPIO_GREEN_LED, 4);  // Set as PWM.
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Configure mute LED GPIO failed %d", __func__, status);
  }
  status = gpio_impl_.ConfigOut(GPIO_GREEN_LED, 1);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Configure mute LED GPIO on failed %d", __func__, status);
  }

  auto amber_led_gpio_bind_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_t931::GPIOAO_PIN_ID_PIN_11),
  };

  auto amber_led_gpio_properties = std::vector{
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_GPIO_AMBER_LED),
  };

  auto amber_led_pwm_bind_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::PWM_ID,
                              bind_fuchsia_amlogic_platform_t931::BIND_PWM_ID_PWM_AO_A),
  };

  auto amber_led_pwm_properties = std::vector{
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION,
                        bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION_AMBER_LED),
  };

  auto green_led_gpio_bind_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_t931::GPIOH_PIN_ID_PIN_5),
  };

  auto green_led_gpio_properties = std::vector{
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_GPIO_GREEN_LED),
  };

  auto green_led_pwm_bind_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::PWM_ID,
                              bind_fuchsia_amlogic_platform_t931::BIND_PWM_ID_PWM_F),
  };

  auto green_led_pwm_properties = std::vector{
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION,
                        bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION_GREEN_LED),
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
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = green_led_gpio_bind_rules,
          .properties = green_led_gpio_properties,
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = green_led_pwm_bind_rules,
          .properties = green_led_pwm_properties,
      }},
  };

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

}  // namespace sherlock
