// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.test/cpp/fidl.h>
#include <fidl/fuchsia.examples.metadata/cpp/fidl.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/fd.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/lib/testing/predicates/status.h"

namespace metadata_test {

class MetadataTest : public gtest::TestLoopFixture {
 public:
  void SetUp() override {
    // Create and build the realm.
    auto realm_builder = component_testing::RealmBuilder::Create();
    driver_test_realm::Setup(realm_builder);
    realm_.emplace(realm_builder.Build(dispatcher()));

    // Start DriverTestRealm.
    zx::result result = realm_->component().Connect<fuchsia_driver_test::Realm>();
    ASSERT_OK(result);
    fidl::SyncClient<fuchsia_driver_test::Realm> driver_test_realm{std::move(result.value())};
    fidl::Result start_result = driver_test_realm->Start(
        fuchsia_driver_test::RealmArgs{{.root_driver = "fuchsia-boot:///dtr#meta/sender.cm"}});
    ASSERT_OK(start_result);

    // Open /dev directory.
    auto [client_end, server_end] = fidl::Endpoints<fuchsia_io::Directory>::Create();
    ASSERT_OK(realm_->component().exposed()->Open("dev-topological", fuchsia::io::PERM_READABLE, {},
                                                  server_end.TakeChannel()));
    ASSERT_OK(fdio_fd_create(client_end.TakeChannel().release(), &dev_fd_));
  }

 protected:
  template <typename FidlProtocol>
  fidl::SyncClient<FidlProtocol> ConnectToNode(std::string_view path) const {
    zx::result channel = device_watcher::RecursiveWaitForFile(dev_fd_, std::string{path}.c_str());
    EXPECT_OK(channel) << path;
    return fidl::SyncClient{fidl::ClientEnd<FidlProtocol>{std::move(channel.value())}};
  }

 private:
  std::optional<component_testing::RealmRoot> realm_;
  int dev_fd_;
};

TEST_F(MetadataTest, TransferMetadata) {
  const char* kMetadataPropertyValue = "test property value";

  auto sender = ConnectToNode<fuchsia_examples_metadata::Sender>("sender");
  auto forwarder = ConnectToNode<fuchsia_examples_metadata::Forwarder>("sender/forwarder");
  auto retriever =
      ConnectToNode<fuchsia_examples_metadata::Retriever>("sender/forwarder/retriever");

  // Set the metadata of the `sender` driver and offer it to its child driver
  // (the `forwarder` driver).
  {
    fuchsia_examples_metadata::Metadata metadata{{.test_property = kMetadataPropertyValue}};
    ASSERT_OK(sender->SetMetadata(std::move(metadata)));
  }

  // Make the `forwarder` driver retrieve metadata from its parent driver (the
  // `sender` driver) and offer it to its child driver (the `retriever` driver).
  ASSERT_OK(forwarder->ForwardMetadata());

  // Retrieve the metadata from `retriever`'s parent driver, `forwarder`.
  // This verifies that:
  //   * The `sender` driver sent the correct metadata.
  //   * The `forwarder` driver forwarded the correct metadata.
  //   * The `retriever` driver retrieved the correct metadata.
  {
    fidl::Result result = retriever->GetMetadata();
    ASSERT_OK(result);
    auto metadata = std::move(result.value().metadata());
    ASSERT_EQ(metadata.test_property(), kMetadataPropertyValue);
  }
}

}  // namespace metadata_test
