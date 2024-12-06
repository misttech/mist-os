// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/metadata/cpp/tests/metadata_forwarder_test_driver/metadata_forwarder_test_driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <bind/fuchsia_driver_metadata_test_bind_library/cpp/bind.h>

namespace fdf_metadata::test {

zx::result<> MetadataForwarderTestDriver::Start() {
  zx::result result = metadata_server_.Serve(*outgoing(), dispatcher());
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to serve metadata.", KV("status", result.status_string()));
    return result.take_error();
  }

  zx_status_t status = InitMetadataRetrieverNode();
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to initialize metadata retriever node.",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  status = InitControllerNode();
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to initialize controller node.",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  return zx::ok();
}

zx_status_t MetadataForwarderTestDriver::InitMetadataRetrieverNode() {
  if (metadata_retriever_node_controller_.has_value()) {
    FDF_LOG(ERROR, "Metadata retriever node already initialized.");
    return ZX_ERR_BAD_STATE;
  }

  static const std::vector<fuchsia_driver_framework::NodeProperty> kNodeProperties{
      fdf::MakeProperty(bind_fuchsia_driver_metadata_test::PURPOSE,
                        bind_fuchsia_driver_metadata_test::PURPOSE_RETRIEVE_METADATA),
      fdf::MakeProperty(bind_fuchsia_driver_metadata_test::USES_METADATA_FIDL_SERVICE, true)};

  std::vector<fuchsia_driver_framework::Offer> offers;
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  offers.emplace_back(metadata_server_.MakeOffer());
#endif
  zx::result result = AddChild(kMetadataRetrieverNodeName, kNodeProperties, std::move(offers));
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add child.", KV("status", result.status_string()));
    return result.status_value();
  }

  metadata_retriever_node_controller_.emplace(std::move(result.value()));

  return ZX_OK;
}

zx_status_t MetadataForwarderTestDriver::InitControllerNode() {
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

void MetadataForwarderTestDriver::Serve(
    fidl::ServerEnd<fuchsia_hardware_test::MetadataForwarder> request) {
  bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
}

void MetadataForwarderTestDriver::ForwardMetadata(ForwardMetadataCompleter::Sync& completer) {
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  zx::result result = metadata_server_.ForwardMetadata(incoming());
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to forward metadata.", KV("status", result.status_string()));
    completer.Reply(fit::error(result.error_value()));
    return;
  }

  completer.Reply(fit::ok());
#else
  FDF_SLOG(ERROR, "Forwarding metadata not supported at current Fuchsia API level.");
  completer.Reply(fit::error(status));
#endif
}

}  // namespace fdf_metadata::test

FUCHSIA_DRIVER_EXPORT(fdf_metadata::test::MetadataForwarderTestDriver);
