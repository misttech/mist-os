// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.test/cpp/fidl.h>
#include <fidl/fuchsia.hardware.test/cpp/fidl.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver/metadata/cpp/tests/metadata_forwarder_test_driver/metadata_forwarder_test_driver.h>
#include <lib/driver/metadata/cpp/tests/metadata_retriever_test_driver/metadata_retriever_test_driver.h>
#include <lib/driver/metadata/cpp/tests/metadata_sender_test_driver/metadata_sender_test_driver.h>
#include <lib/driver/metadata/cpp/tests/test_root/test_root.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/fd.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

namespace fdf_metadata::test {

class MetadataTest : public gtest::TestLoopFixture {
 public:
  void SetUp() override {
    // Create and build the realm.
    auto realm_builder = component_testing::RealmBuilder::Create();
    driver_test_realm::Setup(realm_builder);
    realm_.emplace(realm_builder.Build(dispatcher()));

    // Start DriverTestRealm.
    zx::result result = realm_->component().Connect<fuchsia_driver_test::Realm>();
    ASSERT_EQ(result.status_value(), ZX_OK);
    fidl::SyncClient<fuchsia_driver_test::Realm> driver_test_realm{std::move(result.value())};
    fidl::Result start_result = driver_test_realm->Start(
        fuchsia_driver_test::RealmArgs{{.root_driver = "fuchsia-boot:///dtr#meta/test_root.cm"}});
    ASSERT_TRUE(start_result.is_ok()) << start_result.error_value();

    // Connect to /dev directory.
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(endpoints.status_value(), ZX_OK);
    ASSERT_EQ(realm_->component().exposed()->Open("dev-topological", fuchsia::io::PERM_READABLE, {},
                                                  endpoints->server.TakeChannel()),
              ZX_OK);
    ASSERT_EQ(fdio_fd_create(endpoints->client.TakeChannel().release(), &dev_fd_), ZX_OK);

    root_.emplace(ConnectToNode<fuchsia_hardware_test::Root>(TestRootDriver::kControllerNodeName));
  }

 protected:
  fidl::SyncClient<fuchsia_hardware_test::MetadataRetriever> ConnectToMetadataRetriever(
      std::string_view metadata_retriever_devfs_path) const {
    auto controller_path = std::string{metadata_retriever_devfs_path}.append("/").append(
        MetadataRetrieverTestDriver::kControllerNodeName);
    return ConnectToNode<fuchsia_hardware_test::MetadataRetriever>(controller_path);
  }

  fidl::SyncClient<fuchsia_hardware_test::MetadataForwarder> ConnectToMetadataForwarder(
      std::string_view metadata_forwarder_devfs_path) const {
    auto controller_path = std::string{metadata_forwarder_devfs_path}.append("/").append(
        MetadataForwarderTestDriver::kControllerNodeName);
    return ConnectToNode<fuchsia_hardware_test::MetadataForwarder>(controller_path);
  }

  fidl::SyncClient<fuchsia_hardware_test::MetadataSender> ConnectToMetadataSender(
      std::string_view metadata_sender_devfs_path) const {
    auto controller_path = std::string{metadata_sender_devfs_path}.append("/").append(
        MetadataSenderTestDriver::kControllerNodeName);
    return ConnectToNode<fuchsia_hardware_test::MetadataSender>(controller_path);
  }

  fidl::SyncClient<fuchsia_hardware_test::Root>& root() { return root_.value(); }

  // Create a metadata_sender driver instance and have it create a node for the metadata_retriever
  // driver to bind to.
  //
  // |expose| determines if the metadata_sender driver will expose the metadata
  // FIDL service in its component manifest.

  // |use| determines if the metadata_retriever driver
  // declares that it uses the metadata FIDL service in its component manifest. Return a client for
  // the metadata_sender driver and metadata_retriever driver.
  //
  // If |serve_metadata| is true then the metadata_sender driver will serve metadata to its child
  // nodes.
  std::pair<fidl::SyncClient<fuchsia_hardware_test::MetadataSender>,
            fidl::SyncClient<fuchsia_hardware_test::MetadataRetriever>>
  CreateMetadataSenderAndRetriever(bool expose, bool use, bool serve_metadata) {
    // Create metadata_sender driver instance.
    std::string metadata_sender_node_path;
    fidl::SyncClient<fuchsia_hardware_test::MetadataSender> metadata_sender;
    {
      fidl::Result result =
          root()->AddMetadataSenderNode({{.exposes_metadata_fidl_service = expose}});
      EXPECT_TRUE(result.is_ok());
      metadata_sender_node_path = std::move(result.value().child_node_name());
      metadata_sender = ConnectToMetadataSender(metadata_sender_node_path);
    }

    if (serve_metadata) {
      fidl::Result result = metadata_sender->ServeMetadata();
      EXPECT_TRUE(result.is_ok());
    }

    // Create a metadata_retriever driver instance as a child of the metadata_sender driver.
    fidl::SyncClient<fuchsia_hardware_test::MetadataRetriever> metadata_retriever;
    {
      fidl::Result result =
          metadata_sender->AddMetadataRetrieverNode({{.uses_metadata_fidl_service = use}});
      EXPECT_TRUE(result.is_ok());
      std::string metadata_retriever_node_path =
          metadata_sender_node_path.append("/").append(result.value().child_node_name());
      metadata_retriever = ConnectToMetadataRetriever(metadata_retriever_node_path);
    }

    return std::pair<fidl::SyncClient<fuchsia_hardware_test::MetadataSender>,
                     fidl::SyncClient<fuchsia_hardware_test::MetadataRetriever>>(
        std::move(metadata_sender), std::move(metadata_retriever));
  }

 private:
  // Connect to the |FidlProtocol| FIDL protocol found at |devfs_path| within devfs.
  template <typename FidlProtocol>
  fidl::SyncClient<FidlProtocol> ConnectToNode(std::string_view devfs_path) const {
    zx::result channel =
        device_watcher::RecursiveWaitForFile(dev_fd_, std::string{devfs_path}.c_str());
    EXPECT_EQ(channel.status_value(), ZX_OK) << devfs_path;
    return fidl::SyncClient{fidl::ClientEnd<FidlProtocol>{std::move(channel.value())}};
  }

  std::optional<component_testing::RealmRoot> realm_;
  int dev_fd_;
  std::optional<fidl::SyncClient<fuchsia_hardware_test::Root>> root_;
};

// Verify that `fdf_metadata::MetadataServer` can serve metadata from a node and that one of the
// node's child nodes can retrieve the metadata using `fdf::GetMetadata()`.
TEST_F(MetadataTest, SendAndRetrieveMetadata) {
  static const std::string kMetadataPropertyValue = "arbitrary";

  auto [metadata_sender, metadata_retriever] = CreateMetadataSenderAndRetriever(true, true, true);

  // Set metadata_sender driver's metadata.
  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result result = metadata_sender->SetMetadata(std::move(metadata));
    ASSERT_TRUE(result.is_ok());
  }

  // Make the metadata_retriever get metadata from its parent (which is metadata_sender). Verify
  // that the metadata is the same as the metadata assigned to the metadata_sender driver instance.
  {
    fidl::Result result = metadata_retriever->GetMetadata();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    auto metadata = std::move(result.value().metadata());
    ASSERT_EQ(metadata.test_property(), kMetadataPropertyValue);
  }
}

// Verify that a driver can retrieve metadata with `fdf_metadata::GetMetadataIfExists()`.
TEST_F(MetadataTest, GetMetadataIfExists) {
  static const std::string kMetadataPropertyValue = "arbitrary";

  auto [metadata_sender, metadata_retriever] = CreateMetadataSenderAndRetriever(true, true, true);

  // Set metadata_sender driver's metadata.
  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result result = metadata_sender->SetMetadata(std::move(metadata));
    ASSERT_TRUE(result.is_ok());
  }

  // Make the metadata_retriever get metadata from its parent (which is metadata_sender). Verify
  // that the metadata is the same as the metadata assigned to the metadata_sender driver instance.
  {
    fidl::Result result = metadata_retriever->GetMetadataIfExists();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().retrieved_metadata());
    auto metadata = std::move(result.value().metadata());
    ASSERT_EQ(metadata.test_property(), kMetadataPropertyValue);
  }
}

// Verify that `fdf_metadata::GetMetadataIfExists()` will return std::nullopt instead of an error
// when there are no metadata servers exposed in the calling driver's incoming namespace.
TEST_F(MetadataTest, GetMetadataIfExistsNullopt) {
  auto [metadata_sender, metadata_retriever] = CreateMetadataSenderAndRetriever(true, true, false);

  // Make the metadata_retriever get metadata from its parent (which is metadata_sender). Verify
  // that the metadata is the same as the metadata assigned to the metadata_sender driver instance.
  {
    fidl::Result result = metadata_retriever->GetMetadataIfExists();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_FALSE(result.value().retrieved_metadata());
  }
}

// Verify that a driver can forward metadata using
// `fdf_metadata::MetadataServer::ForwardMetadata()`.
TEST_F(MetadataTest, ForwardMetadata) {
  static const std::string kMetadataPropertyValue = "arbitrary";

  // Create a metadata_sender driver instance.
  std::string metadata_sender_node_path;
  fidl::SyncClient<fuchsia_hardware_test::MetadataSender> metadata_sender;
  {
    fidl::Result result = root()->AddMetadataSenderNode({{.exposes_metadata_fidl_service = true}});
    ASSERT_TRUE(result.is_ok());
    metadata_sender_node_path = std::move(result.value().child_node_name());
    metadata_sender = ConnectToMetadataSender(metadata_sender_node_path);
  }

  // Set and serve metadata_sender driver's metadata.
  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result set_result = metadata_sender->SetMetadata(std::move(metadata));
    ASSERT_TRUE(set_result.is_ok());

    fidl::Result serve_result = metadata_sender->ServeMetadata();
    EXPECT_TRUE(serve_result.is_ok());
  }

  // Create a metadata_forwarder driver instance as a child of the metadata_sender driver. The
  // metadata_forwarder driver will spawn a child node that the metadata_retriever driver binds
  // to.
  fidl::SyncClient<fuchsia_hardware_test::MetadataForwarder> metadata_forwarder;
  fidl::SyncClient<fuchsia_hardware_test::MetadataRetriever> metadata_retriever;
  {
    fidl::Result result = metadata_sender->AddMetadataForwarderNode();
    ASSERT_TRUE(result.is_ok());

    std::string metadata_forwarder_node_path =
        std::string{metadata_sender_node_path}.append("/").append(result.value().child_node_name());
    metadata_forwarder = ConnectToMetadataForwarder(metadata_forwarder_node_path);

    std::string metadata_retriever_node_path =
        std::string{metadata_forwarder_node_path}.append("/").append(
            MetadataForwarderTestDriver::kMetadataRetrieverNodeName);
    metadata_retriever = ConnectToMetadataRetriever(metadata_retriever_node_path);
  }

  // Make the metadata_forwarder driver forward the metadata from its parent driver (which is
  // metadata_sender) to its child node (which is bound to metadata_retriever).
  {
    fidl::Result result = metadata_forwarder->ForwardMetadata();
    ASSERT_TRUE(result.is_ok());
  }

  // Make the metadata_retriever get metadata from its parent which is a metadata_sender driver.
  // Verify that the metadata is the same as the metadata assigned to the metadata_sender
  // driver.
  {
    fidl::Result result = metadata_retriever->GetMetadata();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    auto metadata = std::move(result.value().metadata());
    ASSERT_EQ(metadata.test_property(), kMetadataPropertyValue);
  }
}

// Verify that a driver is unable to retrieve metadata via `fdf::GetMetadata()` if the driver does
// not specify in its component manifest that it uses the metadata FIDL service.
TEST_F(MetadataTest, FailMetadataTransferWithExposeAndNoUse) {
  static const std::string kMetadataPropertyValue = "arbitrary";

  auto [metadata_sender, metadata_retriever] = CreateMetadataSenderAndRetriever(true, false, true);

  // Set metadata_sender driver's metadata.
  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result result = metadata_sender->SetMetadata(std::move(metadata));
    ASSERT_TRUE(result.is_ok());
  }

  // Make the metadata_retriever driver fail to retrieve metadata.
  {
    fidl::Result result = metadata_retriever->GetMetadata();
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error());
    ASSERT_EQ(result.error_value().domain_error(), ZX_ERR_PEER_CLOSED);
  }
}

// Verify that a driver is unable to retrieve metadata via `fdf::GetMetadata()` if the parent driver
// does not expose the metadata FIDL service in its component manifest.
TEST_F(MetadataTest, FailMetadataTransferWithNoExposeButUse) {
  static const std::string kMetadataPropertyValue = "arbitrary";

  auto [metadata_sender, metadata_retriever] = CreateMetadataSenderAndRetriever(false, true, true);

  // Set metadata_sender driver's metadata.
  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result result = metadata_sender->SetMetadata(std::move(metadata));
    ASSERT_TRUE(result.is_ok());
  }

  // Make the metadata_retriever driver fail to retrieve metadata.
  {
    fidl::Result result = metadata_retriever->GetMetadata();
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error());
    ASSERT_EQ(result.error_value().domain_error(), ZX_ERR_PEER_CLOSED);
  }
}

// Verify that a driver is unable to retrieve metadata via `fdf::GetMetadata()` if the driver does
// not specify in its component manifest that it uses the metadata FIDL service and the parent
// driver does not expose the metadata FIDL service in its component manifest.
TEST_F(MetadataTest, FailMetadataTransferWithNoExposeAndNoUse) {
  static const std::string kMetadataPropertyValue = "arbitrary";

  auto [metadata_sender, metadata_retriever] = CreateMetadataSenderAndRetriever(false, false, true);

  // Set metadata_sender driver's metadata.
  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result result = metadata_sender->SetMetadata(std::move(metadata));
    ASSERT_TRUE(result.is_ok());
  }

  // Make the metadata_retriever driver fail to retrieve metadata.
  {
    fidl::Result result = metadata_retriever->GetMetadata();
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error());
    ASSERT_EQ(result.error_value().domain_error(), ZX_ERR_PEER_CLOSED);
  }
}

}  // namespace fdf_metadata::test

#endif
