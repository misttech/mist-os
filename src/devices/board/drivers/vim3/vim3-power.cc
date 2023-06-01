// Copyright 2021 The Fuchsia Authors. All rights reserved.
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

#include <string>

#include <bind/fuchsia/amlogic/platform/a311d/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/pwm/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/power/cpp/bind.h>
#include <ddk/metadata/power.h>
#include <ddk/metadata/pwm.h>
#include <ddktl/device.h>
#include <soc/aml-a311d/a311d-power.h>
#include <soc/aml-a311d/a311d-pwm.h>
#include <soc/aml-common/aml-power.h>

#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"
#include "src/devices/lib/metadata/llcpp/vreg.h"
#include "vim3-gpios.h"
#include "vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

constexpr voltage_pwm_period_ns_t kA311dPwmPeriodNs = 1500;

const uint32_t kVoltageStepUv = 10'000;
static_assert((kMaxVoltageUv - kMinVoltageUv) % kVoltageStepUv == 0,
              "Voltage step must be a factor of (kMaxVoltageUv - kMinVoltageUv)\n");
const uint32_t kNumSteps = (kMaxVoltageUv - kMinVoltageUv) / kVoltageStepUv + 1;

enum VregIdx {
  PWM_AO_D_VREG,
  PWM_A_VREG,

  VREG_COUNT,
};

constexpr zx_bind_inst_t vreg_pwm_ao_d_match[] = {
    BI_ABORT_IF(NE, BIND_FIDL_PROTOCOL, ZX_FIDL_PROTOCOL_VREG),
    BI_MATCH_IF(EQ, BIND_PWM_ID, A311D_PWM_AO_D),
};

constexpr zx_bind_inst_t vreg_pwm_a_match[] = {
    BI_ABORT_IF(NE, BIND_FIDL_PROTOCOL, ZX_FIDL_PROTOCOL_VREG),
    BI_MATCH_IF(EQ, BIND_PWM_ID, A311D_PWM_A),
};

constexpr device_fragment_part_t vreg_pwm_ao_d_fragment[] = {
    {std::size(vreg_pwm_ao_d_match), vreg_pwm_ao_d_match},
};

constexpr device_fragment_part_t vreg_pwm_a_fragment[] = {
    {std::size(vreg_pwm_a_match), vreg_pwm_a_match},
};

// Fragments for the "power-impl" composite.
constexpr device_fragment_t power_impl_fragments[] = {
    {"vreg-pwm-ao-d", std::size(vreg_pwm_ao_d_fragment), vreg_pwm_ao_d_fragment},
    {"vreg-pwm-a", std::size(vreg_pwm_a_fragment), vreg_pwm_a_fragment},
};

constexpr zx_bind_inst_t power_impl_driver_match[] = {
    BI_MATCH_IF(EQ, BIND_PROTOCOL, bind_fuchsia_power::BIND_PROTOCOL_IMPL),
};

constexpr device_fragment_part_t core_power_impl_fragment[] = {
    {std::size(power_impl_driver_match), power_impl_driver_match},
};

constexpr zx_bind_inst_t core_power_pdev_match_big[] = {
    BI_ABORT_IF(NE, BIND_PROTOCOL, ZX_PROTOCOL_PDEV),
    BI_ABORT_IF(NE, BIND_PLATFORM_DEV_VID, PDEV_VID_GENERIC),
    BI_ABORT_IF(NE, BIND_PLATFORM_DEV_PID, PDEV_PID_GENERIC),
    BI_ABORT_IF(NE, BIND_PLATFORM_DEV_DID, PDEV_DID_POWER_CORE),
    BI_MATCH_IF(EQ, BIND_PLATFORM_DEV_INSTANCE_ID, 0),
};

constexpr zx_bind_inst_t core_power_pdev_match_little[] = {
    BI_ABORT_IF(NE, BIND_PROTOCOL, ZX_PROTOCOL_PDEV),
    BI_ABORT_IF(NE, BIND_PLATFORM_DEV_VID, PDEV_VID_GENERIC),
    BI_ABORT_IF(NE, BIND_PLATFORM_DEV_PID, PDEV_PID_GENERIC),
    BI_ABORT_IF(NE, BIND_PLATFORM_DEV_DID, PDEV_DID_POWER_CORE),
    BI_MATCH_IF(EQ, BIND_PLATFORM_DEV_INSTANCE_ID, 1),
};

const device_fragment_part_t core_power_pdev_fragment_big[] = {
    {std::size(core_power_pdev_match_big), core_power_pdev_match_big},
};

const device_fragment_part_t core_power_pdev_fragment_little[] = {
    {std::size(core_power_pdev_match_little), core_power_pdev_match_little},
};

// Fragments for the core power composite.
constexpr device_fragment_t power_domain_core_fragments_big[] = {
    {"power-impl", std::size(core_power_impl_fragment), core_power_impl_fragment},
    {"pdev", std::size(core_power_pdev_fragment_big), core_power_pdev_fragment_big},
};

constexpr device_fragment_t power_domain_core_fragments_little[] = {
    {"power-impl", std::size(core_power_impl_fragment), core_power_impl_fragment},
    {"pdev", std::size(core_power_pdev_fragment_little), core_power_pdev_fragment_little},
};

static const fpbus::Node power_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "aml-power-impl-composite";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_AMLOGIC_A311D;
  dev.did() = PDEV_DID_AMLOGIC_POWER;
  return dev;
}();

constexpr power_domain_t big_domain[] = {
    {static_cast<uint32_t>(A311dPowerDomains::kArmCoreBig)},
};

constexpr power_domain_t little_domain[] = {
    {static_cast<uint32_t>(A311dPowerDomains::kArmCoreLittle)},
};

const ddk::BindRule kI2cRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_A0_0),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS, 0x22u)};

const device_bind_prop_t kI2cProperties[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
};

const ddk::BindRule kGpioRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(VIM3_FUSB302_INT))};

const device_bind_prop_t kGpioProperties[] = {
    ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
    ddk::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_USB_POWER_DELIVERY)};

zx_status_t AddBigCore(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  fpbus::Node big_core_dev;
  big_core_dev.name() = "pd-big-core";
  big_core_dev.vid() = PDEV_VID_GENERIC;
  big_core_dev.pid() = PDEV_PID_GENERIC;
  big_core_dev.did() = PDEV_DID_POWER_CORE;
  big_core_dev.instance_id() = 0;
  big_core_dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_POWER_DOMAINS,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&big_domain),
              reinterpret_cast<const uint8_t*>(&big_domain) + sizeof(big_domain)),
      }},
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena big_core_arena('BIGC');

  auto fragments = platform_bus_composite::MakeFidlFragment(
      fidl_arena, power_domain_core_fragments_big, std::size(power_domain_core_fragments_big));

  fdf::WireUnownedResult big_core_result =
      pbus.buffer(big_core_arena)
          ->AddComposite(fidl::ToWire(fidl_arena, big_core_dev), fragments, "power-impl");
  if (!big_core_result.ok() || big_core_result.value().is_error()) {
    zxlogf(ERROR, "AddComposite for big core failed, error = %s",
           big_core_result.FormatDescription().c_str());
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

zx_status_t AddLittleCore(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  fpbus::Node little_core_dev;
  little_core_dev.name() = "pd-little-core";
  little_core_dev.vid() = PDEV_VID_GENERIC;
  little_core_dev.pid() = PDEV_PID_GENERIC;
  little_core_dev.did() = PDEV_DID_POWER_CORE;
  little_core_dev.instance_id() = 1;
  little_core_dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_POWER_DOMAINS,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&little_domain),
              reinterpret_cast<const uint8_t*>(&little_domain) + sizeof(little_domain)),
      }},
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena little_core_arena('LITC');

  auto fragments =
      platform_bus_composite::MakeFidlFragment(fidl_arena, power_domain_core_fragments_little,
                                               std::size(power_domain_core_fragments_little));

  fdf::WireUnownedResult little_core_result =
      pbus.buffer(little_core_arena)
          ->AddComposite(fidl::ToWire(fidl_arena, little_core_dev), fragments, "power-impl");
  if (!little_core_result.ok() || little_core_result.value().is_error()) {
    zxlogf(ERROR, "AddComposite for little core failed, error = %s",
           little_core_result.FormatDescription().c_str());
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

}  // namespace

zx_status_t Vim3::PowerInit() {
  zx_status_t st;
  st = gpio_impl_.ConfigOut(A311D_GPIOE(1), 0);
  if (st != ZX_OK) {
    zxlogf(ERROR, "%s: ConfigOut failed: %d", __func__, st);
    return st;
  }

  // Configure the GPIO to be Output & set it to alternate
  // function 3 which puts in PWM_D mode. A53 cluster (Small)
  st = gpio_impl_.SetAltFunction(A311D_GPIOE(1), A311D_GPIOE_1_PWM_D_FN);
  if (st != ZX_OK) {
    zxlogf(ERROR, "%s: SetAltFunction failed: %d", __func__, st);
    return st;
  }

  st = gpio_impl_.ConfigOut(A311D_GPIOE(2), 0);
  if (st != ZX_OK) {
    zxlogf(ERROR, "%s: ConfigOut failed: %d", __func__, st);
    return st;
  }

  // Configure the GPIO to be Output & set it to alternate
  // function 3 which puts in PWM_D mode. A73 cluster (Big)
  st = gpio_impl_.SetAltFunction(A311D_GPIOE(2), A311D_GPIOE_2_PWM_D_FN);
  if (st != ZX_OK) {
    zxlogf(ERROR, "%s: SetAltFunction failed: %d", __func__, st);
    return st;
  }

  // Add voltage regulator
  fidl::Arena<2048> allocator;
  fidl::VectorView<vreg::PwmVregMetadataEntry> pwm_vreg_entries(allocator, VREG_COUNT);

  pwm_vreg_entries[PWM_AO_D_VREG] = vreg::BuildMetadata(
      allocator, A311D_PWM_AO_D, kA311dPwmPeriodNs, kMinVoltageUv, kVoltageStepUv, kNumSteps);
  pwm_vreg_entries[PWM_A_VREG] = vreg::BuildMetadata(allocator, A311D_PWM_A, kA311dPwmPeriodNs,
                                                     kMinVoltageUv, kVoltageStepUv, kNumSteps);

  auto metadata = vreg::BuildMetadata(allocator, pwm_vreg_entries);
  fit::result encoded_metadata = fidl::Persist(metadata);
  if (!encoded_metadata.is_ok()) {
    zxlogf(ERROR, "%s: Could not build metadata %s\n", __func__,
           encoded_metadata.error_value().FormatDescription().c_str());
    return encoded_metadata.error_value().status();
  }

  fpbus::Node vreg_dev;
  vreg_dev.name() = "vreg";
  vreg_dev.vid() = PDEV_VID_GENERIC;
  vreg_dev.pid() = PDEV_PID_GENERIC;
  vreg_dev.did() = PDEV_DID_PWM_VREG;
  vreg_dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_VREG,
          .data = encoded_metadata.value(),
      }},
  };

  auto vreg_pwm_9_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules = {fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                                             bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
                     fdf::MakeAcceptBindRule(
                         bind_fuchsia::PWM_ID,
                         bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_AO_D)},
      .properties = {fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                                       bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
                     fdf::MakeProperty(
                         bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION,
                         bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION_CORE_POWER_LITTLE_CLUSTER)},
  }};

  auto vreg_pwm_0_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules = {fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                                             bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
                     fdf::MakeAcceptBindRule(
                         bind_fuchsia::PWM_ID,
                         bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_A)},
      .properties = {fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                                       bind_fuchsia_hardware_pwm::BIND_FIDL_PROTOCOL_DEVICE),
                     fdf::MakeProperty(
                         bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION,
                         bind_fuchsia_hardware_pwm::PWM_ID_FUNCTION_CORE_POWER_BIG_CLUSTER)},
  }};

  auto vreg_node_spec = fuchsia_driver_framework::CompositeNodeSpec{{
      .name = "vreg",
      .parents = {{vreg_pwm_9_node, vreg_pwm_0_node}},
  }};

  fidl::Arena<> fidl_arena;
  fdf::Arena vreg_arena('VREG');
  fdf::WireUnownedResult vreg_result =
      pbus_.buffer(vreg_arena)
          ->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, vreg_dev),
                                 fidl::ToWire(fidl_arena, vreg_node_spec));
  if (!vreg_result.ok() || vreg_result.value().is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec for vreg failed, error = %s",
           vreg_result.FormatDescription().c_str());
    return st;
  }

  fidl_arena.Reset();

  fdf::Arena arena('PWR_');
  auto result = pbus_.buffer(arena)->AddCompositeImplicitPbusFragment(
      fidl::ToWire(fidl_arena, power_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, power_impl_fragments,
                                               std::size(power_impl_fragments)),
      {});
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeImplicitPbusFragment Power(power_dev) request failed: %s",
           __func__, result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeImplicitPbusFragment Power(power_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  st = AddBigCore(pbus_);
  if (st != ZX_OK) {
    zxlogf(ERROR, "%s: CompositeDeviceAdd for power domain Big Arm Core failed, st = %d",
           __FUNCTION__, st);
    return st;
  }

  st = AddLittleCore(pbus_);
  if (st != ZX_OK) {
    zxlogf(ERROR, "%s: CompositeDeviceAdd for power domain Little Arm Core failed, st = %d",
           __FUNCTION__, st);
    return st;
  }

  // Add USB power delivery unit
  st = DdkAddCompositeNodeSpec(
      "fusb302",
      ddk::CompositeNodeSpec(kI2cRules, kI2cProperties).AddParentSpec(kGpioRules, kGpioProperties));
  if (st != ZX_OK) {
    zxlogf(ERROR, "%s: DdkAddCompositeNodeSpec for fusb302 failed, st = %d", __FUNCTION__, st);
    return st;
  }

  return ZX_OK;
}

}  // namespace vim3
