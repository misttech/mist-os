// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <zircon/compiler.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/i2c/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <soc/aml-s905d3/s905d3-hw.h>

#include "nelson.h"
#include "src/ui/backlight/drivers/ti-lp8556/ti-lp8556Metadata.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> backlight_mmios{
    {{
        .base = S905D3_GPIO_AO_BASE,
        .length = S905D3_GPIO_AO_LENGTH,
    }},
};

static const std::vector<fpbus::BootMetadata> backlight_boot_metadata{
    {{
        .zbi_type = DEVICE_METADATA_BOARD_PRIVATE,
        .zbi_extra = 0,
    }},
};

constexpr double kMaxBrightnessInNits = 250.0;

zx_status_t Nelson::BacklightInit() {
  TiLp8556Metadata device_metadata = {
      .allow_set_current_scale = false,
      .registers =
          {
              // Registers
              0x01, 0x85,  // Device Control
                           // EPROM
              0xa2, 0x30,  // CFG2
              0xa3, 0x32,  // CFG3
              0xa5, 0x54,  // CFG5
              0xa7, 0xf4,  // CFG7
              0xa9, 0x60,  // CFG9
              0xae, 0x09,  // CFGE
          },
      .register_count = 14,
  };

  std::vector<fpbus::Metadata> backlight_metadata{
      {{
          .id = std::to_string(DEVICE_METADATA_BACKLIGHT_MAX_BRIGHTNESS_NITS),
          .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&kMaxBrightnessInNits),
                                       reinterpret_cast<const uint8_t*>(&kMaxBrightnessInNits) +
                                           sizeof(kMaxBrightnessInNits)),
      }},
      {{
          .id = std::to_string(DEVICE_METADATA_PRIVATE),
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&device_metadata),
              reinterpret_cast<const uint8_t*>(&device_metadata) + sizeof(device_metadata)),
      }},
  };

  fpbus::Node backlight_dev;
  backlight_dev.name() = "backlight";
  backlight_dev.vid() = PDEV_VID_TI;
  backlight_dev.pid() = PDEV_PID_TI_LP8556;
  backlight_dev.did() = PDEV_DID_TI_BACKLIGHT;
  backlight_dev.mmio() = backlight_mmios;
  backlight_dev.metadata() = backlight_metadata;
  backlight_dev.boot_metadata() = backlight_boot_metadata;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('BACK');

  auto bind_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                              bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_3),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                              bind_fuchsia_i2c::BIND_I2C_ADDRESS_BACKLIGHT),
  };

  auto properties = std::vector{
      fdf::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                        bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
  };

  auto parents = std::vector{
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = bind_rules,
          .properties = properties,
      }},
  };

  auto composite_node_spec =
      fuchsia_driver_framework::CompositeNodeSpec{{.name = "backlight", .parents = parents}};

  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, backlight_dev), fidl::ToWire(fidl_arena, composite_node_spec));

  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Backlight(backlight_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Backlight(backlight_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

}  // namespace nelson
