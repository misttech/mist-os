// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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

#include "src/devices/board/drivers/buckeye/buckeye.h"

namespace buckeye {

static constexpr buttons_button_config_t buckeye_buttons[] = {
    {BUTTONS_TYPE_DIRECT, BUTTONS_ID_MIC_MUTE, 0, 0, 0},
};

static constexpr buttons_gpio_config_t buckeye_gpios[] = {
    {BUTTONS_GPIO_TYPE_POLL, 0, {.poll = {GPIO_NO_PULL, zx::msec(20).get()}}},
};

static const device_metadata_t available_buttons_metadata[] = {
    {
        .type = DEVICE_METADATA_BUTTONS_BUTTONS,
        .data = &buckeye_buttons,
        .length = sizeof(buckeye_buttons),
    },
    {
        .type = DEVICE_METADATA_BUTTONS_GPIOS,
        .data = &buckeye_gpios,
        .length = sizeof(buckeye_gpios),
    }};

zx_status_t Buckeye::ButtonsInit() {
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

  zx_status_t status = DdkAddCompositeNodeSpec("buckeye-buttons", buttonComposite);

  if (status != ZX_OK) {
    zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", zx_status_get_string(status));
  }

  return status;
}

}  // namespace buckeye
