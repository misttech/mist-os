// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <span>
#include <vector>

#include <soc/aml-s905d2/s905d2-gpio.h>
#include <soc/aml-s905d2/s905d2-hw.h>

#include "astro-gpios.h"
#include "astro.h"
#include "src/devices/lib/fidl-metadata/i2c.h"

namespace astro {
namespace fpbus = fuchsia_hardware_platform_bus;
using i2c_channel_t = fidl_metadata::i2c::Channel;

struct I2cBus {
  uint32_t bus_id;
  zx_paddr_t mmio;
  uint32_t irq;
  cpp20::span<const i2c_channel_t> channels;
};

constexpr i2c_channel_t i2c_ao_channels[]{
    {
        .address = I2C_AMBIENTLIGHT_ADDR, .vid = 0, .pid = 0, .did = 0,
        // binds as composite device
    },
};

constexpr i2c_channel_t i2c_2_channels[]{
    // Focaltech touch screen
    {
        .address = I2C_FOCALTECH_TOUCH_ADDR, .vid = 0, .pid = 0, .did = 0,
        // binds as composite device
    },
    // Goodix touch screen
    {
        .address = I2C_GOODIX_TOUCH_ADDR, .vid = 0, .pid = 0, .did = 0,
        // binds as composite device
    },
};

constexpr i2c_channel_t i2c_3_channels[]{
    // Backlight I2C
    {
        .address = I2C_BACKLIGHT_ADDR,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // Audio output
    {
        .address = I2C_AUDIO_CODEC_ADDR, .vid = 0, .pid = 0, .did = 0,
        // binds as composite device
    },
};

constexpr I2cBus buses[]{
    {
        .bus_id = ASTRO_I2C_A0_0,
        .mmio = S905D2_I2C_AO_0_BASE,
        .irq = S905D2_I2C_AO_0_IRQ,
        .channels{i2c_ao_channels, std::size(i2c_ao_channels)},
    },
    {
        .bus_id = ASTRO_I2C_2,
        .mmio = S905D2_I2C2_BASE,
        .irq = S905D2_I2C2_IRQ,
        .channels{i2c_2_channels, std::size(i2c_2_channels)},
    },
    {
        .bus_id = ASTRO_I2C_3,
        .mmio = S905D2_I2C3_BASE,
        .irq = S905D2_I2C3_IRQ,
        .channels{i2c_3_channels, std::size(i2c_3_channels)},
    },
};

zx_status_t AddI2cBus(const I2cBus& bus,
                      const fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  auto i2c_metadata_fidl = fidl_metadata::i2c::I2CChannelsToFidl(bus.bus_id, bus.channels);
  if (i2c_metadata_fidl.is_error()) {
    zxlogf(ERROR, "Failed to FIDL encode I2C channels: %s", i2c_metadata_fidl.status_string());
    return i2c_metadata_fidl.error_value();
  }

  auto& data = i2c_metadata_fidl.value();

  std::vector<fpbus::Metadata> i2c_metadata{
      {{
          .type = DEVICE_METADATA_I2C_CHANNELS,
          .data = std::move(data),
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
          .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
      }},

  };

  char name[32];
  snprintf(name, sizeof(name), "i2c-%u", bus.bus_id);

  fpbus::Node i2c_dev = {};
  i2c_dev.name() = name;
  i2c_dev.vid() = PDEV_VID_AMLOGIC;
  i2c_dev.pid() = PDEV_PID_GENERIC;
  i2c_dev.did() = PDEV_DID_AMLOGIC_I2C;
  i2c_dev.mmio() = mmios;
  i2c_dev.irq() = irqs;
  i2c_dev.metadata() = std::move(i2c_metadata);
  i2c_dev.instance_id() = bus.bus_id;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('I2C_');
  auto result = pbus.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, i2c_dev));
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

zx_status_t Astro::I2cInit() {
  // setup pinmux for our I2C busses

  // i2c_ao_0
  gpio_impl_.SetAltFunction(GPIO_SOC_SENSORS_I2C_SDA, 1);
  gpio_impl_.SetDriveStrength(GPIO_SOC_SENSORS_I2C_SDA, 4000, nullptr);

  gpio_impl_.SetAltFunction(GPIO_SOC_SENSORS_I2C_SCL, 1);
  gpio_impl_.SetDriveStrength(GPIO_SOC_SENSORS_I2C_SCL, 4000, nullptr);

  // i2c2
  gpio_impl_.SetAltFunction(GPIO_SOC_TOUCH_I2C_SDA, 3);
  gpio_impl_.SetDriveStrength(GPIO_SOC_TOUCH_I2C_SDA, 4000, nullptr);
  gpio_impl_.SetAltFunction(GPIO_SOC_TOUCH_I2C_SCL, 3);
  gpio_impl_.SetDriveStrength(GPIO_SOC_TOUCH_I2C_SCL, 4000, nullptr);

  // i2c3
  gpio_impl_.SetAltFunction(GPIO_SOC_AV_I2C_SDA, 2);
  gpio_impl_.SetDriveStrength(GPIO_SOC_AV_I2C_SDA, 3000, nullptr);

  gpio_impl_.SetAltFunction(GPIO_SOC_AV_I2C_SCL, 2);
  gpio_impl_.SetDriveStrength(GPIO_SOC_AV_I2C_SCL, 3000, nullptr);

  for (const auto& bus : buses) {
    AddI2cBus(bus, pbus_);
  }

  return ZX_OK;
}

}  // namespace astro
