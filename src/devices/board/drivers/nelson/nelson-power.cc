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

#include <bind/fuchsia/amlogic/platform/s905d3/cpp/bind.h>
#include <bind/fuchsia/codec/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/power/sensor/cpp/bind.h>
#include <bind/fuchsia/ti/platform/cpp/bind.h>
#include <ddktl/device.h>

#include "nelson-gpios.h"
#include "nelson.h"
#include "src/devices/board/drivers/nelson/ti_ina231_mlb_bind.h"
#include "src/devices/board/drivers/nelson/ti_ina231_mlb_proto_bind.h"
#include "src/devices/board/drivers/nelson/ti_ina231_speakers_bind.h"
#include "src/devices/power/drivers/ti-ina231/ti-ina231-metadata.h"
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

constexpr device_metadata_t kMlbMetadata[] = {
    {
        .type = DEVICE_METADATA_PRIVATE,
        .data = &kMlbSensorMetadata,
        .length = sizeof(kMlbSensorMetadata),
    },
};

constexpr device_metadata_t kAudioMetadata[] = {
    {
        .type = DEVICE_METADATA_PRIVATE,
        .data = &kAudioSensorMetadata,
        .length = sizeof(kAudioSensorMetadata),
    },
};

constexpr zx_device_prop_t props[] = {
    {BIND_PLATFORM_DEV_VID, 0, PDEV_VID_TI},
    {BIND_PLATFORM_DEV_DID, 0, PDEV_DID_TI_INA231},
};

constexpr composite_device_desc_t mlb_power_sensor_dev = {
    .props = props,
    .props_count = std::size(props),
    .fragments = ti_ina231_mlb_fragments,
    .fragments_count = std::size(ti_ina231_mlb_fragments),
    .primary_fragment = "i2c",
    .spawn_colocated = false,
    .metadata_list = kMlbMetadata,
    .metadata_count = std::size(kMlbMetadata),
};

constexpr composite_device_desc_t speakers_power_sensor_dev = {
    .props = props,
    .props_count = std::size(props),
    .fragments = ti_ina231_speakers_fragments,
    .fragments_count = std::size(ti_ina231_speakers_fragments),
    .primary_fragment = "i2c",
    .spawn_colocated = false,
    .metadata_list = kAudioMetadata,
    .metadata_count = std::size(kAudioMetadata),
};

zx_status_t Nelson::PowerInit() {
  zx_status_t status = DdkAddComposite("ti-ina231-mlb", &mlb_power_sensor_dev);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s DdkAddComposite failed %d", __FUNCTION__, status);
    return status;
  }

  if ((status = DdkAddComposite("ti-ina231-speakers", &speakers_power_sensor_dev)) != ZX_OK) {
    zxlogf(ERROR, "%s DdkAddComposite failed %d", __FUNCTION__, status);
    return status;
  }
  const ddk::BindRule kGpioRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                              bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_10),
  };
  const ddk::BindRule kCodecRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_codec::BIND_FIDL_PROTOCOL_SERVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                              bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                              bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_TAS58XX),
  };
  const ddk::BindRule kPowerSensorRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_hardware_power_sensor::BIND_FIDL_PROTOCOL_DEVICE),
  };
  const device_bind_prop_t kGpioProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_GPIO_ALERT_PWR_L),
  };

  const device_bind_prop_t kCodecProperties[] = {
      ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_codec::BIND_FIDL_PROTOCOL_SERVICE),
  };

  const device_bind_prop_t kPowerSensorProperties[] = {
      ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_hardware_power_sensor::BIND_FIDL_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia::POWER_SENSOR_DOMAIN,
                        bind_fuchsia_amlogic_platform_s905d3::BIND_POWER_SENSOR_DOMAIN_AUDIO),
  };

  status = DdkAddCompositeNodeSpec("brownout_protection",
                                   ddk::CompositeNodeSpec(kCodecRules, kCodecProperties)
                                       .AddParentSpec(kGpioRules, kGpioProperties)
                                       .AddParentSpec(kPowerSensorRules, kPowerSensorProperties));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s AddCompositeSpec (brownout-protection)  %d", __FUNCTION__, status);
    return status;
  }

  return ZX_OK;
}

}  // namespace nelson
