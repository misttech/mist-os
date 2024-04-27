// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.hardware.vreg/cpp/fidl.h>
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
#include <bind/fuchsia/regulator/cpp/bind.h>
#include <ddktl/device.h>
#include <soc/aml-a311d/a311d-power.h>
#include <soc/aml-a311d/a311d-pwm.h>
#include <soc/aml-common/aml-power.h>

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
    fdf::MakeAcceptBindRule(bind_fuchsia_regulator::NAME,
                            bind_fuchsia_amlogic_platform_a311d::NAME_PWM_VREG_LITTLE)};
const std::vector<fuchsia_driver_framework::NodeProperty> kVregPwmAoDProperties = {
    fdf::MakeProperty(bind_fuchsia_hardware_vreg::SERVICE,
                      bind_fuchsia_hardware_vreg::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_regulator::NAME,
                      bind_fuchsia_amlogic_platform_a311d::NAME_PWM_VREG_LITTLE)};

const std::vector<fuchsia_driver_framework::BindRule> kVregPwmARules = {
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_vreg::SERVICE,
                            bind_fuchsia_hardware_vreg::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia_regulator::NAME,
                            bind_fuchsia_amlogic_platform_a311d::NAME_PWM_VREG_BIG)};
const std::vector<fuchsia_driver_framework::NodeProperty> kVregPwmAProperties = {
    fdf::MakeProperty(bind_fuchsia_hardware_vreg::SERVICE,
                      bind_fuchsia_hardware_vreg::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_regulator::NAME,
                      bind_fuchsia_amlogic_platform_a311d::NAME_PWM_VREG_BIG)};

static fpbus::Node power_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "aml-power-impl-composite";
  dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_A311D;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_POWER;
  return dev;
}();

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

zx_status_t AddVreg(std::string name, uint32_t pwm_id,
                    fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  auto gpio_init_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules = {fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                             bind_fuchsia_gpio::BIND_INIT_STEP_GPIO)},
      .properties = {fdf::MakeProperty(bind_fuchsia::INIT_STEP,
                                       bind_fuchsia_gpio::BIND_INIT_STEP_GPIO)},
  }};

  fidl::Arena<> fidl_arena;
  fuchsia_hardware_vreg::VregMetadata vreg_metadata = {};
  vreg_metadata.name() = name;
  vreg_metadata.min_voltage_uv() = kMinVoltageUv;
  vreg_metadata.voltage_step_uv() = kVoltageStepUv;
  vreg_metadata.num_steps() = kNumSteps;

  fit::result encoded_metadata = fidl::Persist(vreg_metadata);
  if (!encoded_metadata.is_ok()) {
    zxlogf(ERROR, "%s: Could not build metadata %s\n", __func__,
           encoded_metadata.error_value().FormatDescription().c_str());
    return encoded_metadata.error_value().status();
  }

  char dev_name[20];
  snprintf(dev_name, sizeof(dev_name), "vreg-%d", pwm_id);
  fpbus::Node vreg_dev;
  vreg_dev.name() = dev_name;
  vreg_dev.vid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC;
  vreg_dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  vreg_dev.did() = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_PWM_VREG;
  vreg_dev.instance_id() = pwm_id;
  vreg_dev.metadata() = std::vector<fpbus::Metadata>{{{
      .type = DEVICE_METADATA_VREG,
      .data = encoded_metadata.value(),
  }}};

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
    zxlogf(ERROR, "AddCompositeNodeSpec for %sfailed, error = %s", dev_name,
           vreg_result.FormatDescription().c_str());
    return vreg_result.ok() ? vreg_result->error_value() : vreg_result.status();
  }

  return ZX_OK;
}

}  // namespace

zx_status_t Vim3::PowerInit() {
  gpio_init_steps_.push_back({A311D_GPIOE(1), GpioConfigOut(0)});

  // Configure the GPIO to be Output & set it to alternate
  // function 3 which puts in PWM_D mode. A53 cluster (Little)
  gpio_init_steps_.push_back({A311D_GPIOE(1), GpioSetAltFunction(A311D_GPIOE_1_PWM_D_FN)});

  gpio_init_steps_.push_back({A311D_GPIOE(2), GpioConfigOut(0)});

  // Configure the GPIO to be Output & set it to alternate
  // function 3 which puts in PWM_D mode. A73 cluster (Big)
  gpio_init_steps_.push_back({A311D_GPIOE(2), GpioSetAltFunction(A311D_GPIOE_2_PWM_D_FN)});

  // Configure PWM for A53 cluster (Little).
  pwm_channel_configs_.push_back({{.id = bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_AO_D,
                                   .skip_init = false,
                                   .period_ns = kA311dPwmPeriodNs}});

  // Configure PWM for A73 cluster (Big).
  pwm_channel_configs_.push_back({{.id = bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_A,
                                   .skip_init = false,
                                   .period_ns = kA311dPwmPeriodNs}});

  // Add PWM_AO_D voltage regulator
  auto status = AddVreg(bind_fuchsia_amlogic_platform_a311d::NAME_PWM_VREG_LITTLE,
                        bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_AO_D, pbus_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "AddVreg for ID %d failed. status = %s",
           bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_AO_D, zx_status_get_string(status));
    return status;
  }

  // Add PWM_A voltage regulator
  status = AddVreg(bind_fuchsia_amlogic_platform_a311d::NAME_PWM_VREG_BIG,
                   bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_A, pbus_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "AddVreg for ID %d failed. status = %s",
           bind_fuchsia_amlogic_platform_a311d::BIND_PWM_ID_PWM_A, zx_status_get_string(status));
    return status;
  }

  {
    fuchsia_hardware_power::DomainMetadata metadata = {
        {.domains = {{
             {{.id = {bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE}}},
             {{.id = {bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG}}},
         }}}};

    const fit::result encoded_metadata = fidl::Persist(metadata);
    if (!encoded_metadata.is_ok()) {
      zxlogf(ERROR, "Failed to encode power domain metadata: %s",
             encoded_metadata.error_value().FormatDescription().c_str());
      return encoded_metadata.error_value().status();
    }

    const std::vector<fpbus::Metadata> power_metadata{
        {{
            .type = DEVICE_METADATA_POWER_DOMAINS,
            .data = encoded_metadata.value(),
        }},
    };

    power_dev.metadata() = power_metadata;
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
