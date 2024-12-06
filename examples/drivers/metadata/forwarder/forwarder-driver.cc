// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/logging/cpp/logger.h>

#include <bind/fuchsia_examples_metadata_bind_library/cpp/bind.h>

#include "examples/drivers/metadata/fuchsia.examples.metadata/metadata.h"

namespace examples::drivers::metadata {

// This driver demonstrates how it can forward the
// `fuchsia.examples.metadata.Metadata` metadata from its parent
// driver, `Sender`, to its children. It implements
// `fuchsia_examples_metadata::Forwarder` protocol for testing.
class ForwarderDriver final : public fdf::DriverBase,
                              public fidl::Server<fuchsia_examples_metadata::Forwarder> {
 public:
  ForwarderDriver(fdf::DriverStartArgs start_args,
                  fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("parent", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    // Serve the metadata to the driver's child nodes.
    zx::result result = metadata_server_.Serve(*outgoing(), dispatcher());
    if (result.is_error()) {
      fdf::error("Failed to serve metadata: {}", result);
      return result.take_error();
    }

    zx_status_t status = AddRetrieverChild();
    if (status != ZX_OK) {
      fdf::error("Failed to add retriever child: {}", zx::make_result(status));
      return zx::error(status);
    }

    return zx::ok();
  }

  // fuchsia.examples.metadata/Forwarder implementation.
  void ForwardMetadata(ForwardMetadataCompleter::Sync& completer) override {
    zx::result result = metadata_server_.ForwardMetadata(incoming());
    if (result.is_error()) {
      fdf::error("Failed to forward metadata: {}", result);
      completer.Reply(result.take_error());
      return;
    }

    completer.Reply(fit::ok());
  }

 private:
  void Serve(fidl::ServerEnd<fuchsia_examples_metadata::Forwarder> request) {
    bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
  }

  // Add a child node for the `retriever` driver to bind to.
  zx_status_t AddRetrieverChild() {
    ZX_ASSERT_MSG(!controller_.has_value(), "Already added child.");

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      fdf::error("Failed to bind devfs connector: {}", connector);
      return connector.error_value();
    }

    fuchsia_driver_framework::DevfsAddArgs devfs_args{{.connector = std::move(connector.value())}};

    static const std::vector<fuchsia_driver_framework::NodeProperty> kProperties{
        fdf::MakeProperty(bind_fuchsia_examples_metadata::CHILD_TYPE,
                          bind_fuchsia_examples_metadata::CHILD_TYPE_RETRIEVER)};

    // Offer the metadata service to the child node.
    std::vector offers{metadata_server_.MakeOffer()};

    zx::result controller = AddChild("forwarder", devfs_args, kProperties, offers);
    if (controller.is_error()) {
      fdf::error("Failed to add child: {}", controller);
      return controller.error_value();
    }

    controller_.emplace(std::move(controller.value()));

    return ZX_OK;
  }

  // Responsible for forwarding metadata.
  MetadataServer metadata_server_;

  // Used by tests in order to communicate with the driver via devfs.
  driver_devfs::Connector<fuchsia_examples_metadata::Forwarder> devfs_connector_{
      fit::bind_member<&ForwarderDriver::Serve>(this)};

  fidl::ServerBindingGroup<fuchsia_examples_metadata::Forwarder> bindings_;
  std::optional<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> controller_;
};

}  // namespace examples::drivers::metadata

FUCHSIA_DRIVER_EXPORT(examples::drivers::metadata::ForwarderDriver);
