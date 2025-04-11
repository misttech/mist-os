// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/t931/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <ddk/metadata/buttons.h>
#include <ddktl/device.h>
#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>

#include "lib/fidl_driver/cpp/wire_messaging_declarations.h"
#include "sherlock-gpios.h"
#include "src/devices/board/drivers/sherlock/sherlock.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

// clang-format off
static const buttons_button_config_t buttons[] = {
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_VOLUME_UP, 0, 0, 0},
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_VOLUME_DOWN, 1, 0, 0},
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_FDR, 2, 0, 0},
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_MIC_AND_CAM_MUTE, 3, 0, 0},
};
// clang-format on

// No need for internal pull, external pull-ups used.
static const buttons_gpio_config_t gpios[] = {
    {BUTTONS_GPIO_TYPE_INTERRUPT, BUTTONS_GPIO_FLAG_INVERTED, {}},
    {BUTTONS_GPIO_TYPE_INTERRUPT, BUTTONS_GPIO_FLAG_INVERTED, {}},
    {BUTTONS_GPIO_TYPE_INTERRUPT, BUTTONS_GPIO_FLAG_INVERTED, {}},
    {BUTTONS_GPIO_TYPE_INTERRUPT, 0, {}},
};

zx_status_t Sherlock::ButtonsInit() {
  auto button_pin = [](uint32_t pin, fuchsia_hardware_pin::Pull pull) {
    return fuchsia_hardware_pinimpl::InitStep::WithCall({{
        .pin = pin,
        .call = fuchsia_hardware_pinimpl::InitCall::WithPinConfig({{
            .pull = pull,
            .function = 0,
        }}),
    }});
  };

  gpio_init_steps_.push_back(button_pin(GPIO_VOLUME_UP, fuchsia_hardware_pin::Pull::kUp));
  gpio_init_steps_.push_back(button_pin(GPIO_VOLUME_DOWN, fuchsia_hardware_pin::Pull::kUp));
  gpio_init_steps_.push_back(button_pin(GPIO_VOLUME_BOTH, fuchsia_hardware_pin::Pull::kNone));
  gpio_init_steps_.push_back(button_pin(GPIO_MIC_PRIVACY, fuchsia_hardware_pin::Pull::kNone));

  fidl::Arena<> fidl_arena;
  fdf::Arena buttons_arena('BTTN');

  fpbus::Node dev = {
      {.name = "sherlock-buttons",
       .vid = PDEV_VID_GENERIC,
       .pid = PDEV_PID_GENERIC,
       .did = PDEV_DID_BUTTONS,
       .metadata = std::vector<fpbus::Metadata>{
           {{.id = std::to_string(DEVICE_METADATA_BUTTONS_BUTTONS),
             .data = std::vector<uint8_t>(
                 reinterpret_cast<const uint8_t*>(&buttons),
                 reinterpret_cast<const uint8_t*>(&buttons) + sizeof(buttons))}},
           {{.id = std::to_string(DEVICE_METADATA_BUTTONS_GPIOS),
             .data = std::vector(reinterpret_cast<const uint8_t*>(&gpios),
                                 reinterpret_cast<const uint8_t*>(&gpios) + sizeof(gpios))}}}}};

  const std::vector<fuchsia_driver_framework::BindRule2> kGpioInitRules = {
      fdf::MakeAcceptBindRule2(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };
  const std::vector<fuchsia_driver_framework::NodeProperty2> kGpioInitProps = {
      fdf::MakeProperty2(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };

  const std::vector<fuchsia_driver_framework::BindRule2> kVolUpRules = {
      fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_gpio::SERVICE,
                               bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule2(bind_fuchsia::GPIO_PIN,
                               bind_fuchsia_amlogic_platform_t931::GPIOZ_PIN_ID_PIN_4)};
  const std::vector<fuchsia_driver_framework::NodeProperty2> kVolUpProps = {
      fdf::MakeProperty2(bind_fuchsia_hardware_gpio::SERVICE,
                         bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty2(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_VOLUME_UP),
  };

  const std::vector<fuchsia_driver_framework::BindRule2> kVolDownRules = {
      fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_gpio::SERVICE,
                               bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule2(bind_fuchsia::GPIO_PIN,
                               bind_fuchsia_amlogic_platform_t931::GPIOZ_PIN_ID_PIN_5)};
  const std::vector<fuchsia_driver_framework::NodeProperty2> kVolDownProps = {
      fdf::MakeProperty2(bind_fuchsia_hardware_gpio::SERVICE,
                         bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty2(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_VOLUME_DOWN),
  };

  const std::vector<fuchsia_driver_framework::BindRule2> kVolBothRules = {
      fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_gpio::SERVICE,
                               bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule2(bind_fuchsia::GPIO_PIN,
                               bind_fuchsia_amlogic_platform_t931::GPIOZ_PIN_ID_PIN_13)};
  const std::vector<fuchsia_driver_framework::NodeProperty2> kVolBothProps = {
      fdf::MakeProperty2(bind_fuchsia_hardware_gpio::SERVICE,
                         bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty2(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_VOLUME_BOTH),
  };

  const std::vector<fuchsia_driver_framework::BindRule2> kMicPrivacyRules = {
      fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_gpio::SERVICE,
                               bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule2(bind_fuchsia::GPIO_PIN,
                               bind_fuchsia_amlogic_platform_t931::GPIOH_PIN_ID_PIN_3)};
  const std::vector<fuchsia_driver_framework::NodeProperty2> kMicPrivacyProps = {
      fdf::MakeProperty2(bind_fuchsia_hardware_gpio::SERVICE,
                         bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty2(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_MIC_MUTE),
  };

  std::vector<fuchsia_driver_framework::ParentSpec2> parents = {
      fuchsia_driver_framework::ParentSpec2{{
          .bind_rules = std::move(kGpioInitRules),
          .properties = std::move(kGpioInitProps),
      }},
      fuchsia_driver_framework::ParentSpec2{{
          .bind_rules = std::move(kVolUpRules),
          .properties = std::move(kVolUpProps),
      }},
      fuchsia_driver_framework::ParentSpec2{{
          .bind_rules = std::move(kVolDownRules),
          .properties = std::move(kVolDownProps),
      }},
      fuchsia_driver_framework::ParentSpec2{{
          .bind_rules = std::move(kVolBothRules),
          .properties = std::move(kVolBothProps),
      }},
      fuchsia_driver_framework::ParentSpec2{{
          .bind_rules = std::move(kMicPrivacyRules),
          .properties = std::move(kMicPrivacyProps),
      }}};

  fuchsia_driver_framework::CompositeNodeSpec buttonComposite = {
      {.name = "sherlock-buttons", .parents2 = std::move(parents)}};

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

}  // namespace sherlock
