// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
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
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/amlogiccanvas/cpp/bind.h>
#include <bind/fuchsia/hardware/dsi/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/hdmi/cpp/bind.h>
#include <bind/fuchsia/sysmem/cpp/bind.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"
#include "vim3-gpios.h"
#include "vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> display_mmios{
    {{
        // VBUS/VPU
        .base = A311D_VPU_BASE,
        .length = A311D_VPU_LENGTH,
    }},
    {{
        // DSI Host Controller
        .base = A311D_TOP_MIPI_DSI_BASE,
        .length = A311D_TOP_MIPI_DSI_LENGTH,
    }},
    {{
        // DSI PHY
        .base = A311D_DSI_PHY_BASE,
        .length = A311D_DSI_PHY_LENGTH,
    }},
    {{
        // HHI
        .base = A311D_HIU_BASE,
        .length = A311D_HIU_LENGTH,
    }},
    {{
        // AOBUS
        .base = A311D_AOBUS_BASE,
        .length = A311D_AOBUS_LENGTH,
    }},
    {{
        // CBUS
        .base = A311D_CBUS_BASE,
        .length = A311D_CBUS_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> display_irqs{
    {{
        .irq = A311D_VIU1_VSYNC_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_RDMA_DONE_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_VID1_WR_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Bti> display_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_DISPLAY,
    }},
};

zx_status_t Vim3::DisplayInit() {
  static const display_panel_t display_panel_info[] = {
      {
          .width = 1080,
          .height = 1920,
          .panel_type = PANEL_MTF050FHDI_03,
      },
  };

  std::vector<fpbus::Metadata> display_panel_metadata{
      {{
          .type = DEVICE_METADATA_DISPLAY_CONFIG,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&display_panel_info),
              reinterpret_cast<const uint8_t*>(&display_panel_info) + sizeof(display_panel_info)),
      }},
  };

  static const fpbus::Node display_dev = [&]() {
    fpbus::Node dev = {};
    dev.name() = "display";
    dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
    dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_A311D;
    dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_DISPLAY;
    if (HasLcd()) {
      dev.metadata() = std::move(display_panel_metadata);
    }
    dev.mmio() = display_mmios;
    dev.irq() = display_irqs;
    dev.bti() = display_btis;
    return dev;
  }();

  std::vector<fuchsia_driver_framework::BindRule> dsi_bind_rules{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_dsi::BIND_PROTOCOL_IMPL),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> dsi_properties{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_dsi::BIND_PROTOCOL_IMPL),
  };

  std::vector<fuchsia_driver_framework::BindRule> hdmi_bind_rules{
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_hardware_hdmi::BIND_FIDL_PROTOCOL_SERVICE),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> hdmi_properties{
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_hardware_hdmi::BIND_FIDL_PROTOCOL_SERVICE),
  };

  std::vector<fuchsia_driver_framework::BindRule> gpio_lcd_reset_bind_rules{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(VIM3_LCD_RESET)),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> gpio_lcd_reset_properties{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_LCD_RESET),
  };

  std::vector<fuchsia_driver_framework::BindRule> gpio_hdmi_hotplug_detect_bind_rules{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(VIM3_HPD_IN)),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> gpio_hdmi_hotplug_detect_properties{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                        bind_fuchsia_hardware_gpio::FUNCTION_HDMI_HOTPLUG_DETECT),
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

  std::vector<fuchsia_driver_framework::ParentSpec> parents{
      {{
          .bind_rules = dsi_bind_rules,
          .properties = dsi_properties,
      }},
      {{
          .bind_rules = hdmi_bind_rules,
          .properties = hdmi_properties,
      }},
      {{
          .bind_rules = gpio_lcd_reset_bind_rules,
          .properties = gpio_lcd_reset_properties,
      }},
      {{
          .bind_rules = gpio_hdmi_hotplug_detect_bind_rules,
          .properties = gpio_hdmi_hotplug_detect_properties,
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

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('DISP');
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, display_dev),
                                                          fidl::ToWire(fidl_arena, spec));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeSpec Display(display_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeSpec Display(display_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace vim3
