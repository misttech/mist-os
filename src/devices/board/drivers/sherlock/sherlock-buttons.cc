// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <ddk/metadata/buttons.h>
#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>

#include "sherlock-gpios.h"
#include "sherlock.h"
#include "src/devices/board/drivers/sherlock/sherlock-buttons-bind.h"

namespace sherlock {

zx_status_t Sherlock::ButtonsInit() {
  static constexpr buttons_button_config_t buttons[] = {
      {BUTTONS_TYPE_DIRECT, BUTTONS_ID_VOLUME_UP, 0, 0, 0},
      {BUTTONS_TYPE_DIRECT, BUTTONS_ID_VOLUME_DOWN, 1, 0, 0},
      {BUTTONS_TYPE_DIRECT, BUTTONS_ID_FDR, 2, 0, 0},
      {BUTTONS_TYPE_DIRECT, BUTTONS_ID_MIC_AND_CAM_MUTE, 3, 0, 0},
  };

  // No need for internal pull, external pull-ups used.
  static constexpr buttons_gpio_config_t gpios[] = {
      {BUTTONS_GPIO_TYPE_INTERRUPT, BUTTONS_GPIO_FLAG_INVERTED, {.interrupt = {GPIO_PULL_UP}}},
      {BUTTONS_GPIO_TYPE_INTERRUPT, BUTTONS_GPIO_FLAG_INVERTED, {.interrupt = {GPIO_PULL_UP}}},
      {BUTTONS_GPIO_TYPE_INTERRUPT, BUTTONS_GPIO_FLAG_INVERTED, {.interrupt = {GPIO_NO_PULL}}},
      {BUTTONS_GPIO_TYPE_INTERRUPT, 0, {.interrupt = {GPIO_NO_PULL}}},
  };

  const device_metadata_t available_buttons_metadata[] = {
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

  constexpr zx_device_prop_t props[] = {
      {BIND_PLATFORM_DEV_VID, 0, PDEV_VID_GENERIC},
      {BIND_PLATFORM_DEV_PID, 0, PDEV_PID_GENERIC},
      {BIND_PLATFORM_DEV_DID, 0, PDEV_DID_HID_BUTTONS},
  };

  const composite_device_desc_t comp_desc = {
      .props = props,
      .props_count = std::size(props),
      .fragments = sherlock_buttons_fragments,
      .fragments_count = std::size(sherlock_buttons_fragments),
      .primary_fragment = "volume-up",  // ???
      .spawn_colocated = false,
      .metadata_list = available_buttons_metadata,
      .metadata_count = std::size(available_buttons_metadata),
  };

  zx_status_t status = DdkAddComposite("sherlock-buttons", &comp_desc);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: CompositeDeviceAdd failed %d", __func__, status);
    return status;
  }

  return ZX_OK;
}

}  // namespace sherlock
