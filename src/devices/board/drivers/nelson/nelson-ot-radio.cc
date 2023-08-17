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

static const std::vector<fpbus::Metadata> kNrf52811RadioMetadata{
    {{
        .type = DEVICE_METADATA_PRIVATE,
        .data =
            std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&device_id),
                                 reinterpret_cast<const uint8_t*>(&device_id) + sizeof(device_id)),
    }},
};

}  // namespace

namespace nelson {

zx_status_t Nelson::OtRadioInit() {
  gpio_impl_.SetAltFunction(S905D3_GPIOC(5), 0);
  gpio_impl_.ConfigIn(S905D3_GPIOC(5), GPIO_NO_PULL);
  gpio_impl_.SetAltFunction(S905D3_GPIOA(13), 0);  // Reset
  gpio_impl_.ConfigOut(S905D3_GPIOA(13), 1);
  gpio_impl_.SetAltFunction(S905D3_GPIOZ(1), 0);  // Boot mode
  gpio_impl_.ConfigOut(S905D3_GPIOZ(1), 1);

  fpbus::Node dev;
  dev.name() = "nrf52811-radio";
  dev.vid() = PDEV_VID_GENERIC;
  dev.pid() = PDEV_PID_NELSON;
  dev.did() = PDEV_DID_OT_RADIO;
  dev.metadata() = kNrf52811RadioMetadata;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('RDIO');
  fdf::WireUnownedResult result = pbus_.buffer(arena)->AddComposite(
      fidl::ToWire(fidl_arena, dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, nrf52811_radio_fragments,
                                               std::size(nrf52811_radio_fragments)),
      "spi");
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddComposite request to platform bus: %s",
           result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add nrf52811-radio composite to platform device: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace nelson
