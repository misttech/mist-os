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
#include <soc/aml-common/aml-i2c.h>
#include <soc/aml-s905d3/s905d3-gpio.h>
#include <soc/aml-s905d3/s905d3-hw.h>

#include "nelson-gpios.h"
#include "nelson.h"
#include "src/devices/lib/fidl-metadata/i2c.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;
using i2c_channel_t = fidl_metadata::i2c::Channel;

struct I2cBus {
  uint32_t bus_id;
  zx_paddr_t mmio;
  uint32_t irq;
  aml_i2c_delay_values delay;
  cpp20::span<const i2c_channel_t> channels;
};

constexpr i2c_channel_t i2c_ao_channels[]{
    // Light sensor
    {
        // binds as composite device
        .address = I2C_AMBIENTLIGHT_ADDR,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "als",
    },
    {
        .address = I2C_SHTV3_ADDR,
        .vid = PDEV_VID_SENSIRION,
        .pid = 0,
        .did = PDEV_DID_SENSIRION_SHTV3,
        .name = "temperature",
    },
};

constexpr i2c_channel_t i2c_2_channels[]{
    // Focaltech touch screen
    {
        // binds as composite device
        .address = I2C_FOCALTECH_TOUCH_ADDR,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "focaltech",
    },
    // Goodix touch screen
    {
        // binds as composite device
        .address = I2C_GOODIX_TOUCH_ADDR,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "goodix",
    },
};

constexpr i2c_channel_t i2c_3_channels[]{
    // Backlight I2C
    {
        .address = I2C_BACKLIGHT_ADDR,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "backlight",
    },
    // Unused
    {
        .address = 0,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "unused",
    },
    // Audio output
    {
        // binds as composite device
        .address = I2C_AUDIO_CODEC_ADDR,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "codec_p2",
    },
    // Power sensors
    {
        .address = I2C_TI_INA231_MLB_ADDR,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "mlb_power",
    },
    {
        .address = I2C_TI_INA231_SPEAKERS_ADDR,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "audio_power",
    },
    {
        .address = I2C_TI_INA231_MLB_ADDR_PROTO,
        .vid = 0,
        .pid = 0,
        .did = 0,
        .name = "mlb_power_proto",
    },
};

constexpr I2cBus buses[]{
    // Delay values are based on a core clock rate of 166 Mhz (fclk_div4 / 3).
    {
        .bus_id = NELSON_I2C_A0_0,
        .mmio = S905D3_I2C_AO_0_BASE,
        .irq = S905D3_I2C_AO_0_IRQ,
        .delay = {819, 417},
        .channels{i2c_ao_channels, std::size(i2c_ao_channels)},
    },
    {
        .bus_id = NELSON_I2C_2,
        .mmio = S905D3_I2C2_BASE,
        .irq = S905D3_I2C2_IRQ,
        .delay = {152, 125},
        .channels{i2c_2_channels, std::size(i2c_2_channels)},
    },
    {
        .bus_id = NELSON_I2C_3,
        .mmio = S905D3_I2C3_BASE,
        .irq = S905D3_I2C3_IRQ,
        .delay = {152, 125},
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

  std::vector<fpbus::Metadata> i2c_metadata{
      // TODO(b/385164506): Remove once no longer referenced.
      {{
          .id = std::to_string(DEVICE_METADATA_I2C_CHANNELS),
          .data = encoded_i2c_metadata.value(),
      }},
      {{
          .id = fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata::kSerializableName,
          .data = std::move(encoded_i2c_metadata.value()),
      }},
      {{
          .id = std::to_string(DEVICE_METADATA_PRIVATE),
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&bus.delay),
              reinterpret_cast<const uint8_t*>(&bus.delay) + sizeof(bus.delay)),
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
  const auto result = pbus.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, i2c_dev),
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

zx_status_t Nelson::I2cInit() {
  // setup pinmux for our I2C busses

  auto i2c_pin = [](uint32_t pin, uint64_t function, uint64_t drive_strength_ua) {
    return fuchsia_hardware_pinimpl::InitStep::WithCall({{
        .pin = pin,
        .call = fuchsia_hardware_pinimpl::InitCall::WithPinConfig({{
            .function = function,
            .drive_strength_ua = drive_strength_ua,
        }}),
    }});
  };

  // i2c_ao_0
  gpio_init_steps_.push_back(i2c_pin(GPIO_SOC_SENSORS_I2C_SCL, 1, 2500));
  gpio_init_steps_.push_back(i2c_pin(GPIO_SOC_SENSORS_I2C_SDA, 1, 2500));
  // i2c2
  gpio_init_steps_.push_back(i2c_pin(GPIO_SOC_TOUCH_I2C_SDA, 3, 3000));
  gpio_init_steps_.push_back(i2c_pin(GPIO_SOC_TOUCH_I2C_SCL, 3, 3000));
  // i2c3
  gpio_init_steps_.push_back(i2c_pin(GPIO_SOC_AV_I2C_SDA, 2, 3000));
  gpio_init_steps_.push_back(i2c_pin(GPIO_SOC_AV_I2C_SCL, 2, 3000));

  for (const auto& bus : buses) {
    AddI2cBus(bus, pbus_);
  }

  return ZX_OK;
}

}  // namespace nelson
