// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vim3-adc-buttons.h"

#include <fidl/fuchsia.buttons/cpp/fidl.h>
#include <fidl/fuchsia.hardware.adcimpl/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/adc/cpp/bind.h>
#include <bind/fuchsia/hardware/adc/cpp/bind.h>

namespace vim3_dt {

zx::result<> Vim3AdcButtonsVisitor::DriverVisit(fdf_devicetree::Node& node,
                                                const devicetree::PropertyDecoder& decoder) {
  // Add metadata and composite node spec for vim3 adc buttons.
  if (node.name() == "adc-buttons") {
    auto result = AddAdcButtonMetadata(node);
    if (result.is_error()) {
      return result.take_error();
    }
    return AddAdcButtonCompositeSpec(node);
  }

  // Add corresponding adc channel info to adc node.
  if (node.name() == "adc@9000") {
    return AddAdcMetadata(node);
  }

  return zx::ok();
}

zx::result<> Vim3AdcButtonsVisitor::AddAdcButtonMetadata(fdf_devicetree::Node& node) {
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

  // How long to wait between polling attempts.  This value should be large enough to ensure
  // polling does not overly impact system performance while being small enough to debounce and
  // ensure button presses are correctly registered.
  //
  // TODO(https//fxbug/dev/315366570): Change the driver to use an IRQ instead of polling.
  constexpr uint32_t kPollingPeriodUSec = 20'000;
  auto metadata =
      fuchsia_buttons::Metadata().polling_rate_usec(kPollingPeriodUSec).buttons(std::move(buttons));

  fit::result metadata_bytes = fidl::Persist(std::move(metadata));
  if (!metadata_bytes.is_ok()) {
    FDF_LOG(ERROR, "Could not build fuchsia buttons metadata %s",
            metadata_bytes.error_value().FormatDescription().c_str());
    return zx::error(metadata_bytes.error_value().status());
  }

  fuchsia_hardware_platform_bus::Metadata adc_buttons_metadata{{
      .type = DEVICE_METADATA_BUTTONS,
      .data = metadata_bytes.value(),
  }};
  node.AddMetadata(adc_buttons_metadata);

  FDF_LOG(DEBUG, "Adding vim3 adc button metadata for node '%s' ", node.name().c_str());

  return zx::ok();
}

zx::result<> Vim3AdcButtonsVisitor::AddAdcButtonCompositeSpec(fdf_devicetree::Node& node) {
  // Add ADC parent node spec.
  const std::vector<fuchsia_driver_framework::BindRule> kFunctionButtonCompositeRules = {
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_adc::SERVICE,
                              bind_fuchsia_hardware_adc::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeAcceptBindRule(bind_fuchsia_adc::CHANNEL, 2u),
  };
  const std::vector<fuchsia_driver_framework::NodeProperty> kFunctionButtonCompositeProperties = {
      fdf::MakeProperty(bind_fuchsia_hardware_adc::SERVICE,
                        bind_fuchsia_hardware_adc::SERVICE_ZIRCONTRANSPORT),
      fdf::MakeProperty(bind_fuchsia_adc::FUNCTION, bind_fuchsia_adc::FUNCTION_BUTTON),
      fdf::MakeProperty(bind_fuchsia_adc::CHANNEL, 2u),
  };

  auto adc_button_node = fuchsia_driver_framework::ParentSpec{
      {kFunctionButtonCompositeRules, kFunctionButtonCompositeProperties}};

  node.AddNodeSpec(adc_button_node);

  FDF_LOG(DEBUG, "Adding vim3 adc button composite node for node '%s' ", node.name().c_str());

  return zx::ok();
}

zx::result<> Vim3AdcButtonsVisitor::AddAdcMetadata(fdf_devicetree::Node& node) {
  std::vector<fuchsia_hardware_adcimpl::AdcChannel> adc_channels = {
      {{.idx = 2u, .name = "VIM3_ADC_BUTTON"}}};
  fuchsia_hardware_adcimpl::Metadata metadata;
  metadata.channels(std::move(adc_channels));
  auto encoded_metadata = fidl::Persist(std::move(metadata));
  if (!encoded_metadata.is_ok()) {
    FDF_LOG(ERROR, "Could not build adc channels metadata %s",
            encoded_metadata.error_value().FormatDescription().c_str());
    return zx::error(encoded_metadata.error_value().status());
  }

  fuchsia_hardware_platform_bus::Metadata adc_metadata{{
      .type = DEVICE_METADATA_ADC,
      .data = encoded_metadata.value(),
  }};

  node.AddMetadata(adc_metadata);

  FDF_LOG(DEBUG, "Adding vim3 adc channel metadata for node '%s' ", node.name().c_str());

  return zx::ok();
}

}  // namespace vim3_dt
