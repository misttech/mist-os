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
#include <zircon/syscalls/smc.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/display/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/amlogiccanvas/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/sysmem/cpp/bind.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "vim3-gpios.h"
#include "vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> display_mmios{
    {{
        // VPU
        .base = A311D_VPU_BASE,
        .length = A311D_VPU_LENGTH,
    }},
    {{
        // MIPI DSI "TOP"
        .base = A311D_TOP_MIPI_DSI_BASE,
        .length = A311D_TOP_MIPI_DSI_LENGTH,
    }},
    {{
        // MIPI DSI PHY
        .base = A311D_DSI_PHY_BASE,
        .length = A311D_DSI_PHY_LENGTH,
    }},
    {{
        // DSI Host Controller
        .base = A311D_MIPI_DSI_BASE,
        .length = A311D_MIPI_DSI_LENGTH,
    }},
    {{
        // HIU / HHI
        .base = A311D_HIU_BASE,
        .length = A311D_HIU_LENGTH,
    }},
    {{
        // AOBUS
        // TODO(https://fxbug.dev/42081392): Restrict range to RTI
        .base = A311D_AOBUS_BASE,
        .length = A311D_AOBUS_LENGTH,
    }},
    {{
        // RESET
        .base = A311D_RESET_BASE,
        .length = A311D_RESET_LENGTH,
    }},
    {{
        // PERIPHS_REGS (GPIO Multiplexer)
        .base = A311D_GPIO_BASE,
        .length = A311D_GPIO_LENGTH,
    }},
    {{
        // HDMITX (HDMI transmitter) Controller IP registers
        .base = A311D_HDMITX_CONTROLLER_IP_BASE,
        .length = A311D_HDMITX_CONTROLLER_IP_LENGTH,
    }},
    {{
        // HDMITX (HDMI transmitter) Top-level registers
        .base = A311D_HDMITX_TOP_LEVEL_BASE,
        .length = A311D_HDMITX_TOP_LEVEL_LENGTH,
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

static const std::vector<fpbus::Smc> kDisplaySmcs{
    {{
        .service_call_num_base = ARM_SMC_SERVICE_CALL_NUM_SIP_SERVICE_BASE,
        .count = 1,
        .exclusive = false,
    }},
};

const ddk::BindRule kGpioLcdResetRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                            bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(VIM3_LCD_RESET)),
};

const device_bind_prop_t kGpioLcdReserProperties[] = {
    ddk::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                      bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    ddk::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_LCD_RESET),
};

std::vector<fuchsia_driver_framework::ParentSpec> GetDisplayCommonParents() {
  std::vector<fuchsia_driver_framework::BindRule> gpio_lcd_reset_bind_rules{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, VIM3_EXPANDER_GPIO_ID),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(VIM3_LCD_RESET)),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> gpio_lcd_reset_properties{
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                        bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_LCD_RESET),
  };

  std::vector<fuchsia_driver_framework::BindRule> gpio_hdmi_hotplug_detect_bind_rules{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, VIM3_GPIO_ID),
      fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(VIM3_HPD_IN)),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> gpio_hdmi_hotplug_detect_properties{
      fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                        bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION,
                        bind_fuchsia_gpio::FUNCTION_HDMI_HOTPLUG_DETECT),
  };

  std::vector<fuchsia_driver_framework::BindRule> sysmem_bind_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_sysmem::SERVICE,
                              bind_fuchsia_hardware_sysmem::SERVICE_ZIRCONTRANSPORT),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> sysmem_properties = std::vector{
      fdf::MakeProperty(bind_fuchsia_hardware_sysmem::SERVICE,
                        bind_fuchsia_hardware_sysmem::SERVICE_ZIRCONTRANSPORT),
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

  return parents;
}

zx_status_t CreateHdmiDisplay(
    fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  fpbus::Node display_dev;
  display_dev.name() = "hdmi-display";
  display_dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  display_dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_A311D;
  display_dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_DISPLAY;
  display_dev.mmio() = display_mmios;
  display_dev.irq() = display_irqs;
  display_dev.bti() = display_btis;
  display_dev.smc() = kDisplaySmcs;

  auto parents = GetDisplayCommonParents();

  std::vector<fuchsia_driver_framework::BindRule> hdmi_display_bind{
      fdf::MakeAcceptBindRule(bind_fuchsia_display::OUTPUT, bind_fuchsia_display::OUTPUT_HDMI),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> hdmi_display_properties{
      fdf::MakeProperty(bind_fuchsia_display::OUTPUT, bind_fuchsia_display::OUTPUT_HDMI),
  };

  parents.emplace_back(hdmi_display_bind, hdmi_display_properties);

  fuchsia_driver_framework::CompositeNodeSpec spec{{.name = "hdmi-display", .parents = parents}};

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('DISP');
  auto result = pbus.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, display_dev),
                                                         fidl::ToWire(fidl_arena, spec));
  if (!result.ok() || result->is_error()) {
    zx_status_t status = result.ok() ? result->error_value() : result.status();
    zxlogf(ERROR, "AddCompositeSpec Display(hdmi-display) request failed: %s",
           zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t CreateMipiDsiDisplay(
    fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  static const display_panel_t display_panel_info[] = {
      {
          .width = 1080,
          .height = 1920,
          .panel_type = PANEL_MICROTECH_MTF050FHDI03_NOVATEK_NT35596,
      },
  };

  std::vector<fpbus::Metadata> display_panel_metadata{
      {{
          .type = DEVICE_METADATA_DISPLAY_PANEL_CONFIG,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&display_panel_info),
              reinterpret_cast<const uint8_t*>(&display_panel_info) + sizeof(display_panel_info)),
      }},
  };

  fpbus::Node display_dev;
  display_dev.name() = "dsi-display";
  display_dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  display_dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_A311D;
  display_dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_DISPLAY;
  display_dev.instance_id() = 1;
  display_dev.mmio() = display_mmios;
  display_dev.irq() = display_irqs;
  display_dev.bti() = display_btis;
  display_dev.smc() = kDisplaySmcs;

  display_dev.metadata() = std::move(display_panel_metadata);

  auto parents = GetDisplayCommonParents();

  std::vector<fuchsia_driver_framework::BindRule> dsi_display_bind{
      fdf::MakeAcceptBindRule(bind_fuchsia_display::OUTPUT, bind_fuchsia_display::OUTPUT_MIPI_DSI),
  };

  std::vector<fuchsia_driver_framework::NodeProperty> dsi_display_properties{
      fdf::MakeProperty(bind_fuchsia_display::OUTPUT, bind_fuchsia_display::OUTPUT_MIPI_DSI),
  };

  parents.emplace_back(dsi_display_bind, dsi_display_properties);

  fuchsia_driver_framework::CompositeNodeSpec spec{{.name = "dsi-display", .parents = parents}};

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('DISP');
  auto result = pbus.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, display_dev),
                                                         fidl::ToWire(fidl_arena, spec));
  if (!result.ok() || result->is_error()) {
    zx_status_t status = result.ok() ? result->error_value() : result.status();
    zxlogf(ERROR, "AddCompositeSpec Display(dsi-display) request failed: %s",
           zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t Vim3::DisplayInit() {
  zx_status_t status = DdkAddCompositeNodeSpec(
      "display-detect", ddk::CompositeNodeSpec(kGpioLcdResetRules, kGpioLcdReserProperties));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAddCompositeNodeSpec display-detect failed: %s",
           zx_status_get_string(status));
    return status;
  }

  // Create both HDMI and MIPI DSI composite devices. The display detect driver will add either a
  // HDMI or a MIPI DSI display child based on what output is detected in the system. Only one of
  // the below composite device will get completed and hence started at runtime.

  status = CreateHdmiDisplay(pbus_);
  if (status != ZX_OK) {
    return status;
  }

  status = CreateMipiDsiDisplay(pbus_);
  if (status != ZX_OK) {
    return status;
  }

  return ZX_OK;
}

}  // namespace vim3
