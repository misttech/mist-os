// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/hardware/clockimpl/cpp/banjo.h>
#include <fuchsia/hardware/gpioimpl/cpp/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/amlogic/platform/meson/cpp/bind.h>
#include <bind/fuchsia/amlogic/platform/t931/cpp/bind.h>
#include <bind/fuchsia/arm/platform/cpp/bind.h>
#include <bind/fuchsia/camerasensor2/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gdc/cpp/bind.h>
#include <bind/fuchsia/ge2d/cpp/bind.h>
#include <bind/fuchsia/hardware/amlogiccanvas/cpp/bind.h>
#include <bind/fuchsia/hardware/clock/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/isp/cpp/bind.h>
#include <bind/fuchsia/mipicsi/cpp/bind.h>
#include <bind/fuchsia/sony/platform/cpp/bind.h>
#include <bind/fuchsia/sysmem/cpp/bind.h>
#include <soc/aml-common/aml-registers.h>
#include <soc/aml-meson/g12b-clk.h>
#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>

#include "sherlock-gpios.h"
#include "sherlock.h"
#include "src/devices/board/drivers/sherlock/camera-isp-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

constexpr uint32_t kClk24MAltFunc = 7;
constexpr uint32_t kClkGpioDriveStrengthUa = 4000;

static const std::vector<fpbus::Mmio> ge2d_mmios{
    {{
        .base = T931_GE2D_BASE,
        .length = T931_GE2D_LENGTH,
    }},
};

static const std::vector<fpbus::Bti> ge2d_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_GE2D,
    }},
};

// IRQ for GE2D
static const std::vector<fpbus::Irq> ge2d_irqs{
    {{
        .irq = T931_MALI_GE2D_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const fpbus::Node ge2d_dev = []() {
  // GE2D
  fpbus::Node dev = {};
  dev.name() = "ge2d";
  dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_T931;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_GE2D;
  dev.mmio() = ge2d_mmios;
  dev.bti() = ge2d_btis;
  dev.irq() = ge2d_irqs;
  return dev;
}();

static const std::vector<fpbus::Mmio> gdc_mmios{
    {{
        .base = T931_HIU_BASE,
        .length = T931_HIU_LENGTH,
    }},
    {{
        .base = T931_GDC_BASE,
        .length = T931_GDC_LENGTH,
    }},
};

static const std::vector<fpbus::Bti> gdc_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_GDC,
    }},
};

// IRQ for ISP
static const std::vector<fpbus::Irq> gdc_irqs{
    {{
        .irq = T931_MALI_GDC_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const fpbus::Node gdc_dev = []() {
  // GDC
  fpbus::Node dev = {};
  dev.name() = "gdc";
  dev.vid() = bind_fuchsia_arm_platform::BIND_PLATFORM_DEV_VID_ARM;
  dev.pid() = bind_fuchsia_arm_platform::BIND_PLATFORM_DEV_PID_GDC;
  dev.did() = bind_fuchsia_arm_platform::BIND_PLATFORM_DEV_DID_MALI_IV010;
  dev.mmio() = gdc_mmios;
  dev.bti() = gdc_btis;
  dev.irq() = gdc_irqs;
  return dev;
}();

static const std::vector<fpbus::Bti> isp_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_ISP,
    }},
};

static const std::vector<fpbus::Mmio> isp_mmios{
    {{
        .base = T931_HIU_BASE,
        .length = T931_HIU_LENGTH,
    }},
    {{
        .base = T931_POWER_DOMAIN_BASE,
        .length = T931_POWER_DOMAIN_LENGTH,
    }},
    {{
        .base = T931_MEMORY_PD_BASE,
        .length = T931_MEMORY_PD_LENGTH,
    }},
    {{
        .base = T931_ISP_BASE,
        .length = T931_ISP_LENGTH,
    }},
};

// IRQ for ISP
static const std::vector<fpbus::Irq> isp_irqs{
    {{
        .irq = T931_MALI_ISP_IRQ,
        .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    }},
};

static const fpbus::Node isp_dev = []() {
  // ISP
  fpbus::Node dev = {};
  dev.name() = "isp";
  dev.vid() = PDEV_VID_ARM;
  dev.pid() = PDEV_PID_ARM_ISP;
  dev.did() = PDEV_DID_ARM_MALI_IV009;
  dev.mmio() = isp_mmios;
  dev.bti() = isp_btis;
  dev.irq() = isp_irqs;
  return dev;
}();

static const std::vector<fpbus::Mmio> mipi_mmios{
    {{
        .base = T931_CSI_PHY0_BASE,
        .length = T931_CSI_PHY0_LENGTH,
    }},
    {{
        .base = T931_APHY_BASE,
        .length = T931_APHY_LENGTH,
    }},
    {{
        .base = T931_CSI_HOST0_BASE,
        .length = T931_CSI_HOST0_LENGTH,
    }},
    {{
        .base = T931_MIPI_ADAPTER_BASE,
        .length = T931_MIPI_ADAPTER_LENGTH,
    }},
    {{
        .base = T931_HIU_BASE,
        .length = T931_HIU_LENGTH,
    }},
};

static const std::vector<fpbus::Bti> mipi_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_MIPI,
    }},
};

static const std::vector<fpbus::Irq> mipi_irqs{
    {{
        .irq = T931_MIPI_ADAPTER_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

// Binding rules for MIPI Driver
static const fpbus::Node mipi_dev = []() {
  // MIPI CSI PHY ADAPTER
  fpbus::Node dev = {};
  dev.name() = "mipi-csi2";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_AMLOGIC_T931;
  dev.did() = PDEV_DID_AMLOGIC_MIPI_CSI;
  dev.mmio() = mipi_mmios;
  dev.bti() = mipi_btis;
  dev.irq() = mipi_irqs;
  return dev;
}();

// Binding rules for Sensor Driver
static const fpbus::Node sensor_dev_sherlock = []() {
  fpbus::Node dev = {};
  dev.name() = "imx227-sensor";
  dev.vid() = bind_fuchsia_sony_platform::BIND_PLATFORM_DEV_VID_SONY;
  dev.pid() = bind_fuchsia_sony_platform::BIND_PLATFORM_DEV_PID_IMX227;
  dev.did() = bind_fuchsia_sony_platform::BIND_PLATFORM_DEV_DID_CAMERA_SENSOR;
  return dev;
}();

}  // namespace

// Refer to camera design document for driver
// design and layout details.
zx_status_t Sherlock::CameraInit() {
  // Set GPIO alternate functions.
  gpio_impl_.SetAltFunction(T931_GPIOAO(10), kClk24MAltFunc);
  gpio_impl_.SetDriveStrength(T931_GPIOAO(10), kClkGpioDriveStrengthUa, nullptr);

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('CAME');
  {
    auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, mipi_dev));
    if (!result.ok()) {
      zxlogf(ERROR, "%s: NodeAdd Camera(mipi_dev) request failed: %s", __func__,
             result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "%s: NodeAdd Camera(mipi_dev) failed: %s", __func__,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  auto imx227_sensor_mipicsi_spec = fuchsia_driver_framework::ParentSpec{{
      .bind_rules = std::vector{fdf::MakeAcceptBindRule(
          bind_fuchsia::PROTOCOL, bind_fuchsia_mipicsi::BIND_PROTOCOL_DEVICE)},
      .properties = {fdf::MakeProperty(bind_fuchsia::PROTOCOL,
                                       bind_fuchsia_mipicsi::BIND_PROTOCOL_DEVICE)},
  }};

  auto imx227_sensor_i2c_spec = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                                      bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID,
                                      bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_3),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                                      bind_fuchsia_i2c::BIND_I2C_ADDRESS_SHERLOCK),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                                bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
          },
  }};

  auto imx227_sensor_gpio_reset_spec = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                      bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
              fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                                      bind_fuchsia_amlogic_platform_t931::GPIOZ_PIN_ID_PIN_0),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::PROTOCOL,
                                bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
              fdf::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                                bind_fuchsia_hardware_gpio::FUNCTION_CAM_RESET),
          },
  }};

  auto imx227_sensor_gpio_vana_spec = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                      bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
              fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                                      bind_fuchsia_amlogic_platform_t931::GPIOA_PIN_ID_PIN_6),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::PROTOCOL,
                                bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
              fdf::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                                bind_fuchsia_hardware_gpio::FUNCTION_VANA_ENABLE),
          },
  }};

  auto imx227_sensor_gpio_vdig_spec = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                      bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
              fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                                      bind_fuchsia_amlogic_platform_t931::GPIOZ_PIN_ID_PIN_12),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::PROTOCOL,
                                bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
              fdf::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                                bind_fuchsia_hardware_gpio::FUNCTION_VDIG_ENABLE),
          },
  }};

  auto imx227_sensor_clock_sensor_spec = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                                      bind_fuchsia_hardware_clock::BIND_FIDL_PROTOCOL_DEVICE),
              fdf::MakeAcceptBindRule(
                  bind_fuchsia::CLOCK_ID,
                  bind_fuchsia_amlogic_platform_meson::G12B_CLK_ID_CLK_CAM_INCK_24M),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                                bind_fuchsia_hardware_clock::BIND_FIDL_PROTOCOL_DEVICE),
              fdf::MakeProperty(bind_fuchsia_hardware_clock::FUNCTION,
                                bind_fuchsia_hardware_clock::FUNCTION_CAMERA_SENSOR),
          },
  }};

  auto composite_spec = fuchsia_driver_framework::CompositeNodeSpec{{
      .name = "imx227_sensor",
      .parents = {{
          imx227_sensor_mipicsi_spec,
          imx227_sensor_i2c_spec,
          imx227_sensor_gpio_reset_spec,
          imx227_sensor_gpio_vana_spec,
          imx227_sensor_gpio_vdig_spec,
          imx227_sensor_clock_sensor_spec,
      }},
  }};

  fdf::WireUnownedResult spec_result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, sensor_dev_sherlock), fidl::ToWire(fidl_arena, composite_spec));

  if (!spec_result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Camera(sensor_dev_sherlock) request failed: %s",
           __func__, spec_result.FormatDescription().data());
    return spec_result.status();
  }
  if (spec_result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Camera(sensor_dev_sherlock) failed: %s", __func__,
           zx_status_get_string(spec_result->error_value()));
    return spec_result->error_value();
  }

  auto bind_rules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                              bind_fuchsia_camerasensor2::BIND_PROTOCOL_DEVICE),
  };

  auto properties = std::vector{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_camerasensor2::BIND_PROTOCOL_DEVICE),
  };

  auto parents = std::vector{
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = bind_rules,
          .properties = properties,
      }},
  };

  composite_spec = fuchsia_driver_framework::CompositeNodeSpec{{.name = "gdc", .parents = parents}};

  spec_result = pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, gdc_dev),
                                                          fidl::ToWire(fidl_arena, composite_spec));
  if (!spec_result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Camera(gdc_dev) request failed: %s", __func__,
           spec_result.FormatDescription().data());
    return spec_result.status();
  }
  if (spec_result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Camera(gdc_dev) failed: %s", __func__,
           zx_status_get_string(spec_result->error_value()));
    return spec_result->error_value();
  }

  auto ge2d_sensor_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                      bind_fuchsia_camerasensor2::BIND_PROTOCOL_DEVICE),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia::PROTOCOL,
                                bind_fuchsia_camerasensor2::BIND_PROTOCOL_DEVICE),
          },
  }};

  auto ge2d_canvas_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                                      bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                                bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT),
          },
  }};

  composite_spec = fuchsia_driver_framework::CompositeNodeSpec{{
      .name = "ge2d",
      .parents = {{ge2d_sensor_node, ge2d_canvas_node}},
  }};

  spec_result = pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, ge2d_dev),
                                                          fidl::ToWire(fidl_arena, composite_spec));
  if (!spec_result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Camera(ge2d_dev) request failed: %s", __func__,
           spec_result.FormatDescription().data());
    return spec_result.status();
  }
  if (spec_result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Camera(ge2d_dev) failed: %s", __func__,
           zx_status_get_string(spec_result->error_value()));
    return spec_result->error_value();
  }

  // Add a composite device for ARM ISP
  auto result = pbus_.buffer(arena)->AddComposite(
      fidl::ToWire(fidl_arena, isp_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, isp_fragments, std::size(isp_fragments)),
      "camera-sensor");
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddComposite Camera(isp_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddComposite Camera(isp_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  const ddk::BindRule kIspRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_isp::BIND_PROTOCOL_DEVICE),

  };

  const ddk::BindRule kGdcRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_gdc::BIND_PROTOCOL_DEVICE),

  };

  const ddk::BindRule kGe2dRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_ge2d::BIND_PROTOCOL_DEVICE),

  };

  const ddk::BindRule kSysmemRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_sysmem::BIND_PROTOCOL_DEVICE),

  };

  const device_bind_prop_t kIspProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_isp::BIND_PROTOCOL_DEVICE),
  };

  const device_bind_prop_t kGdcProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_gdc::BIND_PROTOCOL_DEVICE),
  };

  const device_bind_prop_t kGe2dProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_ge2d::BIND_PROTOCOL_DEVICE),
  };

  const device_bind_prop_t kSysmemProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_sysmem::BIND_PROTOCOL_DEVICE),
  };

  auto node_group = ddk::CompositeNodeSpec(kIspRules, kIspProperties)
                        .AddParentSpec(kGdcRules, kGdcProperties)
                        .AddParentSpec(kGe2dRules, kGe2dProperties)
                        .AddParentSpec(kSysmemRules, kSysmemProperties);

  zx_status_t status = DdkAddCompositeNodeSpec("camera_controller", node_group);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Camera Controller DdkAddCompositeNodeSpec failed %d", __func__, status);
    return status;
  }

  return status;
}

}  // namespace sherlock
