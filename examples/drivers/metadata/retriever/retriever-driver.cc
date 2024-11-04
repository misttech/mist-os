// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/logging/cpp/logger.h>

#include "examples/drivers/metadata/fuchsia.examples.metadata/metadata.h"

namespace examples::drivers::metadata {

// This driver demonstrates how to retrieve the metadata from its parent driver,
// Forwarder. It implements the `fuchsia_examples_metadata::Retriever` protocol
// for testing purposes.
class RetrieverDriver final : public fdf::DriverBase,
                              public fidl::Server<fuchsia_examples_metadata::Retriever> {
 public:
  RetrieverDriver(fdf::DriverStartArgs start_args,
                  fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("child", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    zx_status_t status = AddDevfsChild();
    if (status != ZX_OK) {
      fdf::error("Failed to add child: {}", zx::make_result(status));
      return zx::error(status);
    }

    return zx::ok();
  }

  // fuchsia.hardware.test/Child implementation.
  void GetMetadata(GetMetadataCompleter::Sync& completer) override {
    zx::result metadata =
        fdf_metadata::GetMetadata<fuchsia_examples_metadata::Metadata>(incoming());
    if (metadata.is_error()) {
      fdf::error("Failed to get metadata: {}", metadata);
      completer.Reply(fit::error(metadata.status_value()));
      return;
    }

    completer.Reply(fit::ok(std::move(metadata.value())));
  }

 private:
  void Serve(fidl::ServerEnd<fuchsia_examples_metadata::Retriever> request) {
    bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
  }

  // Add child that has a devfs connection in order for tests to communicate with this driver.
  zx_status_t AddDevfsChild() {
    if (child_node_.has_value()) {
      fdf::error("Child node already created.");
      return ZX_ERR_BAD_STATE;
    }

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      fdf::error("Failed to bind devfs connector: {}", connector);
      return connector.status_value();
    }

    // [START add_child]
    fuchsia_driver_framework::DevfsAddArgs devfs_args{{.connector = std::move(connector.value())}};
    zx::result owned_child = AddOwnedChild("retriever", devfs_args);
    if (owned_child.is_error()) {
      return owned_child.error_value();
    }

    child_node_.emplace(std::move(owned_child.value()));
    // [END add_child]

    return ZX_OK;
  }

  // Used by tests in order to communicate with the driver.
  driver_devfs::Connector<fuchsia_examples_metadata::Retriever> devfs_connector_{
      fit::bind_member<&RetrieverDriver::Serve>(this)};

  fidl::ServerBindingGroup<fuchsia_examples_metadata::Retriever> bindings_;
  std::optional<fdf::OwnedChildNode> child_node_;
};

}  // namespace examples::drivers::metadata

FUCHSIA_DRIVER_EXPORT(examples::drivers::metadata::RetrieverDriver);
