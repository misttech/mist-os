// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <span>
#include <vector>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>

#include "sherlock.h"
#include "src/devices/lib/fidl-metadata/i2c.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

using i2c_channel_t = fidl_metadata::i2c::Channel;

struct I2cBus {
  uint32_t bus_id;
  zx_paddr_t mmio;
  uint32_t irq;
  cpp20::span<const i2c_channel_t> channels;
};

constexpr i2c_channel_t i2c_ao_channels[]{
    // Tweeter left
    {
        .address = 0x6c,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // Tweeter right
    {
        .address = 0x6d,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // Woofer
    {
        .address = 0x6f,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // Light Sensor
    {
        .address = 0x39,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
};

constexpr i2c_channel_t i2c_2_channels[]{
    // Touch screen I2C
    {
        .address = 0x38,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
};

constexpr i2c_channel_t i2c_3_channels[]{
    // Backlight I2C
    {
        .address = 0x2C,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // IMX227 Camera Sensor
    {
        .address = 0x36,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // LCD Bias
    {
        .address = 0X3E,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
};

constexpr I2cBus buses[]{
    {
        .bus_id = SHERLOCK_I2C_A0_0,
        .mmio = T931_I2C_AOBUS_BASE,
        .irq = T931_I2C_AO_0_IRQ,
        .channels{i2c_ao_channels, std::size(i2c_ao_channels)},
    },
    {
        .bus_id = SHERLOCK_I2C_2,
        .mmio = T931_I2C2_BASE,
        .irq = T931_I2C2_IRQ,
        .channels{i2c_2_channels, std::size(i2c_2_channels)},
    },
    {
        .bus_id = SHERLOCK_I2C_3,
        .mmio = T931_I2C3_BASE,
        .irq = T931_I2C3_IRQ,
        .channels{i2c_3_channels, std::size(i2c_3_channels)},
    },
};

zx_status_t AddI2cBus(const I2cBus& bus,
                      const fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  auto encoded_i2c_metadata = fidl_metadata::i2c::I2CChannelsToFidl(bus.bus_id, bus.channels);
  if (encoded_i2c_metadata.is_error()) {
    zxlogf(ERROR, "Failed to FIDL encode I2C channels: %s", encoded_i2c_metadata.status_string());
    return encoded_i2c_metadata.error_value();
  }

  std::vector<fpbus::Metadata> metadata{
      // TODO(b/385164506): Remove once no longer referenced.
      {{
          .id = std::to_string(DEVICE_METADATA_I2C_CHANNELS),
          .data = encoded_i2c_metadata.value(),
      }},
      {{
          .id = fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata::kSerializableName,
          .data = std::move(encoded_i2c_metadata.value()),
      }},
  };

  const std::vector<fpbus::Mmio> mmios{
      {{
          .base = bus.mmio,
          .length = 0x20,
      }},
  };

  const std::vector<fpbus::Irq> irqs{
      {{
          .irq = bus.irq,
          .mode = fpbus::ZirconInterruptMode::kEdgeHigh,
      }},
  };

  char name[32];
  snprintf(name, sizeof(name), "i2c-%u", bus.bus_id);

  fpbus::Node dev;
  dev.name() = name;
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = PDEV_DID_AMLOGIC_I2C;
  dev.mmio() = mmios;
  dev.irq() = irqs;
  dev.metadata() = std::move(metadata);
  dev.instance_id() = bus.bus_id;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('I2C_');
  const std::vector<fdf::BindRule> kGpioInitRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };
  const std::vector<fdf::NodeProperty> kGpioInitProps = std::vector{
      fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };
  const std::vector<fdf::ParentSpec> kI2cParents = std::vector{
      fdf::ParentSpec{{kGpioInitRules, kGpioInitProps}},
  };

  const fdf::CompositeNodeSpec i2c_spec{{name, kI2cParents}};
  const auto result = pbus.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, dev),
                                                               fidl::ToWire(fidl_arena, i2c_spec));
  if (!result.ok()) {
    zxlogf(ERROR, "Request to add I2C bus %u failed: %s", bus.bus_id,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add I2C bus %u: %s", bus.bus_id,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

zx_status_t Sherlock::I2cInit() {
  // setup pinmux for our I2C busses
  // i2c_ao_0
  gpio_init_steps_.push_back(GpioFunction(T931_GPIOAO(2), 1));
  gpio_init_steps_.push_back(GpioFunction(T931_GPIOAO(3), 1));
  // i2c2
  gpio_init_steps_.push_back(GpioFunction(T931_GPIOZ(14), 3));
  gpio_init_steps_.push_back(GpioFunction(T931_GPIOZ(15), 3));
  // i2c3
  gpio_init_steps_.push_back(GpioFunction(T931_GPIOA(14), 2));
  gpio_init_steps_.push_back(GpioFunction(T931_GPIOA(15), 2));

  for (const auto& bus : buses) {
    AddI2cBus(bus, pbus_);
  }

  return ZX_OK;
}

}  // namespace sherlock
