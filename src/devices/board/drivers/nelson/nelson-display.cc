// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/display-panel.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/amlogic/platform/s905d3/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/amlogiccanvas/cpp/bind.h>
#include <bind/fuchsia/hardware/dsi/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/sysmem/cpp/bind.h>
#include <ddk/metadata/display.h>
#include <soc/aml-s905d3/s905d3-gpio.h>
#include <soc/aml-s905d3/s905d3-hw.h>

#include "nelson-gpios.h"
#include "nelson.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> display_mmios{
    {{
        // VBUS/VPU
        .base = S905D3_VPU_BASE,
        .length = S905D3_VPU_LENGTH,
    }},
    {{
        // TOP DSI Host Controller (Amlogic Specific)
        .base = S905D3_MIPI_TOP_DSI_BASE,
        .length = S905D3_MIPI_TOP_DSI_LENGTH,
    }},
    {{
        // DSI PHY
        .base = S905D3_DSI_PHY_BASE,
        .length = S905D3_DSI_PHY_LENGTH,
    }},
    {{
        // HHI
        .base = S905D3_HIU_BASE,
        .length = S905D3_HIU_LENGTH,
    }},
    {{
        // AOBUS
        .base = S905D3_AOBUS_BASE,
        .length = S905D3_AOBUS_LENGTH,
    }},
    {{
        // CBUS
        .base = S905D3_CBUS_BASE,
        .length = S905D3_CBUS_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> display_irqs{
    {{
        .irq = S905D3_VIU1_VSYNC_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = S905D3_RDMA_DONE,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = S905D3_VID1_WR,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Bti> display_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_DISPLAY,
    }},
};

// Composite binding rules for display driver.

// DisplayInit's bootloader_display_id must match the enum used by u-boot and the GT6853 touch
// driver.

uint32_t uboot_mapping[] = {
    PANEL_UNKNOWN,           // 0 - invalid
    PANEL_KD070D82_FT,       // 1
    PANEL_TV070WSM_FT,       // 2
    PANEL_P070ACB_FT,        // 3 - should be unused
    PANEL_KD070D82_FT_9365,  // 4
    PANEL_TV070WSM_FT_9365,  // 5
    PANEL_TV070WSM_ST7703I,  // 6
};
zx_status_t Nelson::DisplayInit(uint32_t bootloader_display_id) {
  display_panel_t display_panel_info[] = {
      {
          .width = 600,
          .height = 1024,
          .panel_type = PANEL_UNKNOWN,
      },
  };

  if (bootloader_display_id && bootloader_display_id < std::size(uboot_mapping)) {
    display_panel_info[0].panel_type = uboot_mapping[bootloader_display_id];
    zxlogf(DEBUG, "%s: bootloader provided display panel %d", __func__,
           display_panel_info[0].panel_type);
  }
  if (display_panel_info[0].panel_type == PANEL_UNKNOWN) {
    auto display_id = GetDisplayId();
    switch (display_id) {
      case 0b10:
        display_panel_info[0].panel_type = PANEL_TV070WSM_FT;
        break;
      case 0b11:
        display_panel_info[0].panel_type = PANEL_TV070WSM_FT_9365;
        break;
      case 0b01:
        display_panel_info[0].panel_type = PANEL_KD070D82_FT_9365;
        break;
      case 0b00:
        display_panel_info[0].panel_type = PANEL_KD070D82_FT;
        break;
      default:
        zxlogf(ERROR, "%s: invalid display panel detected: %d", __func__, display_id);
        return ZX_ERR_INVALID_ARGS;
    }
  }
  const std::vector<fpbus::Metadata> display_panel_metadata{
      {{
          .type = DEVICE_METADATA_DISPLAY_CONFIG,
          .data = std::vector<uint8_t>(
              reinterpret_cast<uint8_t*>(&display_panel_info),
              reinterpret_cast<uint8_t*>(&display_panel_info) + sizeof(display_panel_info)),
          // No metadata for this item.
      }},
  };

  fpbus::Node display_dev;
  display_dev.name() = "display";
  display_dev.vid() = PDEV_VID_AMLOGIC;
  display_dev.pid() = PDEV_PID_AMLOGIC_S905D3;
  display_dev.did() = PDEV_DID_AMLOGIC_DISPLAY;
  display_dev.metadata() = display_panel_metadata;
  display_dev.mmio() = display_mmios;
  display_dev.irq() = display_irqs;
  display_dev.bti() = display_btis;

  std::vector<fuchsia_driver_framework::BindRule> dsi_bind_rules{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_dsi::BIND_PROTOCOL_IMPL),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> dsi_properties{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_dsi::BIND_PROTOCOL_IMPL),
  };

  std::vector<fuchsia_driver_framework::BindRule> gpio_bind_rules{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_13),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> gpio_properties{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_LCD_RESET),
  };

  std::vector<fuchsia_driver_framework::BindRule> sysmem_bind_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_sysmem::BIND_FIDL_PROTOCOL_DEVICE),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> sysmem_properties = std::vector{
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_sysmem::BIND_FIDL_PROTOCOL_DEVICE),
  };

  std::vector<fuchsia_driver_framework::BindRule> canvas_bind_rules{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                              bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> canvas_properties{
      fdf::MakeProperty(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                        bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT),
  };

  std::vector<fuchsia_driver_framework::ParentSpec> parents = {
      {{
          .bind_rules = dsi_bind_rules,
          .properties = dsi_properties,
      }},
      {{
          .bind_rules = gpio_bind_rules,
          .properties = gpio_properties,
      }},
      {{
          .bind_rules = sysmem_bind_rules,
          .properties = sysmem_properties,
      }},
      {{
          .bind_rules = canvas_bind_rules,
          .properties = canvas_properties,
      }},
  };

  fuchsia_driver_framework::CompositeNodeSpec spec{{.name = "display", .parents = parents}};

  // TODO(payamm): Change from "dsi" to nullptr to separate DSI and Display into two different
  // driver hosts once support for it lands.
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('DISP');
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, display_dev),
                                                          fidl::ToWire(fidl_arena, spec));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddNodeGroup Display(display_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddNodeGroup Display(display_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace nelson
