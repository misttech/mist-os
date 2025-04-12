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
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/ot-radio/ot-radio.h>
#include <limits.h>
#include <unistd.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/google/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/spi/cpp/bind.h>
#include <bind/fuchsia/nordic/platform/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <fbl/algorithm.h>
#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>

#include "sherlock-gpios.h"
#include "sherlock.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

constexpr uint32_t device_id = kOtDeviceNrf52840;

static const std::vector<fpbus::Metadata> kNrf52840RadioMetadata{
    {{
        .id = std::to_string(DEVICE_METADATA_PRIVATE),
        .data =
            std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&device_id),
                                 reinterpret_cast<const uint8_t*>(&device_id) + sizeof(device_id)),
    }},
};

const std::vector<fdf::BindRule> kSpiRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spi::SERVICE,
                            bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                            bind_fuchsia_nordic_platform::BIND_PLATFORM_DEV_VID_NORDIC),
    fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_PID,
                            bind_fuchsia_nordic_platform::BIND_PLATFORM_DEV_PID_NRF52840),
    fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                            bind_fuchsia_nordic_platform::BIND_PLATFORM_DEV_DID_THREAD),

};

const std::vector<fdf::NodeProperty> kSpiProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia_hardware_spi::SERVICE,
                      bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                      bind_fuchsia_nordic_platform::BIND_PLATFORM_DEV_VID_NORDIC),
    fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                      bind_fuchsia_nordic_platform::BIND_PLATFORM_DEV_DID_THREAD),
};

const std::vector<fdf::BindRule> kGpioInitRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};
const std::vector<fdf::NodeProperty> kGpioInitProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

const std::map<uint32_t, std::string> kGpioPinFunctionMap = {
    {GPIO_OT_RADIO_INTERRUPT, bind_fuchsia_gpio::FUNCTION_OT_RADIO_INTERRUPT},
    {GPIO_OT_RADIO_RESET, bind_fuchsia_gpio::FUNCTION_OT_RADIO_RESET},
    {GPIO_OT_RADIO_BOOTLOADER, bind_fuchsia_gpio::FUNCTION_OT_RADIO_BOOTLOADER},
};

zx_status_t Sherlock::OtRadioInit() {
  gpio_init_steps_.push_back(GpioPull(GPIO_OT_RADIO_INTERRUPT, fuchsia_hardware_pin::Pull::kNone));

  fpbus::Node dev;
  dev.name() = "nrf52840-radio";
  dev.vid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC;
  dev.pid() = bind_fuchsia_google_platform::BIND_PLATFORM_DEV_PID_SHERLOCK;
  dev.did() = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_OT_RADIO;
  dev.metadata() = kNrf52840RadioMetadata;

  std::vector<fdf::ParentSpec> parents = {
      fdf::ParentSpec{{kSpiRules, kSpiProperties}},
      fdf::ParentSpec{{kGpioInitRules, kGpioInitProperties}},
  };
  parents.reserve(parents.size() + kGpioPinFunctionMap.size());

  for (auto& [gpio_pin, function] : kGpioPinFunctionMap) {
    auto rules = std::vector{
        fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                                bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
        fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, gpio_pin),
    };
    auto properties = std::vector{
        fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                          bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
        fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, function),
    };
    parents.push_back(fdf::ParentSpec{{rules, properties}});
  }

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('RDIO');
  fdf::WireUnownedResult result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "nrf52840_radio", .parents = parents}}));

  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddCompositeNodeSpec request to platform bus: %s",
           result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add nrf52840-radio composite to platform device: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace sherlock
