// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "adc-buttons.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/driver/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/mmio/mmio.h>

#include "fbl/ref_ptr.h"

namespace adc_buttons {

namespace {

constexpr uint32_t kDefaultPollingRateUsec = 1'000;

struct MetadataValues {
  uint32_t polling_rate_usec = kDefaultPollingRateUsec;
  std::map<uint32_t, std::vector<fuchsia_buttons::Button>> configs;
  std::set<fuchsia_input_report::ConsumerControlButton> buttons;
};

zx::result<MetadataValues> ParseMetadata(
    const fidl::VectorView<::fuchsia_driver_compat::wire::Metadata>& metadata) {
  MetadataValues ret;

  for (const auto& m : metadata) {
    if (m.type == DEVICE_METADATA_BUTTONS) {
      size_t size;
      auto status = m.data.get_prop_content_size(&size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "%s: failed to get_prop_content_size %s", __func__,
                zx_status_get_string(status));
        continue;
      }

      std::vector<uint8_t> data;
      data.resize(size);
      status = m.data.read(data.data(), 0, size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "%s: failed to read %s", __func__, zx_status_get_string(status));
        continue;
      }

      auto result = fidl::Unpersist<fuchsia_buttons::Metadata>(cpp20::span(data));
      if (result.is_error()) {
        FDF_LOG(ERROR, "%s: failed to unpersist metadata %s", __func__,
                result.error_value().FormatDescription().data());
        continue;
      }

      ret.polling_rate_usec = *result->polling_rate_usec();

      for (auto& button : *result->buttons()) {
        if (button.button_config()->Which() != fuchsia_buttons::ButtonConfig::Tag::kAdc) {
          FDF_LOG(WARNING, "%s: received button config not of type adc, ignoring.", __func__);
          continue;
        }
        ret.buttons.insert(button.types()->begin(), button.types()->end());
        ret.configs[*button.button_config()->adc()->channel_idx()].emplace_back(std::move(button));
      }
      if (ret.buttons.size() >= fuchsia_input_report::kConsumerControlMaxNumButtons) {
        FDF_LOG(
            ERROR,
            "%s: More buttons than expected (max = %d). Please increase kConsumerControlMaxNumButtons",
            __func__, fuchsia_input_report::kConsumerControlMaxNumButtons);
        return zx::error(ZX_ERR_INTERNAL);
      }

      break;
    }
  }

  if (ret.configs.empty() || ret.buttons.empty()) {
    FDF_LOG(ERROR, "%s: failed to get button configs in metadata.", __func__);
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok(std::move(ret));
}

}  // namespace

zx::result<> AdcButtons::Start() {
  // Map hardware resources from pdev.
  std::optional<fdf::MmioBuffer> adc_mmio;
  std::optional<fdf::MmioBuffer> ao_mmio;
  zx::interrupt irq;
  {
    zx::result result = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open pdev service: %s", result.status_string());
      return result.take_error();
    }
    auto pdev = ddk::PDevFidl(std::move(result.value()));
    if (!pdev.is_valid()) {
      FDF_LOG(ERROR, "%s: failed to get pdev", __func__);
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    auto status = pdev.MapMmio(0, &adc_mmio);
    if (status != ZX_OK) {
      return zx::error(status);
    }
    status = pdev.MapMmio(1, &ao_mmio);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    status = pdev.GetInterrupt(0, &irq);
    if (status != ZX_OK) {
      return zx::error(status);
    }
  }

  // Get metadata.
  MetadataValues values;
  {
    zx::result result = incoming()->Connect<fuchsia_driver_compat::Service::Device>();
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open pdev service: %s", result.status_string());
      return result.take_error();
    }
    auto compat = fidl::WireSyncClient(std::move(result.value()));
    if (!compat.is_valid()) {
      FDF_LOG(ERROR, "%s: failed to get compat", __func__);
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    auto metadata = compat->GetMetadata();
    if (!metadata.ok()) {
      FDF_LOG(ERROR, "%s: failed to GetMetadata %s", __func__, metadata.FormatDescription().data());
      return zx::error(metadata.status());
    }
    if (metadata->is_error()) {
      FDF_LOG(ERROR, "%s: failed to GetMetadata %s", __func__,
              zx_status_get_string(metadata->error_value()));
      return metadata->take_error();
    }

    auto vals = ParseMetadata(metadata.value()->metadata);
    if (vals.is_error()) {
      FDF_LOG(ERROR, "%s: failed to ParseMetadata %s", __func__,
              zx_status_get_string(vals.error_value()));
      return vals.take_error();
    }
    values = std::move(vals.value());
  }

  device_ = std::make_unique<adc_buttons_device::AdcButtonsDevice>(
      dispatcher(),
      std::make_unique<AmlSaradcDevice>(*std::move(adc_mmio), *std::move(ao_mmio), std::move(irq)),
      values.polling_rate_usec, std::move(values.configs), std::move(values.buttons));

  auto result = outgoing()->component().AddUnmanagedProtocol<fuchsia_input_report::InputDevice>(
      input_report_bindings_.CreateHandler(device_.get(), dispatcher(),
                                           fidl::kIgnoreBindingClosure),
      kDeviceName);
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Device service %s", result.status_string());
    return result.take_error();
  }

  if (zx::result result = CreateDevfsNode(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to export to devfs %s", result.status_string());
    return result.take_error();
  }

  return zx::ok();
}

void AdcButtons::Stop() { device_->Shutdown(); }

zx::result<> AdcButtons::CreateDevfsNode() {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("input-report");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDeviceName)
                  .devfs_args(devfs.Build())
                  .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create endpoints: %s",
                controller_endpoints.status_string());

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed to create endpoints: %s",
                node_endpoints.status_string());

  fidl::WireResult result = fidl::WireCall(node())->AddChild(
      args, std::move(controller_endpoints->server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", result.status_string());
    return zx::error(result.status());
  }
  controller_.Bind(std::move(controller_endpoints->client));
  node_.Bind(std::move(node_endpoints->client));
  return zx::ok();
}

}  // namespace adc_buttons

FUCHSIA_DRIVER_EXPORT(adc_buttons::AdcButtons);
