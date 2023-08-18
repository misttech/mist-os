// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.buttons/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <ddk/metadata/buttons.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "src/devices/board/drivers/vim3/vim3.h"

namespace vim3 {

static const std::vector<fuchsia_hardware_platform_bus::Mmio> saradc_mmios{
    {{
        .base = A311D_SARADC_BASE,
        .length = A311D_SARADC_LENGTH,
    }},
    {{
        .base = A311D_AOBUS_BASE,
        .length = A311D_AOBUS_LENGTH,
    }},
};

static const std::vector<fuchsia_hardware_platform_bus::Irq> saradc_irqs{
    {{
        .irq = A311D_SARADC_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

zx::result<> Vim3::ButtonsInit() {
  // Function Key
  fuchsia_hardware_platform_bus::Node adc_buttons;
  adc_buttons.name() = "buttons";
  adc_buttons.vid() = PDEV_VID_GENERIC;
  adc_buttons.pid() = PDEV_PID_GENERIC;
  adc_buttons.did() = PDEV_DID_ADC_BUTTONS;
  adc_buttons.mmio() = saradc_mmios;
  adc_buttons.irq() = saradc_irqs;

  auto func_types = std::vector<fuchsia_input_report::ConsumerControlButton>{
      fuchsia_input_report::ConsumerControlButton::kFunction};
  auto func_adc_config =
      fuchsia_buttons::AdcButtonConfig().channel_idx(2).release_threshold(1'000).press_threshold(
          100);
  auto func_config = fuchsia_buttons::ButtonConfig::WithAdc(std::move(func_adc_config));
  auto func_button =
      fuchsia_buttons::Button().types(std::move(func_types)).button_config(std::move(func_config));
  std::vector<fuchsia_buttons::Button> buttons;
  buttons.emplace_back(std::move(func_button));

  auto metadata = fuchsia_buttons::Metadata().polling_rate_usec(1'000).buttons(std::move(buttons));

  fit::result metadata_bytes = fidl::Persist(std::move(metadata));
  if (!metadata_bytes.is_ok()) {
    zxlogf(ERROR, "Could not build metadata %s",
           metadata_bytes.error_value().FormatDescription().c_str());
    return zx::error(metadata_bytes.error_value().status());
  }
  std::vector<fuchsia_hardware_platform_bus::Metadata> adc_buttons_metadata{
      {{
          .type = DEVICE_METADATA_BUTTONS,
          .data = metadata_bytes.value(),
      }},
  };
  adc_buttons.metadata() = std::move(adc_buttons_metadata);

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('BTNS');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, adc_buttons));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd Buttons(adc_buttons) request failed: %s",
           result.FormatDescription().data());
    return result->take_error();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd Buttons(adc_buttons) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  return zx::ok();
}

}  // namespace vim3
