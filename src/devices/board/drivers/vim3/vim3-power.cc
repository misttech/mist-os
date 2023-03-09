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

#include <string>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/pwm/cpp/bind.h>
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

const ddk::BindRule kPwmAODRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_pwm::BIND_PROTOCOL_PWM),
    ddk::MakeAcceptBindRule(bind_fuchsia::PWM_ID, static_cast<uint32_t>(A311D_PWM_AO_D))};

const device_bind_prop_t kPwmAODProperties[] = {
    ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_pwm::BIND_PROTOCOL_PWM),
    ddk::MakeProperty(bind_fuchsia::PWM_ID, static_cast<uint32_t>(A311D_PWM_AO_D))};

const ddk::BindRule kPwmARules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_pwm::BIND_PROTOCOL_PWM),
    ddk::MakeAcceptBindRule(bind_fuchsia::PWM_ID, static_cast<uint32_t>(A311D_PWM_A))};

const device_bind_prop_t kPwmAProperties[] = {
    ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_pwm::BIND_PROTOCOL_PWM),
    ddk::MakeProperty(bind_fuchsia::PWM_ID, static_cast<uint32_t>(A311D_PWM_A))};

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
    BI_ABORT_IF(NE, BIND_PROTOCOL, ZX_PROTOCOL_VREG),
    BI_MATCH_IF(EQ, BIND_PWM_ID, A311D_PWM_AO_D),
};

constexpr zx_bind_inst_t vreg_pwm_a_match[] = {
    BI_ABORT_IF(NE, BIND_PROTOCOL, ZX_PROTOCOL_VREG),
    BI_MATCH_IF(EQ, BIND_PWM_ID, A311D_PWM_A),
};

constexpr device_fragment_part_t vreg_pwm_ao_d_fragment[] = {
    {std::size(vreg_pwm_ao_d_match), vreg_pwm_ao_d_match},
};

constexpr device_fragment_part_t vreg_pwm_a_fragment[] = {
    {std::size(vreg_pwm_a_match), vreg_pwm_a_match},
};

constexpr device_fragment_t power_impl_fragments[] = {
    {"vreg-pwm-ao-d", std::size(vreg_pwm_ao_d_fragment), vreg_pwm_ao_d_fragment},
    {"vreg-pwm-a", std::size(vreg_pwm_a_fragment), vreg_pwm_a_fragment},
};

constexpr zx_bind_inst_t power_impl_driver_match[] = {
    BI_MATCH_IF(EQ, BIND_PROTOCOL, ZX_PROTOCOL_POWER_IMPL),
};

constexpr device_fragment_part_t power_impl_fragment[] = {
    {std::size(power_impl_driver_match), power_impl_driver_match},
};

static const fpbus::Node power_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "aml-power-impl-composite";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_AMLOGIC_A311D;
  dev.did() = PDEV_DID_AMLOGIC_POWER;
  return dev;
}();

zx_device_prop_t power_domain_arm_core_props[] = {
    {BIND_POWER_DOMAIN_COMPOSITE, 0, PDEV_DID_POWER_DOMAIN_COMPOSITE},
};

constexpr device_fragment_t power_domain_arm_core_fragments[] = {
    {"power-impl", std::size(power_impl_fragment), power_impl_fragment},
};

constexpr power_domain_t big_domain[] = {
    {static_cast<uint32_t>(A311dPowerDomains::kArmCoreBig)},
};

constexpr device_metadata_t power_domain_big_core_metadata[] = {
    {
        .type = DEVICE_METADATA_POWER_DOMAINS,
        .data = &big_domain,
        .length = sizeof(big_domain),
    },
};

constexpr composite_device_desc_t power_domain_big_core_desc = {
    .props = power_domain_arm_core_props,
    .props_count = std::size(power_domain_arm_core_props),
    .fragments = power_domain_arm_core_fragments,
    .fragments_count = std::size(power_domain_arm_core_fragments),
    .primary_fragment = "power-impl",
    .spawn_colocated = true,
    .metadata_list = power_domain_big_core_metadata,
    .metadata_count = std::size(power_domain_big_core_metadata),
};

constexpr power_domain_t little_domain[] = {
    {static_cast<uint32_t>(A311dPowerDomains::kArmCoreLittle)},
};

constexpr device_metadata_t power_domain_little_core_metadata[] = {
    {
        .type = DEVICE_METADATA_POWER_DOMAINS,
        .data = &little_domain,
        .length = sizeof(little_domain),
    },
};

constexpr composite_device_desc_t power_domain_little_core_desc = {
    .props = power_domain_arm_core_props,
    .props_count = std::size(power_domain_arm_core_props),
    .fragments = power_domain_arm_core_fragments,
    .fragments_count = std::size(power_domain_arm_core_fragments),
    .primary_fragment = "power-impl",
    .spawn_colocated = true,
    .metadata_list = power_domain_little_core_metadata,
    .metadata_count = std::size(power_domain_little_core_metadata),
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

  std::vector<uint8_t>& encoded_metadata_bytes = encoded_metadata.value();
  static const device_metadata_t vreg_metadata[] = {
      {
          .type = DEVICE_METADATA_VREG,
          .data = encoded_metadata_bytes.data(),
          .length = encoded_metadata_bytes.size(),
      },
  };

  st = DdkAddCompositeNodeSpec("vreg", ddk::CompositeNodeSpec(kPwmAODRules, kPwmAODProperties)
                                           .AddParentSpec(kPwmARules, kPwmAProperties)
                                           .set_metadata(vreg_metadata));
  if (st != ZX_OK) {
    zxlogf(ERROR, "DdkAddCompositeNodeSpec for vreg failed, st = %d", st);
    return st;
  }

  fidl::Arena<> fidl_arena;
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

  st = DdkAddComposite("pd-big-core", &power_domain_big_core_desc);
  if (st != ZX_OK) {
    zxlogf(ERROR, "%s: CompositeDeviceAdd for power domain Big Arm Core failed, st = %d",
           __FUNCTION__, st);
    return st;
  }

  st = DdkAddComposite("pd-little-core", &power_domain_little_core_desc);
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
