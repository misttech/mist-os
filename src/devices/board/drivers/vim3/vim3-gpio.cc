// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/ti/platform/cpp/bind.h>
#include <ddk/metadata/gpio.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "vim3-gpios.h"
#include "vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> gpio_mmios{
    {{
        .base = A311D_GPIO_BASE,
        .length = A311D_GPIO_LENGTH,
    }},
    {{
        .base = A311D_GPIO_AO_BASE,
        .length = A311D_GPIO_AO_LENGTH,
    }},
    {{
        .base = A311D_GPIO_INTERRUPT_BASE,
        .length = A311D_GPIO_INTERRUPT_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> gpio_irqs{
    {{
        .irq = A311D_GPIO_IRQ_0,
        .mode = ZX_INTERRUPT_MODE_DEFAULT,
    }},
    {{
        .irq = A311D_GPIO_IRQ_1,
        .mode = ZX_INTERRUPT_MODE_DEFAULT,
    }},
    {{
        .irq = A311D_GPIO_IRQ_2,
        .mode = ZX_INTERRUPT_MODE_DEFAULT,
    }},
    {{
        .irq = A311D_GPIO_IRQ_3,
        .mode = ZX_INTERRUPT_MODE_DEFAULT,
    }},
    {{
        .irq = A311D_GPIO_IRQ_4,
        .mode = ZX_INTERRUPT_MODE_DEFAULT,
    }},
    {{
        .irq = A311D_GPIO_IRQ_5,
        .mode = ZX_INTERRUPT_MODE_DEFAULT,
    }},
    {{
        .irq = A311D_GPIO_IRQ_6,
        .mode = ZX_INTERRUPT_MODE_DEFAULT,
    }},
    {{
        .irq = A311D_GPIO_IRQ_7,
        .mode = ZX_INTERRUPT_MODE_DEFAULT,
    }},
};

// GPIOs to expose from generic GPIO driver.
static const gpio_pin_t gpio_pins[] = {
    DECL_GPIO_PIN(VIM3_J4_PIN_39),
    DECL_GPIO_PIN(VIM3_ETH_MAC_INTR),
    DECL_GPIO_PIN(A311D_GPIOBOOT(12)),
    DECL_GPIO_PIN(A311D_GPIOX(6)),
    DECL_GPIO_PIN(VIM3_HPD_IN),
    DECL_GPIO_PIN(VIM3_FUSB302_INT),
    DECL_GPIO_PIN(VIM3_TOUCH_PANEL_INTERRUPT),
    DECL_GPIO_PIN(VIM3_WIFI_WAKE_HOST),
    DECL_GPIO_PIN(VIM3_WIFI_32K),
    DECL_GPIO_PIN(VIM3_BT_EN),
};

fuchsia_driver_framework::CompositeNodeSpec CreateGpioExpanderCompositeNodeSpec() {
  std::vector<fuchsia_driver_framework::BindRule> i2cRules = {
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_A0_0),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS, bind_fuchsia_i2c::BIND_I2C_ADDRESS_VIM3)};

  std::vector<fuchsia_driver_framework::NodeProperty> i2cProperties = {
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                        bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI),
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                        bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_TCA6408A),
  };

  std::vector<fuchsia_driver_framework::ParentSpec> parents = {fuchsia_driver_framework::ParentSpec{
      {.bind_rules = std::move(i2cRules), .properties = std::move(i2cProperties)}}};

  return fuchsia_driver_framework::CompositeNodeSpec{
      {.name = "gpio-expander", .parents = std::move(parents)}};
}

fpbus::Node CreateGpioExpanderPbusNode() {
  static const gpio_pin_t kGpioExpanderPins[] = {
      DECL_GPIO_PIN(VIM3_LCD_RESET),
      DECL_GPIO_PIN(VIM3_LCD_BACKLIGHT_ENABLE),
      DECL_GPIO_PIN(VIM3_TOUCH_PANEL_RESET),
      DECL_GPIO_PIN(VIM3_SD_MODE),
  };

  static const uint32_t kGpioExpanderPinOffset = VIM3_EXPANDER_GPIO_START;

  fpbus::Node dev = {};
  dev.name() = "gpio-expander";
  dev.vid() = bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_TCA6408A;
  dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_GPIO_PINS,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&kGpioExpanderPins),
              reinterpret_cast<const uint8_t*>(&kGpioExpanderPins) + sizeof(kGpioExpanderPins)),
      }},
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&kGpioExpanderPinOffset),
                                       reinterpret_cast<const uint8_t*>(&kGpioExpanderPinOffset) +
                                           sizeof(kGpioExpanderPinOffset)),
      }}};
  return dev;
}

zx_status_t Vim3::GpioInit() {
  fuchsia_hardware_gpio::wire::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fuchsia_hardware_gpio::wire::InitStep>::FromExternal(
      gpio_init_steps_.data(), gpio_init_steps_.size());

  const fit::result encoded_metadata = fidl::Persist(metadata);
  if (!encoded_metadata.is_ok()) {
    zxlogf(ERROR, "Failed to encode GPIO init metadata: %s",
           encoded_metadata.error_value().FormatDescription().c_str());
    return encoded_metadata.error_value().status();
  }

  const std::vector<fpbus::Metadata> gpio_metadata{
      {{
          .type = DEVICE_METADATA_GPIO_PINS,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&gpio_pins),
              reinterpret_cast<const uint8_t*>(&gpio_pins) + sizeof(gpio_pins)),
      }},
      {{
          .type = DEVICE_METADATA_GPIO_INIT,
          .data = encoded_metadata.value(),
      }},
  };

  fpbus::Node gpio_dev;
  gpio_dev.name() = "gpio";
  gpio_dev.vid() = PDEV_VID_AMLOGIC;
  gpio_dev.pid() = PDEV_PID_AMLOGIC_A311D;
  gpio_dev.did() = PDEV_DID_AMLOGIC_GPIO;
  gpio_dev.mmio() = gpio_mmios;
  gpio_dev.irq() = gpio_irqs;
  gpio_dev.metadata() = gpio_metadata;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('GPIO');
  auto result = pbus_.buffer(arena)->ProtocolNodeAdd(ZX_PROTOCOL_GPIO_IMPL,
                                                     fidl::ToWire(fidl_arena, gpio_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: ProtocolNodeAdd Gpio(gpio_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: ProtocolNodeAdd Gpio(gpio_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  gpio_impl_ = ddk::GpioImplProtocolClient(parent());
  if (!gpio_impl_.is_valid()) {
    zxlogf(ERROR, "%s: device_get_protocol failed", __func__);
    return ZX_ERR_INTERNAL;
  }

  fdf::Arena gpio_expander_arena('XPND');
  fdf::WireUnownedResult gpio_expander_result =
      pbus_.buffer(gpio_expander_arena)
          ->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, CreateGpioExpanderPbusNode()),
                                 fidl::ToWire(fidl_arena, CreateGpioExpanderCompositeNodeSpec()));
  if (!gpio_expander_result.ok() || gpio_expander_result.value().is_error()) {
    zxlogf(ERROR, "AddComposite for big core failed, error = %s",
           gpio_expander_result.FormatDescription().c_str());
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

}  // namespace vim3
