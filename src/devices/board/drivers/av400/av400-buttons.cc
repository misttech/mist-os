// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <bind/fuchsia/amlogic/platform/a5/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <ddk/metadata/buttons.h>
#include <ddktl/device.h>
#include <soc/aml-a5/a5-gpio.h>
#include <soc/aml-a5/a5-hw.h>

#include "src/devices/board/drivers/av400/av400.h"

namespace av400 {
namespace fhgpio = fuchsia_hardware_gpio;

static constexpr buttons_button_config_t av400_buttons[] = {
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_MIC_MUTE, 0, 0, 0},
};

static constexpr buttons_gpio_config_t av400_gpios[] = {
    {BUTTONS_GPIO_TYPE_POLL,
     0,
     {.poll = {static_cast<uint32_t>(fhgpio::GpioFlags::kNoPull), zx::msec(20).get()}}},
};

static const device_metadata_t available_buttons_metadata[] = {
    {
        .type = DEVICE_METADATA_BUTTONS_BUTTONS,
        .data = &av400_buttons,
        .length = sizeof(av400_buttons),
    },
    {
        .type = DEVICE_METADATA_BUTTONS_GPIOS,
        .data = &av400_gpios,
        .length = sizeof(av400_gpios),
    }};

zx_status_t Av400::ButtonsInit() {
  const ddk::BindRule kMicPrivacyRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_a5::GPIOD_PIN_ID_PIN_3)};
  const device_bind_prop_t kMicPrivacyProps[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_MIC_MUTE),
  };

  const ddk::CompositeNodeSpec buttonComposite =
      ddk::CompositeNodeSpec(kMicPrivacyRules, kMicPrivacyProps)
          .set_metadata(available_buttons_metadata);

  return DdkAddCompositeNodeSpec("av400-buttons", buttonComposite);
}

}  // namespace av400
