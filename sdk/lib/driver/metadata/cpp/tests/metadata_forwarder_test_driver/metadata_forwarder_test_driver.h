// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_METADATA_CPP_TESTS_METADATA_FORWARDER_TEST_DRIVER_METADATA_FORWARDER_TEST_DRIVER_H_
#define LIB_DRIVER_METADATA_CPP_TESTS_METADATA_FORWARDER_TEST_DRIVER_METADATA_FORWARDER_TEST_DRIVER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.test/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/metadata/cpp/metadata_server.h>
#include <lib/driver/node/cpp/add_child.h>

namespace fdf_metadata::test {

// This driver's purpose is to forward metadata using `fdf::MetadataServer::ForwardMetadata()`.
class MetadataForwarderTestDriver : public fdf::DriverBase,
                                    public fidl::Server<fuchsia_hardware_test::MetadataForwarder> {
 public:
  static constexpr std::string_view kDriverName = "metadata_forwarder";
  static constexpr std::string_view kControllerNodeName = "controller";
  static constexpr std::string_view kMetadataRetrieverNodeName = "metadata_retriever";

  MetadataForwarderTestDriver(fdf::DriverStartArgs start_args,
                              fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

  // fuchsia.hardware.test/MetadataForwarder implementation.
  void ForwardMetadata(ForwardMetadataCompleter::Sync& completer) override;

 private:
  void Serve(fidl::ServerEnd<fuchsia_hardware_test::MetadataForwarder> request);

  // Create a non-bindable child node whose purpose is to expose the
  // fuchsia.hardware.test/MetadataForwarder FIDL protocol served by this to devfs.
  zx_status_t InitControllerNode();

  // Create a child node that the metadata_retriever_use driver can bind to.
  zx_status_t InitMetadataRetrieverNode();

  fidl::ServerBindingGroup<fuchsia_hardware_test::MetadataForwarder> bindings_;
  driver_devfs::Connector<fuchsia_hardware_test::MetadataForwarder> devfs_connector_{
      fit::bind_member<&MetadataForwarderTestDriver::Serve>(this)};
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata> metadata_server_;
#endif

  std::optional<fdf::OwnedChildNode> controller_node_;

  std::optional<fidl::ClientEnd<fuchsia_driver_framework::NodeController>>
      metadata_retriever_node_controller_;
};

}  // namespace fdf_metadata::test

#endif  // LIB_DRIVER_METADATA_CPP_TESTS_METADATA_FORWARDER_TEST_DRIVER_METADATA_FORWARDER_TEST_DRIVER_H_
