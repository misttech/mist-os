// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/hardware/gpioimpl/c/banjo.h>
#include <fuchsia/hardware/gpioimpl/cpp/banjo.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <span>
#include <vector>

#include <soc/as370/as370-i2c.h>

#include "pinecrest.h"
#include "src/devices/lib/fidl-metadata/i2c.h"

namespace board_pinecrest {
namespace fpbus = fuchsia_hardware_platform_bus;
using i2c_channel_t = fidl_metadata::i2c::Channel;

struct I2cBus {
  zx_paddr_t mmio;
  uint32_t irq;
  cpp20::span<const i2c_channel_t> channels;
};

constexpr i2c_channel_t i2c_0_channels[]{
    // For power regulator
    {
        .address = 0x61,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
};

constexpr i2c_channel_t i2c_1_channels[]{
    // For audio out
    {
        .address = 0x4c,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // TI LP5018 LED driver
    {
        .address = 0x29,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // Cypress touch sensor
    {
        .address = 0x37,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
};

constexpr I2cBus buses[]{
    {
        .mmio = as370::kI2c0Base,
        .irq = as370::kI2c0Irq,
        .channels{i2c_0_channels, std::size(i2c_0_channels)},
    },
    {
        .mmio = as370::kI2c1Base,
        .irq = as370::kI2c0Irq,
        .channels{i2c_1_channels, std::size(i2c_1_channels)},
    },
};

zx_status_t AddI2cBus(const uint32_t bus_id, const I2cBus& bus,
                      const fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  const std::vector<fpbus::Mmio> i2c_mmios{
      {{
          .base = bus.mmio,
          .length = as370::kI2c0Size,
      }},
  };

  const std::vector<fpbus::Irq> i2c_irqs{
      {{
          .irq = bus.irq,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
  };

  auto i2c_channels_fidl = fidl_metadata::i2c::I2CChannelsToFidl(bus_id, bus.channels);
  if (i2c_channels_fidl.is_error()) {
    zxlogf(ERROR, "Failed to FIDL encode I2C channel metadata: %d",
           i2c_channels_fidl.error_value());
    return i2c_channels_fidl.error_value();
  }

  std::vector<fpbus::Metadata> i2c_metadata{
      {{
          .type = DEVICE_METADATA_I2C_CHANNELS,
          .data = i2c_channels_fidl.value(),
      }},
  };

  char name[32];
  snprintf(name, sizeof(name), "i2c-%u", bus_id);

  fpbus::Node i2c_dev;
  i2c_dev.name() = name;
  i2c_dev.vid() = PDEV_VID_GENERIC;
  i2c_dev.pid() = PDEV_PID_GENERIC;
  i2c_dev.did() = PDEV_DID_DW_I2C;
  i2c_dev.instance_id() = bus_id;
  i2c_dev.mmio() = i2c_mmios;
  i2c_dev.irq() = i2c_irqs;
  i2c_dev.metadata() = i2c_metadata;

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

zx_status_t Pinecrest::I2cInit() {
  zx_status_t status;

  constexpr uint32_t i2c_gpios[] = {
      as370::kI2c0Sda,
      as370::kI2c0Scl,
      as370::kI2c1Sda,
      as370::kI2c1Scl,
  };

  ddk::GpioImplProtocolClient gpio(parent());

  if (!gpio.is_valid()) {
    zxlogf(ERROR, "%s: Failed to create GPIO protocol client", __func__);
    return ZX_ERR_INTERNAL;
  }

  for (uint32_t i = 0; i < std::size(i2c_gpios); i++) {
    status = gpio.SetAltFunction(i2c_gpios[i], 1);  // 1 == SDA/SCL pinmux setting.
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: GPIO SetAltFunction failed %d", __FUNCTION__, status);
      return status;
    }
  }

  for (uint32_t i = 0; i < std::size(buses); i++) {
    AddI2cBus(i, buses[i], pbus_);
  }

  return ZX_OK;
}

}  // namespace board_pinecrest
