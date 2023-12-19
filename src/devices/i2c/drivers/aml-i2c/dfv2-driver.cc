// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dfv2-driver.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <bind/fuchsia/cpp/bind.h>

namespace {

constexpr std::string_view kDriverName = "aml-i2c";
constexpr std::string_view kChildNodeName = "aml-i2c";

}  // namespace

namespace aml_i2c {
Dfv2Driver::Dfv2Driver(fdf::DriverStartArgs start_args,
                       fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)),
      device_server_(incoming(), outgoing(), node_name(), kChildNodeName, std::nullopt,
                     compat::ForwardMetadata::Some({DEVICE_METADATA_I2C_CHANNELS}), std::nullopt) {}

zx::result<aml_i2c_delay_values> GetDelay(
    fidl::WireSyncClient<fuchsia_driver_compat::Device>& compat_client) {
  fidl::WireResult metadata = compat_client->GetMetadata();
  if (!metadata.ok()) {
    FDF_LOG(ERROR, "Failed to send GetMetadata request: %s", metadata.status_string());
    return zx::error(metadata.status());
  }
  if (metadata->is_error()) {
    FDF_LOG(ERROR, "Failed to get metadata: %s", zx_status_get_string(metadata->error_value()));
    return metadata->take_error();
  }

  auto* private_metadata =
      std::find_if(metadata->value()->metadata.begin(), metadata->value()->metadata.end(),
                   [](const auto& metadata) { return metadata.type == DEVICE_METADATA_PRIVATE; });

  if (private_metadata == metadata->value()->metadata.end()) {
    FDF_LOG(DEBUG, "Using default delay values: No metadata found");
    return zx::ok(aml_i2c_delay_values{0, 0});
  }

  size_t size;
  auto status = private_metadata->data.get_prop_content_size(&size);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to get_prop_content_size: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  std::vector<uint8_t> data;
  data.resize(size);
  status = private_metadata->data.read(data.data(), 0, size);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to read metadata: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  aml_i2c_delay_values delay_values;
  memcpy(&delay_values, data.data(), sizeof delay_values);
  return zx::ok(delay_values);
}

zx_status_t Dfv2Driver::CreateAmlI2cChildNode() {
  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    FDF_LOG(ERROR, "Failed to create controller endpoints: %s",
            controller_endpoints.status_string());
    return controller_endpoints.status_value();
  }

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 1);
  properties[0] = fdf::MakeProperty(arena, bind_fuchsia::FIDL_PROTOCOL,
                                    static_cast<uint32_t>(ZX_FIDL_PROTOCOL_I2C_IMPL));

  std::vector<fuchsia_component_decl::wire::Offer> offers = device_server_.CreateOffers(arena);
  offers.push_back(
      fdf::MakeOffer<fuchsia_hardware_i2cimpl::Service>(arena, component::kDefaultInstance));

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, kChildNodeName)
                        .offers(offers)
                        .properties(properties)
                        .Build();
  fidl::WireResult result =
      fidl::WireCall(node())->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send request to add child: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to add child: %u", static_cast<uint32_t>(result->error_value()));
    return ZX_ERR_INTERNAL;
  }
  child_controller_.Bind(std::move(controller_endpoints->client));

  return ZX_OK;
}

zx::result<> Dfv2Driver::Start() {
  auto device_server_result = device_server_.InitResult();
  if (device_server_result.has_value()) {
    if (device_server_result->is_error()) {
      FDF_LOG(ERROR, "Failed to initialize device server: %s",
              device_server_result->status_string());
      return device_server_result->take_error();
    }
  } else {
    FDF_LOG(ERROR, "Device server not initialized");
    return zx::error(ZX_ERR_INTERNAL);
  }

  zx::result compat_result = incoming()->Connect<fuchsia_driver_compat::Service::Device>();
  if (compat_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to compat service: %s", compat_result.status_string());
    return compat_result.take_error();
  }
  auto compat_client = fidl::WireSyncClient(std::move(compat_result.value()));

  zx::result pdev_result =
      incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>("pdev");
  if (pdev_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to pdev protocol: %s", pdev_result.status_string());
    return pdev_result.take_error();
  }
  ddk::PDevFidl pdev(std::move(pdev_result.value()));
  if (!pdev.is_valid()) {
    FDF_LOG(ERROR, "ZX_PROTOCOL_PDEV not available");
    return zx::error(ZX_ERR_NO_RESOURCES);
  }

  zx::result delay = GetDelay(compat_client);
  if (delay.is_error()) {
    FDF_LOG(ERROR, "Failed to get delay values");
    return delay.take_error();
  }

  zx::result aml_i2c = AmlI2c::Create(pdev, delay.value());
  if (aml_i2c.is_error() != ZX_OK) {
    FDF_LOG(ERROR, "Failed to initialize: %s", aml_i2c.status_string());
    return aml_i2c.take_error();
  }
  aml_i2c_ = std::move(aml_i2c.value());

  {
    zx::result result = outgoing()->AddService<fuchsia_hardware_i2cimpl::Service>(
        aml_i2c_->GetI2cImplInstanceHandler(fdf::Dispatcher::GetCurrent()->get()));
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add I2C impl service to outgoing: %s", result.status_string());
      return result.take_error();
    }
  }

  zx_status_t status = CreateAmlI2cChildNode();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to create aml-i2c child node: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

}  // namespace aml_i2c

FUCHSIA_DRIVER_EXPORT(aml_i2c::Dfv2Driver);
