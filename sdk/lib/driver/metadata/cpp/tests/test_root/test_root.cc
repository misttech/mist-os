// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/metadata/cpp/tests/test_root/test_root.h"

#include <fidl/fuchsia.driver.framework/cpp/natural_types.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <bind/fuchsia_driver_metadata_test_bind_library/cpp/bind.h>

namespace fdf_metadata::test {

zx::result<> TestRootDriver::Start() {
  zx_status_t status = InitControllerChildNode();
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to initialize controller node.",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  return zx::ok();
}

void TestRootDriver::Serve(fidl::ServerEnd<fuchsia_hardware_test::Root> request) {
  bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
}

void TestRootDriver::AddMetadataSenderNode(AddMetadataSenderNodeRequest& request,
                                           AddMetadataSenderNodeCompleter::Sync& completer) {
  bool exposes_metadata_fidl_service = request.exposes_metadata_fidl_service();

  std::vector<fuchsia_driver_framework::NodeProperty> node_properties{
      fdf::MakeProperty(bind_fuchsia_driver_metadata_test::PURPOSE,
                        bind_fuchsia_driver_metadata_test::PURPOSE_SEND_METADATA),
      fdf::MakeProperty(bind_fuchsia_driver_metadata_test::EXPOSES_METADATA_FIDL_SERVICE,
                        exposes_metadata_fidl_service)};

  std::stringstream node_name;
  node_name << "metadata_sender_" << (exposes_metadata_fidl_service ? "expose" : "no_expose") << '_'
            << metadata_sender_node_controllers_.size();

  zx::result result = AddChild(node_name.str(), node_properties, {});
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add child.", KV("status", result.status_string()));
    completer.Reply(fit::error(result.status_value()));
    return;
  }

  metadata_sender_node_controllers_.emplace_back(std::move(result.value()));
  completer.Reply(fit::ok(node_name.str()));
}

zx_status_t TestRootDriver::InitControllerChildNode() {
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

FUCHSIA_DRIVER_EXPORT(fdf_metadata::test::TestRootDriver);
