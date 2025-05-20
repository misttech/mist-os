// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/ot-radio/ot-radio.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/google/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/spi/cpp/bind.h>
#include <bind/fuchsia/nordic/platform/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>

#include "nelson-gpios.h"
#include "nelson.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace {
namespace fpbus = fuchsia_hardware_platform_bus;

constexpr uint32_t device_id = kOtDeviceNrf52811;

static const std::vector<fpbus::Metadata> kNrf52811RadioMetadata{
    {{
        .id = std::to_string(DEVICE_METADATA_PRIVATE),
        .data =
            std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&device_id),
                                 reinterpret_cast<const uint8_t*>(&device_id) + sizeof(device_id)),
    }},
};

const std::vector<fdf::BindRule2> kSpiRules = std::vector{
    fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_spi::SERVICE,
                             bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_VID,
                             bind_fuchsia_nordic_platform::BIND_PLATFORM_DEV_VID_NORDIC),
    fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_PID,
                             bind_fuchsia_nordic_platform::BIND_PLATFORM_DEV_PID_NRF52811),
    fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_DID,
                             bind_fuchsia_nordic_platform::BIND_PLATFORM_DEV_DID_THREAD),

};

const std::vector<fdf::NodeProperty2> kSpiProperties = std::vector{
    fdf::MakeProperty2(bind_fuchsia_hardware_spi::SERVICE,
                       bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_VID,
                       bind_fuchsia_nordic_platform::BIND_PLATFORM_DEV_VID_NORDIC),
    fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_DID,
                       bind_fuchsia_nordic_platform::BIND_PLATFORM_DEV_DID_THREAD),
};

const std::vector<fdf::BindRule2> kGpioInitRules = std::vector{
    fdf::MakeAcceptBindRule2(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};
const std::vector<fdf::NodeProperty2> kGpioInitProperties = std::vector{
    fdf::MakeProperty2(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

const std::map<uint32_t, std::string> kGpioPinFunctionMap = {
    {GPIO_TH_SOC_INT, bind_fuchsia_gpio::FUNCTION_OT_RADIO_INTERRUPT},
    {GPIO_SOC_TH_RST_L, bind_fuchsia_gpio::FUNCTION_OT_RADIO_RESET},
    {GPIO_SOC_TH_BOOT_MODE_L, bind_fuchsia_gpio::FUNCTION_OT_RADIO_BOOTLOADER},
};

}  // namespace

namespace nelson {

zx_status_t Nelson::OtRadioInit() {
  gpio_init_steps_.push_back(fuchsia_hardware_pinimpl::InitStep::WithCall({{
      .pin = GPIO_TH_SOC_INT,
      .call = fuchsia_hardware_pinimpl::InitCall::WithPinConfig({{
          .pull = fuchsia_hardware_pin::Pull::kNone,
          .function = 0,
      }}),
  }}));
  gpio_init_steps_.push_back(GpioInput(GPIO_TH_SOC_INT));
  gpio_init_steps_.push_back(GpioFunction(GPIO_SOC_TH_RST_L, 0));  // Reset
  gpio_init_steps_.push_back(GpioOutput(GPIO_SOC_TH_RST_L, true));
  gpio_init_steps_.push_back(GpioFunction(GPIO_SOC_TH_BOOT_MODE_L, 0));  // Boot mode
  gpio_init_steps_.push_back(GpioOutput(GPIO_SOC_TH_BOOT_MODE_L, true));

  fpbus::Node dev;
  dev.name() = "nrf52811-radio";
  dev.vid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC;
  dev.pid() = bind_fuchsia_google_platform::BIND_PLATFORM_DEV_PID_NELSON;
  dev.did() = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_OT_RADIO;
  dev.metadata() = kNrf52811RadioMetadata;

  std::vector<fdf::ParentSpec2> parents = {
      fdf::ParentSpec2{{kSpiRules, kSpiProperties}},
      fdf::ParentSpec2{{kGpioInitRules, kGpioInitProperties}},
  };
  parents.reserve(parents.size() + kGpioPinFunctionMap.size());

  for (auto& [gpio_pin, function] : kGpioPinFunctionMap) {
    auto rules = std::vector{
        fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_gpio::SERVICE,
                                 bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
        fdf::MakeAcceptBindRule2(bind_fuchsia::GPIO_PIN, gpio_pin),
    };
    auto properties = std::vector{
        fdf::MakeProperty2(bind_fuchsia_hardware_gpio::SERVICE,
                           bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
        fdf::MakeProperty2(bind_fuchsia_gpio::FUNCTION, function),
    };
    parents.push_back(fdf::ParentSpec2{{rules, properties}});
  }

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('RDIO');
  fdf::WireUnownedResult result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "nrf52811_radio", .parents2 = parents}}));

  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddCompositeNodeSpec request to platform bus: %s",
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
