// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "test_parent.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <bind/fuchsia_driver_metadata_test_bind_library/cpp/bind.h>

namespace fdf_metadata::test {

zx::result<> TestParent::Start() {
  node_.Bind(std::move(node()));

  zx_status_t status = metadata_server_.Serve(*outgoing(), dispatcher());
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to serve metadata.", KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  status = test_child_use_.Init(std::string(kTestChildUseNodeName),
                                bind_fuchsia_driver_metadata_test::TEST_CHILD_USE, metadata_server_,
                                node_, dispatcher(), fit::bind_member<&TestParent::Serve>(this));
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to add test child node.", KV("name", kTestChildUseNodeName),
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  status = test_child_no_use_.Init(
      std::string(kTestChildNoUseNodeName), bind_fuchsia_driver_metadata_test::TEST_CHILD_NO_USE,
      metadata_server_, node_, dispatcher(), fit::bind_member<&TestParent::Serve>(this));
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to add test child node.", KV("name", kTestChildNoUseNodeName),
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  return zx::ok();
}

void TestParent::Serve(fidl::ServerEnd<fuchsia_hardware_test::Parent> request) {
  bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
}

void TestParent::SetMetadata(SetMetadataRequest& request, SetMetadataCompleter::Sync& completer) {
  zx_status_t status = metadata_server_.SetMetadata(request.metadata());
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to set metadata.", KV("status", zx_status_get_string(status)));
    completer.Reply(fit::error(status));
  }
  completer.Reply(fit::ok());
}

zx_status_t TestParent::TestChild::Init(
    std::string node_name, std::string test_child_property_value,
    fuchsia_hardware_test::MetadataServer& metadata_server,
    fidl::SyncClient<fuchsia_driver_framework::Node>& node, async_dispatcher_t* dispatcher,
    fit::function<void(fidl::ServerEnd<fuchsia_hardware_test::Parent>)> connect_callback) {
  ZX_ASSERT_MSG(!devfs_connector_.has_value() && !controller_.has_value(), "Already initialized.");

  devfs_connector_.emplace(std::move(connect_callback));
  zx::result connector = devfs_connector_->Bind(dispatcher);
  if (connector.is_error()) {
    FDF_SLOG(ERROR, "Failed to bind devfs connector.", KV("status", connector.status_string()));
    return connector.error_value();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs_args{{.connector = std::move(connector.value())}};

  std::vector<fuchsia_driver_framework::NodeProperty> properties{
      {{.key = fuchsia_driver_framework::NodePropertyKey::WithStringValue(
            bind_fuchsia_driver_metadata_test::TEST_CHILD),
        .value = fuchsia_driver_framework::NodePropertyValue::WithEnumValue(
            std::move(test_child_property_value))}}};

  fuchsia_driver_framework::NodeAddArgs args{{.name = std::move(node_name),
                                              .properties = std::move(properties),
                                              .devfs_args = std::move(devfs_args),
                                              .offers2 = std::vector{metadata_server.MakeOffer()}}};

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_SLOG(ERROR, "Failed to create node controller endpoints.",
             KV("status", controller_endpoints.status_string()));
    return controller_endpoints.error_value();
  }

  fidl::Result result =
      node->AddChild({std::move(args), std::move(controller_endpoints->server), {}});
  if (result.is_error()) {
    auto& error = result.error_value();
    FDF_SLOG(ERROR, "Failed to add child.", KV("status", error.FormatDescription()));
    return error.is_domain_error() ? ZX_ERR_INTERNAL : error.framework_error().status();
  }

  controller_.emplace(std::move(controller_endpoints->client));

  return ZX_OK;
}

}  // namespace fdf_metadata::test

FUCHSIA_DRIVER_EXPORT(fdf_metadata::test::TestParent);
