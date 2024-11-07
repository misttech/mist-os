// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/logging/cpp/logger.h>

#include <bind/fuchsia_examples_metadata_bind_library/cpp/bind.h>

#include "examples/drivers/metadata/fuchsia.examples.metadata/metadata.h"

namespace examples::drivers::metadata {

// This driver demonstrates how to send
// `fuchsia.examples.metadata.Metadata` metadata to its children.
// It implements `fuchsia_examples_metadata::Sender` protocol for testing.
class SenderDriver final : public fdf::DriverBase,
                           public fidl::Server<fuchsia_examples_metadata::Sender> {
 public:
  SenderDriver(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("sender", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    // Serve the metadata to the driver's child nodes.
    zx_status_t status = metadata_server_.Serve(*outgoing(), dispatcher());
    if (status != ZX_OK) {
      fdf::error("Failed to serve metadata: {}", zx::make_result(status));
      return zx::error(status);
    }

    status = AddForwarderChild();
    if (status != ZX_OK) {
      fdf::error("Failed to add forwarder child: {}", zx::make_result(status));
      return zx::error(status);
    }

    return zx::ok();
  }

  // fuchsia.examples.metadata/Sender implementation.
  void SetMetadata(SetMetadataRequest& request, SetMetadataCompleter::Sync& completer) override {
    zx_status_t status = metadata_server_.SetMetadata(request.metadata());
    if (status != ZX_OK) {
      fdf::error("Failed to set metadata: {}", zx::make_result(status));
      completer.Reply(fit::error(status));
    }
    completer.Reply(fit::ok());
  }

 private:
  void Serve(fidl::ServerEnd<fuchsia_examples_metadata::Sender> request) {
    bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
  }

  // Add a child node for the `forwarder` driver to bind to.
  zx_status_t AddForwarderChild() {
    ZX_ASSERT_MSG(!controller_.has_value(), "Already added child.");

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      fdf::error("Failed to bind devfs connector: {}", connector);
      return connector.error_value();
    }

    fuchsia_driver_framework::DevfsAddArgs devfs_args{{.connector = std::move(connector.value())}};

    static const std::vector<fuchsia_driver_framework::NodeProperty> kProperties{
        fdf::MakeProperty(bind_fuchsia_examples_metadata::CHILD_TYPE,
                          bind_fuchsia_examples_metadata::CHILD_TYPE_FORWARDER)};

    // Offer the metadata service to the child node.
    std::vector offers{metadata_server_.MakeOffer()};

    zx::result controller = AddChild("sender", devfs_args, kProperties, offers);
    if (controller.is_error()) {
      fdf::error("Failed to add child: {}", controller);
      return controller.error_value();
    }

    controller_.emplace(std::move(controller.value()));

    return ZX_OK;
  }

  // Responsible for serving metadata.
  MetadataServer metadata_server_;

  // Used by tests in order to communicate with the driver via devfs.
  driver_devfs::Connector<fuchsia_examples_metadata::Sender> devfs_connector_{
      fit::bind_member<&SenderDriver::Serve>(this)};

  fidl::ServerBindingGroup<fuchsia_examples_metadata::Sender> bindings_;
  std::optional<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> controller_;
};

}  // namespace examples::drivers::metadata

FUCHSIA_DRIVER_EXPORT(examples::drivers::metadata::SenderDriver);
