// Copyright 2020 The Fuchsia Authors. All rights reserved.
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

#include <bind/fuchsia/focaltech/platform/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "src/devices/lib/fidl-metadata/i2c.h"
#include "vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;
using i2c_channel_t = fidl_metadata::i2c::Channel;

// Only the AO and EE_M3 i2c busses are used on VIM3

struct I2cBus {
  uint32_t bus_id;
  zx_paddr_t mmio;
  uint32_t irq;
  cpp20::span<const i2c_channel_t> channels;
};

constexpr i2c_channel_t i2c_ao_channels[]{
    // RTC
    {
        .address = 0x51,
        .vid = PDEV_VID_NXP,
        .pid = PDEV_PID_GENERIC,
        .did = PDEV_DID_PCF8563_RTC,
    },
    // STM8s microcontroller
    {
        .address = 0x18,
        .vid = PDEV_VID_KHADAS,
        .pid = PDEV_PID_VIM3,
        .did = PDEV_DID_VIM3_MCU,
    },
    // USB PD
    {
        .address = 0x22,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // TCA6408 (U17) IO expander (used for various lcd/cam signals and LEDs)
    {
        .address = 0x20,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
};
#if 0
    // placeholder until driver implemented and vid/pid/did assigned
    // bus_ids and addresses are correct
    // KXTJ3 (U18) 3 axis accelerometer
    {
        .address = 0x0E,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
};
#endif

constexpr i2c_channel_t i2c_ee_m3_channels[]{
    // SDIO test rig on the J4 GPIO header. May or may not be connected.
    {
        .address = 0x32,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
    // FT5336 touch panel interrupt, used by TS050 touchscreen.
    {
        .address = bind_fuchsia_focaltech_platform::BIND_I2C_ADDRESS_TOUCH,
        .vid = 0,
        .pid = 0,
        .did = 0,
    },
};

constexpr I2cBus kBuses[]{
    {
        .bus_id = bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_A0_0,
        .mmio = A311D_I2C_AOBUS_BASE,
        .irq = A311D_I2C_AO_IRQ,
        .channels{i2c_ao_channels, std::size(i2c_ao_channels)},
    },
    {
        .bus_id = bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_3,
        .mmio = A311D_EE_I2C_M3_BASE,
        .irq = A311D_I2C_M3_IRQ,
        .channels{i2c_ee_m3_channels, std::size(i2c_ee_m3_channels)},
    },
};

zx_status_t AddI2cBus(const I2cBus& bus,
                      const fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  const std::vector<fpbus::Mmio> mmios{
      {{
          .base = bus.mmio,
          .length = A311D_I2C_LENGTH,
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

  fpbus::Node i2c_dev;
  i2c_dev.name() = name;
  i2c_dev.vid() = PDEV_VID_AMLOGIC;
  i2c_dev.pid() = PDEV_PID_GENERIC;
  i2c_dev.did() = PDEV_DID_AMLOGIC_I2C;
  i2c_dev.instance_id() = bus.bus_id;
  i2c_dev.mmio() = mmios;
  i2c_dev.irq() = irqs;

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
  i2c_dev.metadata() = std::move(i2c_metadata);

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

zx_status_t Vim3::I2cInit() {
  // AO
  gpio_impl_.SetAltFunction(A311D_GPIOAO(2), A311D_GPIOAO_2_M0_SCL_FN);
  gpio_impl_.SetAltFunction(A311D_GPIOAO(3), A311D_GPIOAO_3_M0_SDA_FN);

  // EE - M3
  // Used on J13(pins 3,4), M.2 socket(pins 40,42), and J4(pins 22,23)
  gpio_impl_.SetAltFunction(A311D_GPIOA(15), A311D_GPIOA_15_I2C_EE_M3_SCL_FN);
  gpio_impl_.SetAltFunction(A311D_GPIOA(14), A311D_GPIOA_14_I2C_EE_M3_SDA_FN);

  for (const I2cBus& bus : kBuses) {
    AddI2cBus(bus, pbus_);
  }

  return ZX_OK;
}
}  // namespace vim3
