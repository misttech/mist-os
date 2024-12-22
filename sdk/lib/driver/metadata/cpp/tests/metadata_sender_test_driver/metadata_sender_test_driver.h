// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_METADATA_CPP_TESTS_METADATA_SENDER_TEST_DRIVER_METADATA_SENDER_TEST_DRIVER_H_
#define LIB_DRIVER_METADATA_CPP_TESTS_METADATA_SENDER_TEST_DRIVER_METADATA_SENDER_TEST_DRIVER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.test/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/metadata/cpp/metadata_server.h>
#include <lib/driver/node/cpp/add_child.h>

namespace fdf_metadata::test {

// This driver's purpose is to serve metadata to its two child nodes using `fdf::MetadataServer`.
class MetadataSenderTestDriver : public fdf::DriverBase,
                                 public fidl::Server<fuchsia_hardware_test::MetadataSender> {
 public:
  static constexpr std::string_view kDriverName = "metadata_sender";
  static constexpr std::string_view kControllerNodeName = "controller";

  MetadataSenderTestDriver(fdf::DriverStartArgs start_args,
                           fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

  // fuchsia.hardware.test/MetadataSender implementation.
  void ServeMetadata(ServeMetadataCompleter::Sync& completer) override;
  void SetMetadata(SetMetadataRequest& request, SetMetadataCompleter::Sync& completer) override;
  void AddMetadataRetrieverNode(AddMetadataRetrieverNodeRequest& request,
                                AddMetadataRetrieverNodeCompleter::Sync& completer) override;
  void AddMetadataForwarderNode(AddMetadataForwarderNodeCompleter::Sync& completer) override;

 private:
  void Serve(fidl::ServerEnd<fuchsia_hardware_test::MetadataSender> request);

  // Helper function that adds a child node with the name |node_name| and properties
  // |node_properties|. The child node's node controller in `metadata_node_controllers_`.
  zx_status_t AddMetadataNode(std::string_view node_name,
                              const fuchsia_driver_framework::NodePropertyVector& node_properties);

  // Create a non-bindable child node whose purpose is to expose the
  // fuchsia.hardware.test/MetadataSender FIDL protocol served by this to /dev/.
  zx_status_t InitControllerNode();

  fidl::ServerBindingGroup<fuchsia_hardware_test::MetadataSender> bindings_;
  driver_devfs::Connector<fuchsia_hardware_test::MetadataSender> devfs_connector_{
      fit::bind_member<&MetadataSenderTestDriver::Serve>(this)};
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata> metadata_server_;
  bool offer_metadata_to_child_nodes_ = false;
#endif

  std::optional<fdf::OwnedChildNode> controller_node_;

  std::vector<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> metadata_node_controllers_;
};

}  // namespace fdf_metadata::test

#endif  // LIB_DRIVER_METADATA_CPP_TESTS_METADATA_SENDER_TEST_DRIVER_METADATA_SENDER_TEST_DRIVER_H_
