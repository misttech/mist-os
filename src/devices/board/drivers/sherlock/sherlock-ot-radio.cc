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
#include <lib/ot-radio/ot-radio.h>
#include <limits.h>
#include <unistd.h>

#include <fbl/algorithm.h>
#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>

#include "sherlock-gpios.h"
#include "sherlock.h"
#include "src/devices/board/drivers/sherlock/sherlock-ot-radio-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

static const uint32_t device_id = kOtDeviceNrf52840;
static const device_metadata_t nrf52840_radio_metadata[] = {
    {
        .type = DEVICE_METADATA_PRIVATE,
        .data = &device_id,
        .length = sizeof(device_id),
    },
};

static constexpr zx_device_prop_t props[] = {
    {BIND_PLATFORM_DEV_VID, 0, PDEV_VID_GENERIC},
    {BIND_PLATFORM_DEV_PID, 0, PDEV_PID_SHERLOCK},
    {BIND_PLATFORM_DEV_DID, 0, PDEV_DID_OT_RADIO},
};

static composite_device_desc_t composite_dev = []() {
  composite_device_desc_t desc = {};
  desc.props = props;
  desc.props_count = std::size(props);
  desc.fragments = nrf52840_radio_fragments;
  desc.fragments_count = std::size(nrf52840_radio_fragments);
  desc.primary_fragment = "spi";
  desc.spawn_colocated = true;
  desc.metadata_list = nrf52840_radio_metadata;
  desc.metadata_count = std::size(nrf52840_radio_metadata);
  return desc;
}();

zx_status_t Sherlock::OtRadioInit() {
  if (zx_status_t status = DdkAddComposite("nrf52840-radio", &composite_dev); status != ZX_OK) {
    zxlogf(ERROR, "%s: AddComposite OtRadio(dev) failed: %s", __func__,
           zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

}  // namespace sherlock
