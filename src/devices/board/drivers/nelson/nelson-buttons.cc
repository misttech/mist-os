// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
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
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <ddk/metadata/buttons.h>
#include <ddktl/device.h>
#include <soc/aml-s905d2/s905d2-gpio.h>
#include <soc/aml-s905d2/s905d2-hw.h>

#include "lib/fidl_driver/cpp/wire_messaging_declarations.h"
#include "nelson-gpios.h"
#include "src/devices/board/drivers/nelson/nelson.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

// clang-format off
static const buttons_button_config_t buttons[] = {
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_VOLUME_UP,   0, 0, 0},
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_VOLUME_DOWN, 1, 0, 0},
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_FDR,         2, 0, 0},
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_MIC_MUTE,    3, 0, 0},
};
// clang-format on

// No need for internal pull, external pull-ups used.
static const buttons_gpio_config_t gpios[] = {
    {BUTTONS_GPIO_TYPE_INTERRUPT, BUTTONS_GPIO_FLAG_INVERTED, {}},
    {BUTTONS_GPIO_TYPE_INTERRUPT, BUTTONS_GPIO_FLAG_INVERTED, {}},
    {BUTTONS_GPIO_TYPE_POLL, BUTTONS_GPIO_FLAG_INVERTED, {.poll = {zx::msec(20).get()}}},
    {BUTTONS_GPIO_TYPE_POLL, 0, {.poll = {zx::msec(20).get()}}},
};

zx_status_t Nelson::ButtonsInit() {
  auto button_pin = [](uint32_t pin, fuchsia_hardware_pin::Pull pull) {
    return fuchsia_hardware_pinimpl::InitStep::WithCall({{
        .pin = pin,
        .call = fuchsia_hardware_pinimpl::InitCall::WithPinConfig({{
            .pull = pull,
            .function = 0,
        }}),
    }});
  };

  gpio_init_steps_.push_back(button_pin(GPIO_VOL_UP_L, fuchsia_hardware_pin::Pull::kUp));
  gpio_init_steps_.push_back(button_pin(GPIO_VOL_DN_L, fuchsia_hardware_pin::Pull::kUp));
  gpio_init_steps_.push_back(button_pin(GPIO_FDR_L, fuchsia_hardware_pin::Pull::kNone));
  gpio_init_steps_.push_back(button_pin(GPIO_MUTE_SOC, fuchsia_hardware_pin::Pull::kNone));

  fidl::Arena<> fidl_arena;
  fdf::Arena buttons_arena('BTTN');

  fpbus::Node dev = {{.name = "nelson-buttons",
                      .vid = PDEV_VID_GENERIC,
                      .pid = PDEV_PID_GENERIC,
                      .did = PDEV_DID_BUTTONS,
                      .metadata = std::vector<fpbus::Metadata>{
                          {{.id = std::to_string(DEVICE_METADATA_BUTTONS_BUTTONS),
                            .data = std::vector<uint8_t>(
                                reinterpret_cast<const uint8_t*>(&buttons),
                                reinterpret_cast<const uint8_t*>(&buttons) + sizeof(buttons))}},
                          {{.id = std::to_string(DEVICE_METADATA_BUTTONS_GPIOS),
                            .data = std::vector<uint8_t>(
                                reinterpret_cast<const uint8_t*>(&gpios),
                                reinterpret_cast<const uint8_t*>(&gpios) + sizeof(gpios))}}}}};

  const std::vector<fuchsia_driver_framework::BindRule> kGpioInitRules = {
      fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };
  const std::vector<fuchsia_driver_framework::NodeProperty> kGpioInitProps = {
      fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };

  const std::vector<fuchsia_driver_framework::BindRule> kVolUpRules = {
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_5)};
  const std::vector<fuchsia_driver_framework::NodeProperty> kVolUpProps = {
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                        bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_VOLUME_UP),
  };

  const std::vector<fuchsia_driver_framework::BindRule> kVolDownRules = {
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_6)};
  const std::vector<fuchsia_driver_framework::NodeProperty> kVolDownProps = {
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                        bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_VOLUME_DOWN),
  };

  const std::vector<fuchsia_driver_framework::BindRule> kVolBothRules = {
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_s905d3::GPIOAO_PIN_ID_PIN_10)};
  const std::vector<fuchsia_driver_framework::NodeProperty> kVolBothProps = {
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                        bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_VOLUME_BOTH),
  };

  const std::vector<fuchsia_driver_framework::BindRule> kMicPrivacyRules = {
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_2)};
  const std::vector<fuchsia_driver_framework::NodeProperty> kMicPrivacyProps = {
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                        bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_MIC_MUTE),
  };

  std::vector<fuchsia_driver_framework::ParentSpec> parents = {
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = std::move(kGpioInitRules),
          .properties = std::move(kGpioInitProps),
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = std::move(kVolUpRules),
          .properties = std::move(kVolUpProps),
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = std::move(kVolDownRules),
          .properties = std::move(kVolDownProps),
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = std::move(kVolBothRules),
          .properties = std::move(kVolBothProps),
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = std::move(kMicPrivacyRules),
          .properties = std::move(kMicPrivacyProps),
      }}};

  fuchsia_driver_framework::CompositeNodeSpec buttonComposite = {
      {.name = "nelson-buttons", .parents = std::move(parents)}};

  fdf::WireUnownedResult result =
      pbus_.buffer(buttons_arena)
          ->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, dev),
                                 fidl::ToWire(fidl_arena, buttonComposite));
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddCompositeNodeSpec request: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec error: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace nelson
