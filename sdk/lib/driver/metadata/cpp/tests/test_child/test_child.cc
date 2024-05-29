// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "test_child.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/driver/metadata/cpp/metadata.h>

#include <bind/fuchsia_driver_metadata_test_bind_library/cpp/bind.h>

namespace fdf_metadata::test {

zx::result<> TestChild::Start() {
  node_.Bind(std::move(node()));

  zx_status_t status = forward_metadata_server_.Serve(*outgoing(), dispatcher());
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to serve metadata.", KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  // Create a non-bindable child node whose purpose is to expose the fuchsia.hardware.test/Child
  // protocol served by this to /dev/.
  status = AddChild();
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to export devfs node.", KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  return zx::ok();
}

zx_status_t TestChild::AddChild() {
  if (child_node_controller_.has_value()) {
    FDF_LOG(ERROR, "Child node already created.");
    return ZX_ERR_BAD_STATE;
  }

  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    FDF_SLOG(ERROR, "Failed to bind devfs connector.", KV("status", connector.status_string()));
    return connector.status_value();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs_args{{.connector = std::move(connector.value())}};

  std::vector<fuchsia_driver_framework::NodeProperty> properties{
      {{.key = fuchsia_driver_framework::NodePropertyKey::WithStringValue(
            bind_fuchsia_driver_metadata_test::TEST_GRANDCHILD),
        .value = fuchsia_driver_framework::NodePropertyValue::WithStringValue(
            std::string{kTestGrandchildPropertyValue})}}};

  fuchsia_driver_framework::NodeAddArgs args{
      {.name = std::string(kChildNodeName),
       .properties = std::move(properties),
       .devfs_args = std::move(devfs_args),
       .offers2 = std::vector{forward_metadata_server_.MakeOffer()}}};

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_SLOG(ERROR, "Failed to create node controller endpoints.",
             KV("status", controller_endpoints.status_string()));
    return controller_endpoints.status_value();
  }

  fidl::Result result =
      node_->AddChild({std::move(args), std::move(controller_endpoints->server), {}});
  if (result.is_error()) {
    auto& error = result.error_value();
    FDF_SLOG(ERROR, "Failed to add child.", KV("status", error.FormatDescription()));
    return error.is_domain_error() ? ZX_ERR_INTERNAL : error.framework_error().status();
  }

  child_node_controller_.emplace(std::move(controller_endpoints->client));

  return ZX_OK;
}

void TestChild::Serve(fidl::ServerEnd<fuchsia_hardware_test::Child> request) {
  bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
}

void TestChild::GetMetadata(GetMetadataCompleter::Sync& completer) {
  zx::result metadata = fdf_metadata::GetMetadata<fuchsia_hardware_test::Metadata>(incoming());
  if (metadata.is_error()) {
    FDF_SLOG(ERROR, "Failed to get metadata.", KV("status", metadata.status_string()));
    completer.Reply(fit::error(metadata.status_value()));
    return;
  }

  completer.Reply(fit::ok(std::move(metadata.value())));
}

void TestChild::ForwardMetadata(ForwardMetadataCompleter::Sync& completer) {
  zx_status_t status = forward_metadata_server_.ForwardMetadata(incoming());
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to forward metadata.", KV("status", zx_status_get_string(status)));
    completer.Reply(fit::error(status));
    return;
  }

  completer.Reply(fit::ok());
}

}  // namespace fdf_metadata::test

FUCHSIA_DRIVER_EXPORT(fdf_metadata::test::TestChild);
