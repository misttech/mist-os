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

zx_status_t Imx8mmEvk::I2cInit() {
  static const std::vector<fpbus::Mmio> i2c_mmios{
      {{
          .base = imx8mm::kI2c1Base,
          .length = imx8mm::kI2cSize,
      }},
      {{
          .base = imx8mm::kI2c2Base,
          .length = imx8mm::kI2cSize,
      }},
  };

  static const std::vector<fpbus::Irq> i2c_irqs{
      {{
          .irq = imx8mm::kI2c1Irq,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
      {{
          .irq = imx8mm::kI2c2Irq,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
  };

  constexpr i2c_channel_t i2c_channels[] = {
      // PMIC
      {
          .bus_id = 0,
          .address = 0x25,
          .vid = 0,
          .pid = 0,
          .did = 0,
          .name = "PMIC",
      },
  };

  auto i2c_channels_fidl = fidl_metadata::i2c::I2CChannelsToFidl(i2c_channels);
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

  fpbus::Node i2c_dev;
  i2c_dev.name() = "i2c";
  i2c_dev.vid() = PDEV_VID_NXP;
  i2c_dev.pid() = PDEV_PID_IMX8MMEVK;
  i2c_dev.did() = PDEV_DID_IMX_I2C;
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
  // DdkAddCompositeNodeSpec() calls with PlatformBus::AddNodeGroup().
  auto status = DdkAddCompositeNodeSpec("i2c", ddk::CompositeNodeSpec(kPdevRules, kPdevProperties));
  if (status != ZX_OK) {
    zxlogf(INFO, "DdkAddCompositeNodeSpec failed: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

}  // namespace imx8mm_evk
