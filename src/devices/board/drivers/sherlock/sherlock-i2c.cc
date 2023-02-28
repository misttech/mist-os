// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/hardware/gpioimpl/cpp/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <span>
#include <vector>

#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>

#include "sherlock.h"
#include "src/devices/lib/fidl-metadata/i2c.h"

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
  const std::vector<fpbus::Mmio> mmios{
      {{
          .base = bus.mmio,
          .length = 0x20,
      }},
  };

  const std::vector<fpbus::Irq> irqs{
      {{
          .irq = bus.irq,
          .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
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
  dev.instance_id() = bus.bus_id;

  std::vector<fpbus::Metadata> fidl_metadata;

  auto i2c_metadata_fidl = fidl_metadata::i2c::I2CChannelsToFidl(bus.bus_id, bus.channels);
  if (i2c_metadata_fidl.is_error()) {
    zxlogf(ERROR, "Failed to FIDL encode I2C channels: %s", i2c_metadata_fidl.status_string());
    return i2c_metadata_fidl.error_value();
  }

  auto& data = i2c_metadata_fidl.value();

  fidl_metadata.emplace_back([&]() {
    fpbus::Metadata ret;
    ret.type() = DEVICE_METADATA_I2C_CHANNELS;
    ret.data() = std::move(data);
    return ret;
  }());

  dev.metadata() = std::move(fidl_metadata);

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('I2C_');
  auto result = pbus.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, dev));
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
  gpio_impl_.SetAltFunction(T931_GPIOAO(2), 1);
  gpio_impl_.SetAltFunction(T931_GPIOAO(3), 1);
  // i2c2
  gpio_impl_.SetAltFunction(T931_GPIOZ(14), 3);
  gpio_impl_.SetAltFunction(T931_GPIOZ(15), 3);
  // i2c3
  gpio_impl_.SetAltFunction(T931_GPIOA(14), 2);
  gpio_impl_.SetAltFunction(T931_GPIOA(15), 2);

  for (const auto& bus : buses) {
    AddI2cBus(bus, pbus_);
  }

  return ZX_OK;
}

}  // namespace sherlock
