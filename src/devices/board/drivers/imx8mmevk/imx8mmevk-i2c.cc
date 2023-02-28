// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <span>
#include <vector>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/nxp/platform/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>

#include "imx8mmevk.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"
#include "src/devices/lib/fidl-metadata/i2c.h"
#include "src/devices/lib/nxp/include/soc/imx8mm/i2c.h"

namespace imx8mm_evk {

namespace fpbus = fuchsia_hardware_platform_bus;
using i2c_channel_t = fidl_metadata::i2c::Channel;

struct Imx8mmEvk::I2cBus {
  zx_paddr_t mmio;
  uint32_t irq;
  cpp20::span<const i2c_channel_t> channels;
};

zx_status_t Imx8mmEvk::AddI2cBus(const uint32_t bus_id, const I2cBus& bus) {
  const std::vector<fpbus::Mmio> i2c_mmios{
      {{
          .base = bus.mmio,
          .length = imx8mm::kI2cSize,
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

  const std::vector<fpbus::Metadata> i2c_metadata{
      {{
          .type = DEVICE_METADATA_I2C_CHANNELS,
          .data = i2c_channels_fidl.value(),
      }},
  };

  char name[32];
  snprintf(name, sizeof(name), "i2c-%u", bus_id);

  fpbus::Node i2c_dev;
  i2c_dev.name() = name;
  i2c_dev.vid() = PDEV_VID_NXP;
  i2c_dev.pid() = PDEV_PID_IMX8MMEVK;
  i2c_dev.did() = PDEV_DID_IMX_I2C;
  i2c_dev.instance_id() = bus_id;
  i2c_dev.mmio() = i2c_mmios;
  i2c_dev.irq() = i2c_irqs;
  i2c_dev.metadata() = i2c_metadata;

  const ddk::BindRule kPdevRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                              bind_fuchsia_nxp_platform::BIND_PLATFORM_DEV_VID_NXP),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_PID,
                              bind_fuchsia_nxp_platform::BIND_PLATFORM_DEV_PID_IMX8MMEVK),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                              bind_fuchsia_nxp_platform::BIND_PLATFORM_DEV_DID_I2C),
  };

  const device_bind_prop_t kPdevProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                        bind_fuchsia_nxp_platform::BIND_PLATFORM_DEV_VID_NXP),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                        bind_fuchsia_nxp_platform::BIND_PLATFORM_DEV_DID_I2C),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, bus_id),
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('I2C_');

  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, i2c_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd i2c_dev request failed: %s", result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd i2c_dev failed: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // TODO (fxbug.dev/121200): Add clock fragment, and replace the PlatformBus::NodeAdd() and
  // DdkAddCompositeNodeSpec() calls with PlatformBus::AddCompositeNodeSpec().
  auto status = DdkAddCompositeNodeSpec(name, ddk::CompositeNodeSpec(kPdevRules, kPdevProperties));
  if (status != ZX_OK) {
    zxlogf(INFO, "DdkAddCompositeNodeSpec failed: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t Imx8mmEvk::I2cInit() {
  static constexpr i2c_channel_t i2c_1_channels[]{
      // PMIC
      {
          .address = 0x25,
          .vid = 0,
          .pid = 0,
          .did = 0,
          .name = "PMIC",
      },
  };

  static constexpr I2cBus buses[]{
      {
          .mmio = imx8mm::kI2c1Base,
          .irq = imx8mm::kI2c1Irq,
          .channels{i2c_1_channels, std::size(i2c_1_channels)},
      },
      {
          .mmio = imx8mm::kI2c2Base,
          .irq = imx8mm::kI2c2Irq,
          .channels{},
      },
  };

  for (uint32_t i = 0; i < std::size(buses); i++) {
    AddI2cBus(i, buses[i]);
  }

  return ZX_OK;
}

}  // namespace imx8mm_evk
