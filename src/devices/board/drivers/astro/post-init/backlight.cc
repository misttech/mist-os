// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/compiler.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/i2c/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/ti/platform/cpp/bind.h>
#include <soc/aml-s905d2/s905d2-hw.h>

#include "src/devices/board/drivers/astro/post-init/post-init.h"
#include "src/ui/backlight/drivers/ti-lp8556/ti-lp8556Metadata.h"

namespace astro {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> backlight_mmios{
    {{
        .base = S905D2_GPIO_AO_BASE,
        .length = S905D2_GPIO_AO_LENGTH,
    }},
};

constexpr double kMaxBrightnessInNits = 400.0;

TiLp8556Metadata kDeviceMetadata = {
    .panel_id = 0,
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

zx::result<> PostInit::InitBacklight() {
  const std::vector<fpbus::Metadata> backlight_metadata{
      {{
          .id = std::to_string(DEVICE_METADATA_BACKLIGHT_MAX_BRIGHTNESS_NITS),
          .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&kMaxBrightnessInNits),
                                       reinterpret_cast<const uint8_t*>(&kMaxBrightnessInNits) +
                                           sizeof(kMaxBrightnessInNits)),
      }},
      {{
          .id = std::to_string(DEVICE_METADATA_PRIVATE),
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&kDeviceMetadata),
              reinterpret_cast<const uint8_t*>(&kDeviceMetadata) + sizeof(kDeviceMetadata)),
      }},
      {{
          .id = std::to_string(DEVICE_METADATA_DISPLAY_PANEL_TYPE),
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&panel_type_),
              reinterpret_cast<const uint8_t*>(&panel_type_) + sizeof(panel_type_)),
      }},
  };

  fpbus::Node backlight_dev = {};
  backlight_dev.name() = "backlight";
  backlight_dev.vid() = bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI;
  backlight_dev.pid() = bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_PID_LP8556;
  backlight_dev.did() = bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_BACKLIGHT;
  backlight_dev.metadata() = backlight_metadata;
  backlight_dev.mmio() = backlight_mmios;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('BACK');

  const auto bind_rules = std::vector{
      fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_i2c::SERVICE,
                               bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule2(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_3),
      fdf::MakeAcceptBindRule2(bind_fuchsia::I2C_ADDRESS,
                               bind_fuchsia_i2c::BIND_I2C_ADDRESS_BACKLIGHT),
  };

  const auto properties = std::vector{
      fdf::MakeProperty2(bind_fuchsia_hardware_i2c::SERVICE,
                         bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
  };

  const auto parents = std::vector{
      fuchsia_driver_framework::ParentSpec2{{
          .bind_rules = bind_rules,
          .properties = properties,
      }},
  };

  const auto composite_node_spec =
      fuchsia_driver_framework::CompositeNodeSpec{{.name = "backlight", .parents2 = parents}};

  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, backlight_dev), fidl::ToWire(fidl_arena, composite_node_spec));

  if (!result.ok()) {
    FDF_LOG(ERROR, "AddCompositeNodeSpec Backlight(backlight_dev) request failed: %s",
            result.FormatDescription().data());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "AddCompositeNodeSpec Backlight(backlight_dev) failed: %s",
            zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  return zx::ok();
}

}  // namespace astro
