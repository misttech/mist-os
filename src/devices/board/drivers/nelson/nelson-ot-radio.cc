// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/ot-radio/ot-radio.h>

#include "nelson-gpios.h"
#include "nelson.h"
#include "src/devices/board/drivers/nelson/nelson_ot_radio_bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace {
namespace fpbus = fuchsia_hardware_platform_bus;

constexpr uint32_t device_id = kOtDeviceNrf52811;
static const device_metadata_t nrf52811_radio_metadata[] = {
    {
        .type = DEVICE_METADATA_PRIVATE,
        .data = &device_id,
        .length = sizeof(device_id),
    },
};

static constexpr zx_device_prop_t props[] = {
    {BIND_PLATFORM_DEV_VID, 0, PDEV_VID_GENERIC},
    {BIND_PLATFORM_DEV_PID, 0, PDEV_PID_NELSON},
    {BIND_PLATFORM_DEV_DID, 0, PDEV_DID_OT_RADIO},
};

static composite_device_desc_t composite_dev = []() {
  composite_device_desc_t desc = {};
  desc.props = props;
  desc.props_count = std::size(props);
  desc.fragments = nrf52811_radio_fragments;
  desc.fragments_count = std::size(nrf52811_radio_fragments);
  desc.primary_fragment = "spi";
  desc.spawn_colocated = true;
  desc.metadata_list = nrf52811_radio_metadata;
  desc.metadata_count = std::size(nrf52811_radio_metadata);
  return desc;
}();

}  // namespace

namespace nelson {

zx_status_t Nelson::OtRadioInit() {
  gpio_impl_.SetAltFunction(S905D3_GPIOC(5), 0);
  gpio_impl_.ConfigIn(S905D3_GPIOC(5), GPIO_NO_PULL);
  gpio_impl_.SetAltFunction(S905D3_GPIOA(13), 0);  // Reset
  gpio_impl_.ConfigOut(S905D3_GPIOA(13), 1);
  gpio_impl_.SetAltFunction(S905D3_GPIOZ(1), 0);  // Boot mode
  gpio_impl_.ConfigOut(S905D3_GPIOZ(1), 1);

  if (zx_status_t status = DdkAddComposite("nrf52811-radio", &composite_dev); status != ZX_OK) {
    zxlogf(ERROR, "%s: DdkAddComposite OtRadio(dev) failed: %s", __func__,
           zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

}  // namespace nelson
