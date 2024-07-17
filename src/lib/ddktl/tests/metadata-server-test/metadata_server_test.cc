// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.test/cpp/fidl.h>
#include <fidl/fuchsia.hardware.test/cpp/fidl.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/fd.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace ddk::test {

class MetadataServerTest : public gtest::TestLoopFixture {
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
    fidl::Result start_result = driver_test_realm->Start(fuchsia_driver_test::RealmArgs{
        {.root_driver = "fuchsia-boot:///dtr#meta/metadata_sender_test_driver.cm"}});
    ASSERT_TRUE(start_result.is_ok()) << start_result.error_value();

    // Connect to /dev directory.
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(endpoints.status_value(), ZX_OK);
    ASSERT_EQ(realm_->component().Connect("dev-topological", endpoints->server.TakeChannel()),
              ZX_OK);

    ASSERT_EQ(fdio_fd_create(endpoints->client.TakeChannel().release(), &dev_fd_), ZX_OK);
  }

 protected:
  // Connect to the |FidlProtocol| FIDL protocol found at "/dev/|path|".
  template <typename FidlProtocol>
  fidl::SyncClient<FidlProtocol> ConnectToNode(std::string_view path) const {
    zx::result channel = device_watcher::RecursiveWaitForFile(dev_fd_, std::string{path}.c_str());
    EXPECT_EQ(channel.status_value(), ZX_OK) << path;
    return fidl::SyncClient{fidl::ClientEnd<FidlProtocol>{std::move(channel.value())}};
  }

 private:
  std::optional<component_testing::RealmRoot> realm_;
  int dev_fd_;
};

// Verify that `ddk::MetadataServer` can serve metadata from a device. Verify that one of the
// device's child device can retrieve that metadata and forward it to its children using
// `ddk::MetadataServer::ForwardMetadata()`. Lastly, verify that one of the grandchildren can
// retrieve that metadata using `ddk::GetMetadata()`.
TEST_F(MetadataServerTest, TransferMetadata) {
  const char* kMetadataPropertyValue = "test property value";
  const std::string kMetadataSenderTopologicalPath{"metadata_sender"};
  const std::string kMetadataForwarderTopologicalPath =
      kMetadataSenderTopologicalPath + "/metadata_forwarder";
  const std::string kMetadataRetrieverTopologicalPath =
      kMetadataForwarderTopologicalPath + "/metadata_retriever";

  fidl::SyncClient metadata_sender =
      ConnectToNode<fuchsia_hardware_test::MetadataSender>(kMetadataSenderTopologicalPath);
  fidl::SyncClient metadata_forwarder =
      ConnectToNode<fuchsia_hardware_test::MetadataForwarder>(kMetadataForwarderTopologicalPath);
  fidl::SyncClient metadata_retriever =
      ConnectToNode<fuchsia_hardware_test::MetadataRetriever>(kMetadataRetrieverTopologicalPath);

  // Verify that the "metadata_forwarder" driver fails to forward metadata from "metadata_sender" to
  // "metadata_retriever" when "metadata_sender" has not set its metadata.
  {
    fidl::Result result = metadata_forwarder->ForwardMetadata();
    ASSERT_TRUE(result.is_error());
  }

  // Serve the metadata to "metadata_forwarder" child device using `ddk::MetadataServer::Serve()`.
  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result result = metadata_sender->SetMetadata(std::move(metadata));
    ASSERT_TRUE(result.is_ok()) << result.error_value();
  }

  // Verify that the "metadata_retriever" driver fails to retrieve metadata from the
  // "metadata_forwarder" driver when "metadata_forwarder" has not set its metadata.
  {
    fidl::Result result = metadata_retriever->GetMetadata();
    ASSERT_TRUE(result.is_error());
  }

  // Forward the metadata from the "metadata_sender" device to the "metadata_retriever" device using
  // `ddk::MetadataServer::ForwardMetadata()`.
  {
    fidl::Result result = metadata_forwarder->ForwardMetadata();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
  }

  // Make the "metadata_retreiver" driver retrieve the metadata from the "metadata_forwarder" device
  // using `ddk::GetMetadata()`.
  {
    fidl::Result result = metadata_retriever->GetMetadata();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    fuchsia_hardware_test::Metadata metadata = std::move(result.value().metadata());
    ASSERT_EQ(metadata.test_property(), kMetadataPropertyValue);
  }
}

}  //  namespace ddk::test
