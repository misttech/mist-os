// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <bind/fuchsia/amlogic/platform/t931/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <ddk/metadata/buttons.h>
#include <ddktl/device.h>
#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>

#include "src/devices/board/drivers/sherlock/sherlock.h"

namespace sherlock {

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
    {BUTTONS_GPIO_TYPE_INTERRUPT, BUTTONS_GPIO_FLAG_INVERTED, {.interrupt = {GPIO_PULL_UP}}},
    {BUTTONS_GPIO_TYPE_INTERRUPT, BUTTONS_GPIO_FLAG_INVERTED, {.interrupt = {GPIO_PULL_UP}}},
    {BUTTONS_GPIO_TYPE_INTERRUPT, BUTTONS_GPIO_FLAG_INVERTED, {.interrupt = {GPIO_NO_PULL}}},
    {BUTTONS_GPIO_TYPE_INTERRUPT, 0, {.interrupt = {GPIO_NO_PULL}}},
};

static const device_metadata_t available_buttons_metadata[] = {
    {
        .type = DEVICE_METADATA_BUTTONS_BUTTONS,
        .data = &buttons,
        .length = sizeof(buttons),
    },
    {
        .type = DEVICE_METADATA_BUTTONS_GPIOS,
        .data = &gpios,
        .length = sizeof(gpios),
    },
};

zx_status_t Sherlock::ButtonsInit() {
  const ddk::BindRule kVolUpRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_t931::GPIOZ_PIN_ID_PIN_4)};
  const device_bind_prop_t kVolUpProps[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_VOLUME_UP),
  };

  const ddk::BindRule kVolDownRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_t931::GPIOZ_PIN_ID_PIN_5)};
  const device_bind_prop_t kVolDownProps[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_VOLUME_DOWN),
  };

  const ddk::BindRule kVolBothRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_t931::GPIOZ_PIN_ID_PIN_13)};
  const device_bind_prop_t kVolBothProps[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_VOLUME_BOTH),
  };

  const ddk::BindRule kMicPrivacyRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_t931::GPIOH_PIN_ID_PIN_3)};
  const device_bind_prop_t kMicPrivacyProps[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_MIC_MUTE),
  };

  const ddk::CompositeNodeSpec buttonComposite =
      ddk::CompositeNodeSpec(kVolUpRules, kVolUpProps)
          .AddParentSpec(kVolDownRules, kVolDownProps)
          .AddParentSpec(kVolBothRules, kVolBothProps)
          .AddParentSpec(kMicPrivacyRules, kMicPrivacyProps)
          .set_metadata(available_buttons_metadata);

  zx_status_t status = DdkAddCompositeNodeSpec("sherlock-buttons", buttonComposite);

  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec failed: %s", __func__, zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

}  // namespace sherlock
