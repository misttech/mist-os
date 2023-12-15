// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/board/drivers/astro/astro-adc.h"

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <soc/aml-s905d2/s905d2-hw.h>

#include "src/devices/board/drivers/astro/astro.h"
#include "src/devices/lib/fidl-metadata/adc.h"

namespace astro {

static const std::vector<fuchsia_hardware_platform_bus::Mmio> saradc_mmios{
    {{
        .base = S905D2_SARADC_BASE,
        .length = S905D2_SARADC_LENGTH,
    }},
    {{
        .base = S905D2_AOBUS_BASE,
        .length = S905D2_AOBUS_LENGTH,
    }},
};

static const std::vector<fuchsia_hardware_platform_bus::Irq> saradc_irqs{
    {{
        .irq = S905D2_SARADC_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

// ADC Channels to expose from generic ADC driver.
static const fidl_metadata::adc::Channel kAdcChannels[] = {
    DECL_ADC_CHANNEL(ASTRO_THERMISTOR_SOC),
    DECL_ADC_CHANNEL(ASTRO_THERMISTOR_WIFI),
    DECL_ADC_CHANNEL(ASTRO_THERMISTOR_DSP),
    DECL_ADC_CHANNEL(ASTRO_THERMISTOR_AMBIENT),
};

zx::result<> Astro::AdcInit() {
  fuchsia_hardware_platform_bus::Node dev;
  dev.name() = "adc";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_ADC;
  dev.mmio() = saradc_mmios;
  dev.irq() = saradc_irqs;

  auto metadata_bytes = fidl_metadata::adc::AdcChannelsToFidl(kAdcChannels);
  if (metadata_bytes.is_error()) {
    zxlogf(ERROR, "Failed to FIDL encode adc metadata: %s", metadata_bytes.status_string());
    return metadata_bytes.take_error();
  }
  dev.metadata() = std::vector<fuchsia_hardware_platform_bus::Metadata>{
      {{
          .type = DEVICE_METADATA_ADC,
          .data = metadata_bytes.value(),
      }},
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('ADC_');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, dev));
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

}  // namespace astro
