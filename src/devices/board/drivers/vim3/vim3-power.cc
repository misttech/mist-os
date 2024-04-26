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
#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/i2c/cpp/bind.h>
#include <bind/fuchsia/hardware/pwm/cpp/bind.h>
#include <bind/fuchsia/hardware/vreg/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <bind/fuchsia/power/cpp/bind.h>
#include <bind/fuchsia/pwm/cpp/bind.h>
#include <ddk/metadata/power.h>
#include <ddktl/device.h>
#include <soc/aml-a311d/a311d-power.h>
#include <soc/aml-a311d/a311d-pwm.h>
#include <soc/aml-common/aml-power.h>

#include "src/devices/lib/metadata/llcpp/vreg.h"
#include "vim3-gpios.h"
#include "vim3.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

constexpr voltage_pwm_period_ns_t kA311dPwmPeriodNs = 1250;

const uint32_t kVoltageStepUv = 1'000;
static_assert((kMaxVoltageUv - kMinVoltageUv) % kVoltageStepUv == 0,
              "Voltage step must be a factor of (kMaxVoltageUv - kMinVoltageUv)\n");
const uint32_t kNumSteps = (kMaxVoltageUv - kMinVoltageUv) / kVoltageStepUv + 1;

const std::vector<fuchsia_driver_framework::BindRule> kVregPwmAoDRules = {
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_vreg::SERVICE,
                            bind_fuchsia_hardware_vreg::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia::PWM_ID, static_cast<uint32_t>(A311D_PWM_AO_D))};
const std::vector<fuchsia_driver_framework::NodeProperty> kVregPwmAoDProperties = {
    fdf::MakeProperty(bind_fuchsia_hardware_vreg::SERVICE,
                      bind_fuchsia_hardware_vreg::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_amlogic_platform::PWM_ID,
                      bind_fuchsia_amlogic_platform::PWM_ID_AO_D)};

const std::vector<fuchsia_driver_framework::BindRule> kVregPwmARules = {
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_vreg::SERVICE,
                            bind_fuchsia_hardware_vreg::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia::PWM_ID, static_cast<uint32_t>(A311D_PWM_A))};
const std::vector<fuchsia_driver_framework::NodeProperty> kVregPwmAProperties = {
    fdf::MakeProperty(bind_fuchsia_hardware_vreg::SERVICE,
                      bind_fuchsia_hardware_vreg::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_amlogic_platform::PWM_ID,
                      bind_fuchsia_amlogic_platform::PWM_ID_A)};

static const fpbus::Node power_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "aml-power-impl-composite";
  dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_A311D;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_POWER;
  return dev;
}();

constexpr power_domain_t big_domain[] = {
    {bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG},
};

constexpr power_domain_t little_domain[] = {
    {bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE},
};

const ddk::BindRule kI2cRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                            bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_A0_0),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS, 0x22u)};

const device_bind_prop_t kI2cProperties[] = {
    ddk::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                      bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
};

const ddk::BindRule kGpioRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                            bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(VIM3_FUSB302_INT))};

const device_bind_prop_t kGpioProperties[] = {
    ddk::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                      bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    ddk::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_USB_POWER_DELIVERY)};

const std::vector<fdf::BindRule> kPowerArmcoreRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_power::BIND_PROTOCOL_IMPL),
};

const std::vector<fdf::NodeProperty> kPowerArmcoreProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_power::BIND_PROTOCOL_IMPL),
};

const std::vector<fdf::ParentSpec> kPowerArmcoreParents = {
    {
        kPowerArmcoreRules,
        kPowerArmcoreProperties,
    },
};

zx_status_t AddBigCore(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  fpbus::Node big_core_dev;
  big_core_dev.name() = "pd-big-core";
  big_core_dev.vid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC;
  big_core_dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  big_core_dev.did() = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_POWER_CORE;
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
  fdf::WireUnownedResult big_core_result =
      pbus.buffer(big_core_arena)
          ->AddCompositeNodeSpec(
              fidl::ToWire(fidl_arena, big_core_dev),
              fidl::ToWire(fidl_arena, fdf::CompositeNodeSpec{{.name = "pd_big_core",
                                                               .parents = kPowerArmcoreParents}}));
  if (!big_core_result.ok() || big_core_result.value().is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec for big core failed, error = %s",
           big_core_result.FormatDescription().c_str());
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

zx_status_t AddLittleCore(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  fpbus::Node little_core_dev;
  little_core_dev.name() = "pd-little-core";
  little_core_dev.vid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC;
  little_core_dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  little_core_dev.did() = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_POWER_CORE;
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
  fdf::WireUnownedResult little_core_result =
      pbus.buffer(little_core_arena)
          ->AddCompositeNodeSpec(
              fidl::ToWire(fidl_arena, little_core_dev),
              fidl::ToWire(fidl_arena, fdf::CompositeNodeSpec{{.name = "pd_little_core",
                                                               .parents = kPowerArmcoreParents}}));
  if (!little_core_result.ok() || little_core_result.value().is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec for little core failed, error = %s",
           little_core_result.FormatDescription().c_str());
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

zx_status_t AddVreg(uint32_t pwm_id,
                    fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  auto gpio_init_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules = {fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                             bind_fuchsia_gpio::BIND_INIT_STEP_GPIO)},
      .properties = {fdf::MakeProperty(bind_fuchsia::INIT_STEP,
                                       bind_fuchsia_gpio::BIND_INIT_STEP_GPIO)},
  }};

  fidl::Arena<> fidl_arena;
  vreg::PwmVregMetadata pwm_vreg_metadata = vreg::BuildMetadata(
      fidl_arena, pwm_id, kA311dPwmPeriodNs, kMinVoltageUv, kVoltageStepUv, kNumSteps);

  fit::result encoded_metadata = fidl::Persist(pwm_vreg_metadata);
  if (!encoded_metadata.is_ok()) {
    zxlogf(ERROR, "%s: Could not build metadata %s\n", __func__,
           encoded_metadata.error_value().FormatDescription().c_str());
    return encoded_metadata.error_value().status();
  }

  char name[20];
  snprintf(name, sizeof(name), "vreg-%d", pwm_id);
  fpbus::Node vreg_dev;
  vreg_dev.name() = name;
  vreg_dev.vid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC;
  vreg_dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  vreg_dev.did() = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_PWM_VREG;
  vreg_dev.instance_id() = pwm_id;
  vreg_dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_VREG,
          .data = encoded_metadata.value(),
      }},
  };

  auto vreg_pwm_node = fdf::ParentSpec{{
      .bind_rules = {fdf::MakeAcceptBindRule(bind_fuchsia_hardware_pwm::SERVICE,
                                             bind_fuchsia_hardware_pwm::SERVICE_ZIRCONTRANSPORT),
                     fdf::MakeAcceptBindRule(bind_fuchsia::PWM_ID, pwm_id)},
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia_hardware_pwm::SERVICE,
                                bind_fuchsia_hardware_pwm::SERVICE_ZIRCONTRANSPORT),
          },
  }};

  auto vreg_node_spec = fuchsia_driver_framework::CompositeNodeSpec{{
      .name = name,
      .parents = {{vreg_pwm_node, gpio_init_node}},
  }};

  fdf::Arena fdf_arena('VREG');
  fdf::WireUnownedResult vreg_result = pbus.buffer(fdf_arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, vreg_dev), fidl::ToWire(fidl_arena, vreg_node_spec));
  if (!vreg_result.ok() || vreg_result.value().is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec for %sfailed, error = %s", name,
           vreg_result.FormatDescription().c_str());
    return vreg_result.ok() ? vreg_result->error_value() : vreg_result.status();
  }

  return ZX_OK;
}

}  // namespace

zx_status_t Vim3::PowerInit() {
  gpio_init_steps_.push_back({A311D_GPIOE(1), GpioConfigOut(0)});

  // Configure the GPIO to be Output & set it to alternate
  // function 3 which puts in PWM_D mode. A53 cluster (Small)
  gpio_init_steps_.push_back({A311D_GPIOE(1), GpioSetAltFunction(A311D_GPIOE_1_PWM_D_FN)});

  gpio_init_steps_.push_back({A311D_GPIOE(2), GpioConfigOut(0)});

  // Configure the GPIO to be Output & set it to alternate
  // function 3 which puts in PWM_D mode. A73 cluster (Big)
  gpio_init_steps_.push_back({A311D_GPIOE(2), GpioSetAltFunction(A311D_GPIOE_2_PWM_D_FN)});

  // Add PWM_AO_D voltage regulator
  auto status = AddVreg(bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_AO_D, pbus_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "AddVreg for ID %d failed. status = %s",
           bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_AO_D, zx_status_get_string(status));
    return status;
  }

  // Add PWM_A voltage regulator
  status = AddVreg(bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_A, pbus_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "AddVreg for ID %d failed. status = %s",
           bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_A, zx_status_get_string(status));
    return status;
  }

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('PWR_');
  const std::vector<fdf::ParentSpec> kAmlPowerImplComposite = {
      fdf::ParentSpec{{kVregPwmAoDRules, kVregPwmAoDProperties}},
      fdf::ParentSpec{{kVregPwmARules, kVregPwmAProperties}}};
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, power_dev),
      fidl::ToWire(fidl_arena, fdf::CompositeNodeSpec{{.name = "aml-power-impl-composite",
                                                       .parents = kAmlPowerImplComposite}}));
  if (!result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Power(power_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Power(power_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  status = AddBigCore(pbus_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: CompositeDeviceAdd for power domain Big Arm Core failed, status = %d",
           __FUNCTION__, status);
    return status;
  }

  status = AddLittleCore(pbus_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: CompositeDeviceAdd for power domain Little Arm Core failed, status = %d",
           __FUNCTION__, status);
    return status;
  }

  // Add USB power delivery unit
  status = DdkAddCompositeNodeSpec(
      "fusb302",
      ddk::CompositeNodeSpec(kI2cRules, kI2cProperties).AddParentSpec(kGpioRules, kGpioProperties));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: DdkAddCompositeNodeSpec for fusb302 failed, status = %d", __FUNCTION__,
           status);
    return status;
  }

  return ZX_OK;
}

}  // namespace vim3
