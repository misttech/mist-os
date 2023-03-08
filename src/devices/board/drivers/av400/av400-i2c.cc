// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <limits.h>

#include <span>
#include <vector>

#include <soc/aml-a5/a5-gpio.h>
#include <soc/aml-a5/a5-hw.h>

#include "av400.h"
#include "src/devices/board/drivers/av400/av400-i2c-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"
#include "src/devices/lib/fidl-metadata/i2c.h"

namespace av400 {
namespace fpbus = fuchsia_hardware_platform_bus;
using i2c_channel_t = fidl_metadata::i2c::Channel;

// Only the I2C_C and I2C_D busses are used on AV400

struct I2cBus {
  zx_paddr_t mmio;
  uint32_t irq;
  cpp20::span<const i2c_channel_t> channels;
};

constexpr i2c_channel_t i2c_c_channels[]{
    // ESMT audio amplifier
    {
        .address = 0x30,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // ESMT audio amplifier
    {
        .address = 0x31,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // ESMT audio amplifier
    {
        .address = 0x34,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // ESMT audio amplifier
    {
        .address = 0x35,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
};

constexpr i2c_channel_t i2c_d_channels[]{
    // ti, tas5707 amplifier
    {
        .address = 0x1b,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // ESMT audio amplifier
    {
        .address = 0x30,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // ESMT audio amplifier
    {
        .address = 0x31,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // ESMT audio amplifier
    {
        .address = 0x34,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // ESMT audio amplifier
    {
        .address = 0x35,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
};

constexpr I2cBus buses[]{
    {
        .mmio = A5_I2C_C_BASE,
        .irq = A5_I2C_C_IRQ,
        .channels{i2c_c_channels, std::size(i2c_c_channels)},
    },
    {
        .mmio = A5_I2C_D_BASE,
        .irq = A5_I2C_D_IRQ,
        .channels{i2c_d_channels, std::size(i2c_d_channels)},
    },
};

zx_status_t AddI2cBus(const uint32_t bus_id, const I2cBus& bus,
                      const fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  const std::vector<fpbus::Mmio> i2c_mmios{
      {{
          .base = bus.mmio,
          .length = A5_I2C_LENGTH,
      }},
  };

  const std::vector<fpbus::Irq> i2c_irqs{
      {{
          .irq = bus.irq,
          .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
      }},
  };

  char name[32];
  snprintf(name, sizeof(name), "i2c-%u", bus_id);

  fpbus::Node i2c_dev;
  i2c_dev.name() = name;
  i2c_dev.vid() = PDEV_VID_AMLOGIC;
  i2c_dev.pid() = PDEV_PID_GENERIC;
  i2c_dev.did() = PDEV_DID_AMLOGIC_I2C;
  i2c_dev.instance_id() = bus_id;
  i2c_dev.mmio() = i2c_mmios;
  i2c_dev.irq() = i2c_irqs;

  auto i2c_status = fidl_metadata::i2c::I2CChannelsToFidl(bus_id, bus.channels);
  if (i2c_status.is_error()) {
    zxlogf(ERROR, "I2cInit: Failed to fidl encode i2c channels: %s", i2c_status.status_string());
    return i2c_status.error_value();
  }

  auto& data = i2c_status.value();

  std::vector<fpbus::Metadata> i2c_metadata{
      {{
          .type = DEVICE_METADATA_I2C_CHANNELS,
          .data = std::move(data),
      }},
  };
  i2c_dev.metadata() = std::move(i2c_metadata);

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('I2C_');
  auto result = pbus.buffer(arena)->AddComposite(
      fidl::ToWire(fidl_arena, i2c_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, av400_i2c_fragments,
                                               std::size(av400_i2c_fragments)),
      "pdev");
  if (!result.ok()) {
    zxlogf(ERROR, "AddComposite I2c(i2c_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddComposite I2c(i2c_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

zx_status_t Av400::I2cInit() {
  auto i2c_gpio = [&arena = gpio_init_arena_](
                      uint64_t alt_function) -> fuchsia_hardware_gpio_init::wire::GpioInitOptions {
    return fuchsia_hardware_gpio_init::wire::GpioInitOptions::Builder(arena)
        .alt_function(alt_function)
        .Build();
  };

  gpio_init_steps_.push_back({A5_GPIOD(15), i2c_gpio(A5_GPIOD_15_I2C2_SCL_FN)});
  gpio_init_steps_.push_back({A5_GPIOD(14), i2c_gpio(A5_GPIOD_14_I2C2_SDA_FN)});
  gpio_init_steps_.push_back({A5_GPIOD(13), i2c_gpio(A5_GPIOD_13_I2C3_SCL_FN)});
  gpio_init_steps_.push_back({A5_GPIOD(12), i2c_gpio(A5_GPIOD_12_I2C3_SDA_FN)});

  for (uint32_t i = 0; i < std::size(buses); i++) {
    AddI2cBus(i, buses[i], pbus_);
  }

  return ZX_OK;
}
}  // namespace av400
