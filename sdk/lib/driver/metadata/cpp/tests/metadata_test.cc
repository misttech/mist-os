// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.test/cpp/fidl.h>
#include <fidl/fuchsia.hardware.test/cpp/fidl.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver/metadata/cpp/tests/test_child/test_child.h>
#include <lib/driver/metadata/cpp/tests/test_grandchild/test_grandchild.h>
#include <lib/driver/metadata/cpp/tests/test_parent/test_parent.h>
#include <lib/driver/metadata/cpp/tests/test_root/test_root.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/fd.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

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
    ASSERT_EQ(realm_->component().Connect("dev-topological", endpoints->server.TakeChannel()),
              ZX_OK);

    ASSERT_EQ(fdio_fd_create(endpoints->client.TakeChannel().release(), &dev_fd_), ZX_OK);
  }

 protected:
  using ParentAndChild = std::pair<fidl::SyncClient<fuchsia_hardware_test::Parent>,
                                   fidl::SyncClient<fuchsia_hardware_test::Child>>;

  template <typename FidlProtocol>
  fidl::SyncClient<FidlProtocol> ConnectToNode(std::string_view path) const {
    zx::result channel = device_watcher::RecursiveWaitForFile(dev_fd_, std::string{path}.c_str());
    EXPECT_EQ(channel.status_value(), ZX_OK) << path;
    return fidl::SyncClient{fidl::ClientEnd<FidlProtocol>{std::move(channel.value())}};
  }

  // Connect to the fuchsia.hardware.test/Parent protocol served by the driver that is bound to the
  // |parent_node_name| node and connect to the fuchsia.hardware.test/Child protocol served by the
  // driver that is bound to the |child_node_name| node.
  ParentAndChild ConnectToParentAndChild(std::string_view parent_node_name,
                                         std::string_view child_node_name) const {
    auto path = std::string{parent_node_name}.append("/").append(child_node_name);
    auto parent = ConnectToNode<fuchsia_hardware_test::Parent>(path);
    path.append("/").append(TestChild::kChildNodeName);
    auto child = ConnectToNode<fuchsia_hardware_test::Child>(path);
    return {std::move(parent), std::move(child)};
  }

  fidl::SyncClient<fuchsia_hardware_test::Grandchild> ConnectToGrandchild(
      std::string_view parent_node_name, std::string_view child_node_name) const {
    auto path = std::string{parent_node_name}
                    .append("/")
                    .append(child_node_name)
                    .append("/")
                    .append(TestChild::kChildNodeName)
                    .append("/")
                    .append(TestGrandchild::kChildNodeName);
    return ConnectToNode<fuchsia_hardware_test::Grandchild>(path);
  }

 private:
  std::optional<component_testing::RealmRoot> realm_;
  int dev_fd_;
};

// Verify that `fdf::MetadataServer` can serve metadata from a node and that one of the node's child
// nodes can retrieve the metadata using `fdf::GetMetadata()`.
TEST_F(MetadataTest, TransferMetadata) {
  const char* kMetadataPropertyValue = "test property value";

  auto [parent, child] = ConnectToParentAndChild(TestRoot::kTestParentExposeNodeName,
                                                 TestParent::kTestChildUseNodeName);

  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result result = parent->SetMetadata(std::move(metadata));
    ASSERT_TRUE(result.is_ok()) << result.error_value();
  }

  {
    fidl::Result result = child->GetMetadata();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    auto metadata = std::move(result.value().metadata());
    ASSERT_EQ(metadata.test_property(), kMetadataPropertyValue);
  }
}

// Verify that a driver can forward metadata using `fdf::MetadataServer::ForwardMetadata()`.
TEST_F(MetadataTest, ForwardMetadata) {
  const char* kMetadataPropertyValue = "test property value";

  auto [parent, child] = ConnectToParentAndChild(TestRoot::kTestParentExposeNodeName,
                                                 TestParent::kTestChildUseNodeName);
  auto grandchild =
      ConnectToGrandchild(TestRoot::kTestParentExposeNodeName, TestParent::kTestChildUseNodeName);

  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result result = parent->SetMetadata(std::move(metadata));
    ASSERT_TRUE(result.is_ok()) << result.error_value();
  }

  {
    fidl::Result result = child->ForwardMetadata();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
  }

  {
    fidl::Result result = grandchild->GetMetadata();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    auto metadata = std::move(result.value().metadata());
    ASSERT_EQ(metadata.test_property(), kMetadataPropertyValue);
  }
}

// Verify that a driver is unable to retrieve metadata offered to it by the node it is bound to via
// `fdf::GetMetadata()` if the driver does not specify in its component manifest that it uses the
// service associated with the metadata.
TEST_F(MetadataTest, FailMetadataTransferWithExposeAndNoUse) {
  const char* kMetadataPropertyValue = "test property value";
  auto [parent, child] = ConnectToParentAndChild(TestRoot::kTestParentExposeNodeName,
                                                 TestParent::kTestChildNoUseNodeName);

  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result result = parent->SetMetadata(std::move(metadata));
    ASSERT_TRUE(result.is_ok()) << result.error_value();
  }

  {
    fidl::Result result = child->GetMetadata();
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error());
    ASSERT_EQ(result.error_value().domain_error(), ZX_ERR_PEER_CLOSED);
  }
}

// Verify that a driver is unable to retrieve metadata offered to it by the node it is bound to via
// `fdf::GetMetadata()` if the driver of the parent node does not specify in its component manifest
// that it offers the service associated with the metadata.
TEST_F(MetadataTest, FailMetadataTransferWithNoExposeButUse) {
  const char* kMetadataPropertyValue = "test property value";
  auto [parent, child] = ConnectToParentAndChild(TestRoot::kTestParentNoExposeNodeName,
                                                 TestParent::kTestChildUseNodeName);

  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result result = parent->SetMetadata(std::move(metadata));
    ASSERT_TRUE(result.is_ok()) << result.error_value();
  }

  {
    fidl::Result result = child->GetMetadata();
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error());
    ASSERT_EQ(result.error_value().domain_error(), ZX_ERR_PEER_CLOSED);
  }

  fidl::Result result = child->GetMetadata();
}

// Verify that a driver is unable to retrieve metadata offered to it by the node it is bound to via
// `fdf::GetMetadata()` if the driver does not specify in its component manifest that it uses the
// service associated with the metadata and the driver of the parent node does not specify in its
// component manifest that is offers the service associated with the metadata.
TEST_F(MetadataTest, FailMetadataTransferWithNoExposeAndNoUse) {
  const char* kMetadataPropertyValue = "test property value";
  auto [parent, child] = ConnectToParentAndChild(TestRoot::kTestParentNoExposeNodeName,
                                                 TestParent::kTestChildNoUseNodeName);

  {
    fuchsia_hardware_test::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    fidl::Result result = parent->SetMetadata(std::move(metadata));
    ASSERT_TRUE(result.is_ok()) << result.error_value();
  }

  {
    fidl::Result result = child->GetMetadata();
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error());
    ASSERT_EQ(result.error_value().domain_error(), ZX_ERR_PEER_CLOSED);
  }

  fidl::Result result = child->GetMetadata();
}

}  // namespace fdf_metadata::test
