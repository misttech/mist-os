// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/mmio/mmio.h>
#include <unistd.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/pwm/cpp/bind.h>
#include <sdk/lib/driver/component/cpp/composite_node_spec.h>
#include <sdk/lib/driver/component/cpp/node_add_args.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> bt_uart_mmios{
    {{
        .base = A311D_UART_EE_A_BASE,
        .length = A311D_UART_EE_A_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> bt_uart_irqs{
    {{
        .irq = A311D_UART_EE_A_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const fuchsia_hardware_serial::wire::SerialPortInfo bt_uart_serial_info = {
    .serial_class = fuchsia_hardware_serial::Class::kBluetoothHci,
    .serial_vid = PDEV_VID_BROADCOM,
    .serial_pid = PDEV_PID_BCM4359,
};

static const std::vector<fpbus::BootMetadata> bt_uart_boot_metadata{
    {{
        .zbi_type = DEVICE_METADATA_MAC_ADDRESS,
        .zbi_extra = MACADDR_BLUETOOTH,
    }},
};

constexpr fuchsia_power_broker::PowerLevel kPowerLevelOff =
    static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOff);
constexpr fuchsia_power_broker::PowerLevel kPowerLevelHandling =
    static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOn);

// Construct the PowerElementConfiguration for "wake-on-request" power element. This power element
// has an active dependency on SAG's "WakeHandling" power element, which allows aml-uart driver to
// wake the system up or prevent it from suspension when an interrupt comes.
fuchsia_hardware_power::PowerElementConfiguration wake_on_interrupt_power_config() {
  constexpr char kPowerElementName[] = "aml-uart-wake-on-interrupt";

  auto transitions_from_off =
      std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
          .target_level = kPowerLevelHandling,
          .latency_us = 0,
      }}};

  auto transitions_from_handling =
      std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
          .target_level = kPowerLevelOff,
          .latency_us = 0,
      }}};

  fuchsia_hardware_power::PowerLevel off = {
      {.level = kPowerLevelOff, .name = "off", .transitions = transitions_from_off}};
  fuchsia_hardware_power::PowerLevel handling = {
      {.level = kPowerLevelHandling, .name = "handling", .transitions = transitions_from_handling}};

  fuchsia_hardware_power::PowerElement wake_on_interrupt = {{
      .name = kPowerElementName,
      .levels = {{off, handling}},
  }};

  fuchsia_hardware_power::LevelTuple handling_to_active = {{
      .child_level = kPowerLevelHandling,
      .parent_level = static_cast<uint8_t>(fuchsia_power_system::WakeHandlingLevel::kActive),
  }};

  fuchsia_hardware_power::PowerDependency active_on_wake_handling_active = {{
      .child = kPowerElementName,
      .parent = fuchsia_hardware_power::ParentElement::WithSag(
          fuchsia_hardware_power::SagElement::kWakeHandling),
      .level_deps = {{handling_to_active}},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration wake_on_interrupt_config = {
      {.element = wake_on_interrupt, .dependencies = {{active_on_wake_handling_active}}}};

  return wake_on_interrupt_config;
}

std::vector<fuchsia_hardware_power::PowerElementConfiguration> bt_uart_power_configs() {
  return std::vector<fuchsia_hardware_power::PowerElementConfiguration>{
      wake_on_interrupt_power_config()};
}

zx_status_t Vim3::BluetoothInit() {
  // set alternate functions to enable Bluetooth UART
  gpio_init_steps_.push_back({A311D_UART_EE_A_TX, GpioSetAltFunction(A311D_UART_EE_A_TX_FN)});
  gpio_init_steps_.push_back({A311D_UART_EE_A_RX, GpioSetAltFunction(A311D_UART_EE_A_RX_FN)});
  gpio_init_steps_.push_back({A311D_UART_EE_A_CTS, GpioSetAltFunction(A311D_UART_EE_A_CTS_FN)});
  gpio_init_steps_.push_back({A311D_UART_EE_A_RTS, GpioSetAltFunction(A311D_UART_EE_A_RTS_FN)});

  // Bind UART for Bluetooth HCI
  fdf::Arena arena('BLUE');

  fuchsia_driver_framework::wire::BindRule kPwmBindRules[] = {
      // TODO(https://fxbug.dev/42079489): Replace this with wire type function.
      fidl::ToWire(arena, fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                                  bind_fuchsia_pwm::BIND_INIT_STEP_PWM)),
  };

  fuchsia_driver_framework::wire::NodeProperty kPwmProperties[] = {
      fdf::MakeProperty(arena, bind_fuchsia::INIT_STEP, bind_fuchsia_pwm::BIND_INIT_STEP_PWM),
  };

  fuchsia_driver_framework::wire::BindRule kGpioBindRules[] = {
      fidl::ToWire(arena, fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                                  bind_fuchsia_gpio::BIND_INIT_STEP_GPIO)),
  };

  fuchsia_driver_framework::wire::NodeProperty kGpioProperties[] = {
      fdf::MakeProperty(arena, bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };

  auto parents = std::vector{
      fuchsia_driver_framework::wire::ParentSpec{
          .bind_rules = fidl::VectorView<fuchsia_driver_framework::wire::BindRule>::FromExternal(
              kPwmBindRules, 1),
          .properties =
              fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty>::FromExternal(
                  kPwmProperties, 1),
      },
      fuchsia_driver_framework::wire::ParentSpec{
          .bind_rules = fidl::VectorView<fuchsia_driver_framework::wire::BindRule>::FromExternal(
              kGpioBindRules, 1),
          .properties =
              fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty>::FromExternal(
                  kGpioProperties, 1),
      },
  };

  auto builder =
      fuchsia_driver_framework::wire::CompositeNodeSpec::Builder(arena)
          .name("bluetooth-composite-spec")
          .parents(fidl::VectorView<fuchsia_driver_framework::wire::ParentSpec>(arena, parents));

  fit::result encoded = fidl::Persist(bt_uart_serial_info);
  if (encoded.is_error()) {
    zxlogf(ERROR, "Failed to encode serial metadata: %s",
           encoded.error_value().FormatDescription().c_str());
    return encoded.error_value().status();
  }

  const std::vector<fpbus::Metadata> bt_uart_metadata{
      {{
          .type = DEVICE_METADATA_SERIAL_PORT_INFO,
          .data = *std::move(encoded),
      }},
  };

  const fpbus::Node bt_uart_dev = [&]() {
    fpbus::Node dev = {};
    dev.name() = "bt-uart";
    dev.vid() = PDEV_VID_AMLOGIC;
    dev.pid() = PDEV_PID_GENERIC;
    dev.did() = PDEV_DID_AMLOGIC_UART;
    dev.mmio() = bt_uart_mmios;
    dev.irq() = bt_uart_irqs;
    dev.metadata() = bt_uart_metadata;
    dev.boot_metadata() = bt_uart_boot_metadata;
    dev.power_config() = bt_uart_power_configs();
    return dev;
  }();

  // Create composite spec for aml-uart based on UART and PWM nodes. The parent spec of bt_uart_dev
  // will be generated by the handler of AddCompositeNodeSpec.
  auto result =
      pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(arena, bt_uart_dev), builder.Build());
  if (!result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Bluetooth(bt_uart_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Bluetooth(bt_uart_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace vim3
