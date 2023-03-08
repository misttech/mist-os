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

#include <soc/aml-a5/a5-hw.h>

#include "buckeye-gpios.h"
#include "buckeye.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"
#include "src/devices/lib/fidl-metadata/i2c.h"

namespace buckeye {
namespace fpbus = fuchsia_hardware_platform_bus;
using i2c_channel_t = fidl_metadata::i2c::Channel;

struct I2cBus {
  zx_paddr_t mmio;
  uint32_t irq;
  cpp20::span<const i2c_channel_t> channels;
};

constexpr i2c_channel_t i2c_a_channels[]{
    // 0 - I2C_A: PMIC
    {
        .address = 0x40,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "pmic",
    },
};

constexpr i2c_channel_t i2c_b_channels[]{
    // 1 - I2C_B: Type-C CC controller
    {
        .address = 0x61,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "usb",
    },
};

constexpr i2c_channel_t i2c_c_channels[]{
    // 2 - I2C_C: Temperature sensor
    {.address = 0x48, .vid = 0, .pid = 0, .did = 0, .name = "temp"},
    // 3 - I2C_C: Woofer codec
    {.address = 0x3C, .vid = 0, .pid = 0, .did = 0, .name = "woofer"},
    // 4 - I2C_C: SHTV3 temperature sensor
    {.address = 0x70, .vid = 0, .pid = 0, .did = 0, .name = "shtv3"},
};

constexpr i2c_channel_t i2c_d_channels[]{
    // 5 - I2C_D: Ambient light sensor
    {
        .address = 0x29,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "als1",
    },
    // 6 - I2C_D: Ambient light sensor
    {
        .address = 0x39,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "als2",
    },
};

constexpr I2cBus buses[]{
    {
        .mmio = A5_I2C_A_BASE,
        .irq = A5_I2C_A_IRQ,
        .channels{i2c_a_channels, std::size(i2c_a_channels)},
    },
    {
        .mmio = A5_I2C_B_BASE,
        .irq = A5_I2C_B_IRQ,
        .channels{i2c_b_channels, std::size(i2c_b_channels)},
    },
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
    zxlogf(ERROR, "I2cInit: Failed to fidl encode i2c channels: %d", i2c_status.error_value());
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
  auto result = pbus.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, i2c_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: NodeAdd I2c(i2c_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: NodeAdd I2c(i2c_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

zx_status_t Buckeye::I2cInit() {
  // I2C_A
  gpio_impl_.SetAltFunction(A5_GPIOZ(15), A5_GPIOZ_15_I2C0_SCL_FN);
  gpio_impl_.SetAltFunction(A5_GPIOZ(14), A5_GPIOZ_14_I2C0_SDA_FN);

  // I2C_B
  gpio_impl_.SetAltFunction(A5_GPIOD(11), A5_GPIOD_11_I2C1_SCL_FN);
  gpio_impl_.SetAltFunction(A5_GPIOD(10), A5_GPIOD_10_I2C1_SDA_FN);

  // I2C_C
  gpio_impl_.SetAltFunction(A5_GPIOC(1), A5_GPIOC_1_I2C2_SCL_FN);
  gpio_impl_.SetAltFunction(A5_GPIOC(0), A5_GPIOC_0_I2C2_SDA_FN);

  // I2C_D
  gpio_impl_.SetAltFunction(A5_GPIOC(8), A5_GPIOC_8_I2C3_SCL_FN);
  gpio_impl_.SetAltFunction(A5_GPIOC(7), A5_GPIOC_7_I2C3_SDA_FN);

  for (uint32_t i = 0; i < std::size(buses); i++) {
    AddI2cBus(i, buses[i], pbus_);
  }

  return ZX_OK;
}
}  // namespace buckeye
