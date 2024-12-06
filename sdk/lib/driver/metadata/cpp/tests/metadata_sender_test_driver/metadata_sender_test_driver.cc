// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/metadata/cpp/tests/metadata_sender_test_driver/metadata_sender_test_driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <bind/fuchsia_driver_metadata_test_bind_library/cpp/bind.h>

namespace fdf_metadata::test {

zx::result<> MetadataSenderTestDriver::Start() {
  zx_status_t status = InitControllerNode();
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to initialize controller node.",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  return zx::ok();
}

void MetadataSenderTestDriver::Serve(
    fidl::ServerEnd<fuchsia_hardware_test::MetadataSender> request) {
  bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
}

void MetadataSenderTestDriver::ServeMetadata(ServeMetadataCompleter::Sync& completer) {
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  zx::result result = metadata_server_.Serve(*outgoing(), dispatcher());
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to serve metadata.", KV("status", result.status_string()));
    completer.Reply(fit::error(result.error_value()));
    return;
  }
  offer_metadata_to_child_nodes_ = true;
  completer.Reply(fit::ok());
#else
  FDF_SLOG(ERROR, "Serving metadata not supported at current Fuchsia API level.");
  completer.Reply(fit::error(ZX_ERR_NOT_SUPPORTED));
#endif
}

void MetadataSenderTestDriver::SetMetadata(SetMetadataRequest& request,
                                           SetMetadataCompleter::Sync& completer) {
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  zx::result result = metadata_server_.SetMetadata(request.metadata());
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to set metadata.", KV("status", result.status_string()));
    completer.Reply(fit::error(result.error_value()));
  }
  completer.Reply(fit::ok());
#else
  FDF_SLOG(ERROR, "Setting metadata not supported at current Fuchsia API level.");
  completer.Reply(fit::error(ZX_ERR_NOT_SUPPORTED));
#endif
}

void MetadataSenderTestDriver::AddMetadataRetrieverNode(
    AddMetadataRetrieverNodeRequest& request, AddMetadataRetrieverNodeCompleter::Sync& completer) {
  bool uses_metadata_fidl_service = request.uses_metadata_fidl_service();

  std::stringstream node_name;
  node_name << "metadata_retriever_" << (uses_metadata_fidl_service ? "use" : "no_use") << '_'
            << metadata_node_controllers_.size();

  std::vector<fuchsia_driver_framework::NodeProperty> node_properties{
      fdf::MakeProperty(bind_fuchsia_driver_metadata_test::PURPOSE,
                        bind_fuchsia_driver_metadata_test::PURPOSE_RETRIEVE_METADATA),
      fdf::MakeProperty(bind_fuchsia_driver_metadata_test::USES_METADATA_FIDL_SERVICE,
                        uses_metadata_fidl_service)};

  zx_status_t status = AddMetadataNode(node_name.str(), node_properties);
  if (status != ZX_OK) {
    // Don't log. AddMetadataNode() performs error logging.
    completer.Reply(fit::error(status));
    return;
  }

  completer.Reply(fit::ok(node_name.str()));
}

void MetadataSenderTestDriver::AddMetadataForwarderNode(
    AddMetadataForwarderNodeCompleter::Sync& completer) {
  std::stringstream node_name;
  node_name << "metadata_forwarder_" << metadata_node_controllers_.size();

  static const std::vector<fuchsia_driver_framework::NodeProperty> kNodeProperties{
      fdf::MakeProperty(bind_fuchsia_driver_metadata_test::PURPOSE,
                        bind_fuchsia_driver_metadata_test::PURPOSE_FORWARD_METADATA)};

  zx_status_t status = AddMetadataNode(node_name.str(), kNodeProperties);
  if (status != ZX_OK) {
    // Don't log. AddMetadataNode() performs error logging.
    completer.Reply(fit::error(status));
    return;
  }

  completer.Reply(fit::ok(node_name.str()));
}

zx_status_t MetadataSenderTestDriver::AddMetadataNode(
    std::string_view node_name,
    const fuchsia_driver_framework::NodePropertyVector& node_properties) {
  std::vector<fuchsia_driver_framework::Offer> offers;
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (offer_metadata_to_child_nodes_) {
    offers.emplace_back(metadata_server_.MakeOffer());
  }
#endif
  zx::result result = AddChild(node_name, node_properties, offers);
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add child.", KV("status", result.status_string()));
    return result.status_value();
  }
  metadata_node_controllers_.emplace_back(std::move(result.value()));
  return ZX_OK;
}

zx_status_t MetadataSenderTestDriver::InitControllerNode() {
  if (controller_node_.has_value()) {
    FDF_SLOG(ERROR, "Controller node already initialized.");
    return ZX_ERR_BAD_STATE;
  }

  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    FDF_SLOG(ERROR, "Failed to bind devfs connector.", KV("status", connector.status_string()));
    return connector.status_value();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs_args{{.connector = std::move(connector.value())}};

  zx::result result = AddOwnedChild(kControllerNodeName, devfs_args);
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add child.", KV("status", result.status_string()));
    return result.status_value();
  }

  controller_node_.emplace(std::move(result.value()));

  return ZX_OK;
}

}  // namespace fdf_metadata::test

FUCHSIA_DRIVER_EXPORT(fdf_metadata::test::MetadataSenderTestDriver);
