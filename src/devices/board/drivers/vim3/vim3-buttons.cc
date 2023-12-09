// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.buttons/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/adc/cpp/bind.h>
#include <bind/fuchsia/amlogic/platform/a311d/cpp/bind.h>
#include <bind/fuchsia/hardware/adc/cpp/bind.h>
#include <ddk/metadata/buttons.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "src/devices/board/drivers/vim3/vim3-adc.h"
#include "src/devices/board/drivers/vim3/vim3.h"

namespace vim3 {

zx::result<> Vim3::ButtonsInit() {
  // Function Key
  fuchsia_hardware_platform_bus::Node adc_buttons;
  adc_buttons.name() = "buttons";
  adc_buttons.vid() = PDEV_VID_GENERIC;
  adc_buttons.pid() = PDEV_PID_GENERIC;
  adc_buttons.did() = PDEV_DID_ADC_BUTTONS;

  auto func_types = std::vector<fuchsia_input_report::ConsumerControlButton>{
      fuchsia_input_report::ConsumerControlButton::kFunction};
  auto func_adc_config =
      fuchsia_buttons::AdcButtonConfig().channel_idx(2).release_threshold(1'000).press_threshold(
          70);
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
  const std::vector<fuchsia_driver_framework::BindRule> kFunctionButtonCompositeRules = {
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_adc::SERVICE,
                              bind_fuchsia_hardware_adc::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia_adc::CHANNEL, VIM3_ADC_BUTTON),
  };
  const std::vector<fuchsia_driver_framework::NodeProperty> kFunctionButtonCompositeProperties = {
      fdf::MakeProperty(bind_fuchsia_hardware_adc::SERVICE,
                        bind_fuchsia_hardware_adc::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia_adc::FUNCTION, bind_fuchsia_adc::FUNCTION_BUTTON),
      fdf::MakeProperty(bind_fuchsia_adc::CHANNEL, VIM3_ADC_BUTTON),
  };

  const std::vector<fuchsia_driver_framework::ParentSpec> kFunctionButtonParents = {
      fuchsia_driver_framework::ParentSpec{{.bind_rules = kFunctionButtonCompositeRules,
                                            .properties = kFunctionButtonCompositeProperties}}};
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, adc_buttons),
      fidl::ToWire(fidl_arena,
                   fuchsia_driver_framework::CompositeNodeSpec{
                       {.name = "function-button", .parents = kFunctionButtonParents}}));
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
