// Copyright 2020 The Fuchsia Authors. All rights reserved.
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

#include <bind/fuchsia/amlogic/platform/s905d3/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/google/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/audio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/i2c/cpp/bind.h>
#include <bind/fuchsia/hardware/power/sensor/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <bind/fuchsia/power/cpp/bind.h>
#include <bind/fuchsia/ti/platform/cpp/bind.h>
#include <ddktl/device.h>

#include "nelson-gpios.h"
#include "nelson.h"
#include "src/devices/power/drivers/ti-ina231/ti-ina231-metadata.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

// These values are specific to Nelson, and are only used within this board driver.
enum : uint32_t {
  kPowerSensorDomainMlb = 0,
  kPowerSensorDomainAudio = 1,
};

constexpr power_sensor::Ina231Metadata kMlbSensorMetadata = {
    .mode = power_sensor::Ina231Metadata::kModeShuntAndBusContinuous,
    .shunt_voltage_conversion_time = power_sensor::Ina231Metadata::kConversionTime332us,
    .bus_voltage_conversion_time = power_sensor::Ina231Metadata::kConversionTime332us,
    .averages = power_sensor::Ina231Metadata::kAverages1024,
    .shunt_resistance_microohm = 10'000,
    .bus_voltage_limit_microvolt = 0,
    .alert = power_sensor::Ina231Metadata::kAlertNone,
    .power_sensor_domain = kPowerSensorDomainMlb,
};

static const std::vector<fpbus::Metadata> kMlbMetadata{
    {{
        .id = std::to_string(DEVICE_METADATA_PRIVATE),
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&kMlbSensorMetadata),
            reinterpret_cast<const uint8_t*>(&kMlbSensorMetadata) + sizeof(kMlbSensorMetadata)),
    }},
};

constexpr power_sensor::Ina231Metadata kAudioSensorMetadata = {
    .mode = power_sensor::Ina231Metadata::kModeShuntAndBusContinuous,
    .shunt_voltage_conversion_time = power_sensor::Ina231Metadata::kConversionTime332us,
    .bus_voltage_conversion_time = power_sensor::Ina231Metadata::kConversionTime332us,
    .averages = power_sensor::Ina231Metadata::kAverages1024,
    .shunt_resistance_microohm = 10'000,
    .bus_voltage_limit_microvolt = 11'000'000,
    .alert = power_sensor::Ina231Metadata::kAlertBusUnderVoltage,
    .power_sensor_domain = kPowerSensorDomainAudio,
};

static const std::vector<fpbus::Metadata> kSpeakersMetadata{
    {{
        .id = std::to_string(DEVICE_METADATA_PRIVATE),
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&kAudioSensorMetadata),
            reinterpret_cast<const uint8_t*>(&kAudioSensorMetadata) + sizeof(kAudioSensorMetadata)),
    }},
};

zx_status_t AddMlbComposite(fdf::WireSyncClient<fpbus::PlatformBus>& pbus,
                            fidl::AnyArena& fidl_arena, fdf::Arena& arena) {
  fpbus::Node mlb_dev;
  mlb_dev.name() = "ti-ina231-mlb";
  mlb_dev.vid() = bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI;
  mlb_dev.pid() = bind_fuchsia_google_platform::BIND_PLATFORM_DEV_PID_NELSON;
  mlb_dev.did() = bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_INA231_MLB;
  mlb_dev.metadata() = kMlbMetadata;

  const auto kI2cRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                              bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_3),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                              bind_fuchsia_ti_platform::BIND_I2C_ADDRESS_INA231_MLB),
  };
  const auto kI2cProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                        bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS,
                        bind_fuchsia_ti_platform::BIND_I2C_ADDRESS_INA231_MLB),
  };

  const std::vector<fdf::ParentSpec> kParents{{kI2cRules, kI2cProperties}};
  auto result = pbus.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, mlb_dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "ti_ina231_mlb", .parents = kParents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddCompositeNodeSpec request failed to platform bus: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add ti-ina231-mlb composite to platform device: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

zx_status_t AddSpeakerComposite(fdf::WireSyncClient<fpbus::PlatformBus>& pbus,
                                fidl::AnyArena& fidl_arena, fdf::Arena& arena) {
  fpbus::Node speakers_dev;
  speakers_dev.name() = "ti-ina231-speakers";
  speakers_dev.vid() = bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI;
  speakers_dev.pid() = bind_fuchsia_google_platform::BIND_PLATFORM_DEV_PID_NELSON;
  speakers_dev.did() = bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_INA231_SPEAKERS;
  speakers_dev.metadata() = kSpeakersMetadata;

  const auto kI2cRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                              bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_3),
      fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                              bind_fuchsia_ti_platform::BIND_I2C_ADDRESS_INA231_SPEAKERS),
  };
  const auto kI2cProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                        bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS,
                        bind_fuchsia_ti_platform::BIND_I2C_ADDRESS_INA231_SPEAKERS),
  };

  const std::vector<fdf::ParentSpec> kParents{{kI2cRules, kI2cProperties}};
  auto result = pbus.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, speakers_dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "ti_ina231_speakers", .parents = kParents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddCompositeNodeSpec request failed to platform bus: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add ti-ina231-speakers composite to platform device: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

zx_status_t Nelson::PowerInit() {
  fidl::Arena<> fidl_arena;
  fdf::Arena mlb_arena('TMLB');
  zx_status_t status = AddMlbComposite(pbus_, fidl_arena, mlb_arena);
  if (status != ZX_OK) {
    return status;
  }

  fdf::Arena speakers_arena('SPKR');
  status = AddSpeakerComposite(pbus_, fidl_arena, speakers_arena);
  if (status != ZX_OK) {
    return status;
  }

  return ZX_OK;
}

zx_status_t Nelson::BrownoutProtectionInit() {
  // Pull up externally.
  gpio_init_steps_.push_back(GpioPull(GPIO_ALERT_PWR_L, fuchsia_hardware_pin::Pull::kNone));

  const ddk::BindRule kGpioRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_10),
  };

  const ddk::BindRule kCodecRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia_hardware_audio::CODECSERVICE,
                              bind_fuchsia_hardware_audio::CODECSERVICE_ZIRCONTRANSPORT),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                              bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                              bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_TAS58XX),
  };

  const ddk::BindRule kPowerSensorRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia_hardware_power_sensor::SERVICE,
                              bind_fuchsia_hardware_power_sensor::SERVICE_ZIRCONTRANSPORT),
  };

  const ddk::BindRule kGpioInitRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };

  const device_bind_prop_t kGpioProperties[] = {
      ddk::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                        bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
      ddk::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_GPIO_ALERT_PWR_L),
  };

  const device_bind_prop_t kCodecProperties[] = {
      ddk::MakeProperty(bind_fuchsia_hardware_audio::CODECSERVICE,
                        bind_fuchsia_hardware_audio::CODECSERVICE_ZIRCONTRANSPORT),
  };

  const device_bind_prop_t kPowerSensorProperties[] = {
      ddk::MakeProperty(bind_fuchsia_hardware_power_sensor::SERVICE,
                        bind_fuchsia_hardware_power_sensor::SERVICE_ZIRCONTRANSPORT),
      ddk::MakeProperty(bind_fuchsia::POWER_SENSOR_DOMAIN,
                        bind_fuchsia_amlogic_platform_s905d3::BIND_POWER_SENSOR_DOMAIN_AUDIO),
  };

  const device_bind_prop_t kGpioInitProperties[] = {
      ddk::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
  };

  zx_status_t status = DdkAddCompositeNodeSpec(
      "brownout_protection", ddk::CompositeNodeSpec(kCodecRules, kCodecProperties)
                                 .AddParentSpec(kGpioRules, kGpioProperties)
                                 .AddParentSpec(kPowerSensorRules, kPowerSensorProperties)
                                 .AddParentSpec(kGpioInitRules, kGpioInitProperties));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s AddCompositeSpec (brownout-protection)  %d", __FUNCTION__, status);
    return status;
  }

  return ZX_OK;
}

}  // namespace nelson
