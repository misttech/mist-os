// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/device-protocol/display-panel.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/amlogic/platform/s905d2/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/amlogiccanvas/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <soc/aml-s905d2/s905d2-gpio.h>
#include <soc/aml-s905d2/s905d2-hw.h>

#include "src/devices/board/drivers/astro/astro-btis.h"
#include "src/devices/board/drivers/astro/post-init/post-init.h"

namespace astro {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> display_mmios{
    {{
        // VPU
        .base = S905D2_VPU_BASE,
        .length = S905D2_VPU_LENGTH,
        .name = "vpu",
    }},
    {{
        // MIPI DSI "TOP"
        .base = S905D2_MIPI_TOP_DSI_BASE,
        .length = S905D2_MIPI_TOP_DSI_LENGTH,
        .name = "dsi-top",
    }},
    {{
        // MIPI DSI PHY
        .base = S905D2_DSI_PHY_BASE,
        .length = S905D2_DSI_PHY_LENGTH,
        .name = "dsi-phy",
    }},
    {{
        // DSI Host Controller
        .base = S905D2_MIPI_DSI_BASE,
        .length = S905D2_MIPI_DSI_LENGTH,
        .name = "dsi-controller",
    }},
    {{
        // HIU / HHI
        .base = S905D2_HIU_BASE,
        .length = S905D2_HIU_LENGTH,
        .name = "hhi",
    }},
    {{
        // AOBUS
        // TODO(https://fxbug.dev/42081392): Restrict range to RTI
        .base = S905D2_AOBUS_BASE,
        .length = S905D2_AOBUS_LENGTH,
        .name = "always-on-rti",
    }},
    {{
        // RESET
        .base = S905D2_RESET_BASE,
        .length = S905D2_RESET_LENGTH,
        .name = "ee-reset",
    }},
    {{
        // PERIPHS_REGS (GPIO Multiplexer)
        .base = S905D2_GPIO_BASE,
        .length = S905D2_GPIO_LENGTH,
        .name = "gpio-mux",
    }},
};

static const std::vector<fpbus::Irq> display_irqs{
    {{
        .irq = S905D2_VIU1_VSYNC_IRQ,
        .mode = fpbus::ZirconInterruptMode::kEdgeHigh,
        .name = "viu1-vsync",
    }},
    {{
        .irq = S905D2_RDMA_DONE,
        .mode = fpbus::ZirconInterruptMode::kEdgeHigh,
        .name = "rdma-done",
    }},
    {{
        .irq = S905D2_VID1_WR,
        .mode = fpbus::ZirconInterruptMode::kEdgeHigh,
        .name = "vdin1-write-done",
    }},
};

static std::vector<fpbus::Metadata> display_panel_metadata{
    {{
        .id = std::to_string(DEVICE_METADATA_DISPLAY_PANEL_CONFIG),
        // No metadata for this item.
    }},
};

static const std::vector<fpbus::Bti> display_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_DISPLAY,
    }},
};

zx::result<> PostInit::InitDisplay() {
  display_panel_t display_panel_info[] = {
      {
          .width = 600,
          .height = 1024,
      },
  };

  if (display_id_) {
    display_panel_info[0].panel_type = PANEL_INNOLUX_P070ACB_FITIPOWER_JD9364;
  } else {
    display_panel_info[0].panel_type = PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO;
  }
  display_panel_metadata[0].data() =
      std::vector(reinterpret_cast<uint8_t*>(&display_panel_info),
                  reinterpret_cast<uint8_t*>(&display_panel_info) + sizeof(display_panel_info));

  const fpbus::Node display_dev = []() {
    fpbus::Node dev = {};
    dev.name() = "display";
    dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
    dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_S905D2;
    dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_DISPLAY;
    dev.metadata() = display_panel_metadata;
    dev.mmio() = display_mmios;
    dev.irq() = display_irqs;
    dev.bti() = display_btis;
    return dev;
  }();

  std::vector<fuchsia_driver_framework::BindRule2> gpio_bind_rules{
      fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_gpio::SERVICE,
                               bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule2(bind_fuchsia::GPIO_PIN,
                               bind_fuchsia_amlogic_platform_s905d2::GPIOH_PIN_ID_PIN_6),
  };

  std::vector<fuchsia_driver_framework::NodeProperty2> gpio_properties{
      fdf::MakeProperty2(bind_fuchsia_hardware_gpio::SERVICE,
                         bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty2(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_LCD_RESET),
  };

  std::vector<fuchsia_driver_framework::BindRule2> canvas_bind_rules{
      fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                               bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT),
  };

  std::vector<fuchsia_driver_framework::NodeProperty2> canvas_properties{
      fdf::MakeProperty2(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                         bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT),
  };

  std::vector<fuchsia_driver_framework::ParentSpec2> parents = {
      {{
          .bind_rules = gpio_bind_rules,
          .properties = gpio_properties,
      }},
      {{
          .bind_rules = canvas_bind_rules,
          .properties = canvas_properties,
      }},
  };

  fuchsia_driver_framework::CompositeNodeSpec node_group{{.name = "display", .parents2 = parents}};

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('DISP');
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, display_dev),
                                                          fidl::ToWire(fidl_arena, node_group));
  if (!result.ok()) {
    FDF_LOG(ERROR, "AddCompositeSpec Display(display_dev) request failed: %s",
            result.FormatDescription().data());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "AddCompositeSpec Display(display_dev) failed: %s",
            zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  return zx::ok();
}

}  // namespace astro
