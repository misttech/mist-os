// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <ddk/metadata/buttons.h>
#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>

#include "buttons-device.h"

namespace buttons {

void ButtonsDevice::DdkUnbind(ddk::UnbindTxn txn) {
  ShutDown();
  txn.Reply();
}

void ButtonsDevice::DdkRelease() { delete this; }

static zx_status_t buttons_bind(void* ctx, zx_device_t* parent) {
  fbl::AllocChecker ac;
  auto dev = fbl::make_unique_checked<buttons::ButtonsDevice>(
      &ac, parent, fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher()));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  // Get buttons metadata.
  auto temp_buttons =
      ddk::GetMetadataArray<buttons_button_config_t>(parent, DEVICE_METADATA_BUTTONS_BUTTONS);
  if (!temp_buttons.is_ok()) {
    return temp_buttons.error_value();
  }
  auto buttons = fbl::MakeArray<buttons_button_config_t>(&ac, temp_buttons->size());
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  std::copy(temp_buttons->begin(), temp_buttons->end(), buttons.begin());

  // Get gpios metadata.
  auto configs =
      ddk::GetMetadataArray<buttons_gpio_config_t>(parent, DEVICE_METADATA_BUTTONS_GPIOS);
  if (!configs.is_ok()) {
    return configs.error_value();
  }
  size_t n_gpios = configs->size();

  // Prepare gpios array.
  auto gpios = fbl::Array(new (&ac) ButtonsDevice::Gpio[n_gpios], n_gpios);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  for (uint32_t i = 0; i < n_gpios; ++i) {
    const char* name;
    switch (buttons[i].id) {
      case BUTTONS_ID_VOLUME_UP:
        name = "volume-up";
        break;
      case BUTTONS_ID_VOLUME_DOWN:
        name = "volume-down";
        break;
      case BUTTONS_ID_FDR:
        name = "volume-both";
        break;
      case BUTTONS_ID_MIC_MUTE:
      case BUTTONS_ID_MIC_AND_CAM_MUTE:
        name = "mic-privacy";
        break;
      case BUTTONS_ID_CAM_MUTE:
        name = "cam-mute";
        break;
      case BUTTONS_ID_POWER:
        name = "power";
        break;
      default:
        return ZX_ERR_NOT_SUPPORTED;
    };
    zx::result gpio_client =
        ddk::Device<void>::DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(
            parent, name);
    if (gpio_client.is_error()) {
      zxlogf(ERROR, "Failed to get gpio protocol from fragment %s: %s", name,
             gpio_client.status_string());
      return ZX_ERR_INTERNAL;
    }
    gpios[i].client.Bind(std::move(gpio_client.value()));
    gpios[i].config = configs.value()[i];
  }

  zx_status_t status = dev->Bind(std::move(gpios), std::move(buttons));
  if (status == ZX_OK) {
    // devmgr is now in charge of the memory for dev.
    [[maybe_unused]] auto ptr = dev.release();
  }
  return status;
}

static constexpr zx_driver_ops_t buttons_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = buttons_bind;
  return ops;
}();

}  // namespace buttons

ZIRCON_DRIVER(buttons, buttons::buttons_driver_ops, "zircon", "0.1");
