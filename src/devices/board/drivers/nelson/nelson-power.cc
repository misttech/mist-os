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
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"
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

static const std::vector<fpbus::Metadata> kMlbMetadata{
    {{
        .type = DEVICE_METADATA_PRIVATE,
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
        .type = DEVICE_METADATA_PRIVATE,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&kAudioSensorMetadata),
            reinterpret_cast<const uint8_t*>(&kAudioSensorMetadata) + sizeof(kAudioSensorMetadata)),
    }},
};

zx_status_t Nelson::PowerInit() {
  fpbus::Node mlb_dev;
  mlb_dev.name() = "ti-ina231-mlb";
  mlb_dev.vid() = PDEV_VID_TI;
  mlb_dev.pid() = PDEV_PID_NELSON;
  mlb_dev.did() = PDEV_DID_TI_INA231_MLB;
  mlb_dev.metadata() = kMlbMetadata;

  fidl::Arena<> fidl_arena;
  fdf::Arena mlb_arena('TMLB');
  fdf::WireUnownedResult result = pbus_.buffer(mlb_arena)->AddComposite(
      fidl::ToWire(fidl_arena, mlb_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, ti_ina231_mlb_fragments,
                                               std::size(ti_ina231_mlb_fragments)),
      "i2c");
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddComposite request to platform bus: %s",
           result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add ti-ina231-mlb composite to platform device: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  fpbus::Node speakers_dev;
  speakers_dev.name() = "ti-ina231-speakers";
  speakers_dev.vid() = PDEV_VID_TI;
  speakers_dev.pid() = PDEV_PID_NELSON;
  speakers_dev.did() = PDEV_DID_TI_INA231_SPEAKERS;
  speakers_dev.metadata() = kSpeakersMetadata;

  fdf::Arena speakers_arena('SPKR');
  result = pbus_.buffer(speakers_arena)
               ->AddComposite(fidl::ToWire(fidl_arena, speakers_dev),
                              platform_bus_composite::MakeFidlFragment(
                                  fidl_arena, ti_ina231_speakers_fragments,
                                  std::size(ti_ina231_speakers_fragments)),
                              "i2c");
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddComposite request to platform bus: %s",
           result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add ti-ina231-speakers composite to platform device: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
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

  zx_status_t status = DdkAddCompositeNodeSpec(
      "brownout_protection", ddk::CompositeNodeSpec(kCodecRules, kCodecProperties)
                                 .AddParentSpec(kGpioRules, kGpioProperties)
                                 .AddParentSpec(kPowerSensorRules, kPowerSensorProperties));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s AddCompositeSpec (brownout-protection)  %d", __FUNCTION__, status);
    return status;
  }

  return ZX_OK;
}

}  // namespace nelson
