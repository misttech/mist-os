// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/board/drivers/sherlock/sherlock-adc.h"

#include <fidl/fuchsia.hardware.adcimpl/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <soc/aml-t931/t931-hw.h>

#include "src/devices/board/drivers/sherlock/sherlock.h"
#include "src/devices/lib/fidl-metadata/adc.h"

namespace sherlock {

static const std::vector<fuchsia_hardware_platform_bus::Mmio> saradc_mmios{
    {{
        .base = T931_SARADC_BASE,
        .length = T931_SARADC_LENGTH,
    }},
    {{
        .base = T931_AOBUS_BASE,
        .length = T931_AOBUS_LENGTH,
    }},
};

static const std::vector<fuchsia_hardware_platform_bus::Irq> saradc_irqs{
    {{
        .irq = T931_SARADC_IRQ,
        .mode = fuchsia_hardware_platform_bus::ZirconInterruptMode::kEdgeHigh,
    }},
};

// ADC Channels to expose from generic ADC driver.
static const fidl_metadata::adc::Channel kAdcChannels[] = {
    DECL_ADC_CHANNEL(0),
    DECL_ADC_CHANNEL(SHERLOCK_THERMISTOR_BASE),
    DECL_ADC_CHANNEL(SHERLOCK_THERMISTOR_AUDIO),
    DECL_ADC_CHANNEL(SHERLOCK_THERMISTOR_AMBIENT),
};

zx::result<> Sherlock::AdcInit() {
  fuchsia_hardware_platform_bus::Node node;
  node.name() = "adc";
  node.vid() = PDEV_VID_AMLOGIC;
  node.pid() = PDEV_PID_GENERIC;
  node.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_ADC;
  node.mmio() = saradc_mmios;
  node.irq() = saradc_irqs;

  auto metadata_bytes = fidl_metadata::adc::AdcChannelsToFidl(kAdcChannels);
  if (metadata_bytes.is_error()) {
    zxlogf(ERROR, "Failed to FIDL encode adc metadata: %s", metadata_bytes.status_string());
    return metadata_bytes.take_error();
  }
  node.metadata() = std::vector<fuchsia_hardware_platform_bus::Metadata>{
      {{
          .id = fuchsia_hardware_adcimpl::Metadata::kSerializableName,
          .data = metadata_bytes.value(),
      }},
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('ADC_');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, node));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd (adc) request failed: %s", result.FormatDescription().data());
    return result->take_error();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd (adc) failed: %s", zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  return zx::ok();
}

}  // namespace sherlock
