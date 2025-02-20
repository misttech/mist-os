// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.test/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/fake-platform-device/cpp/fake-pdev.h>
#include <lib/driver/metadata/cpp/metadata.h>
#include <lib/driver/metadata/cpp/metadata_server.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/lib/testing/predicates/status.h"

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

namespace fdf_metadata::test {

class MetadataServerTest : public gtest::TestLoopFixture {
 protected:
  void StartPlatformDevice() {
    ASSERT_FALSE(pdev_.has_value()) << "Platform device already started";
    ASSERT_OK(pdev_loop_.StartThread("pdev"));
    pdev_.emplace(pdev_loop_.dispatcher(), std::in_place);
    auto [client, server] = fidl::Endpoints<fuchsia_hardware_platform_device::Device>::Create();
    pdev_->SyncCall(&fdf_fake::FakePDev::Connect, std::move(server));
    pdev_client_.emplace(std::move(client));
  }

  void SetPlatformDeviceMetadata(fuchsia_hardware_test::Metadata metadata) {
    std::string metadata_id{fuchsia_hardware_test::Metadata::kSerializableName};
    pdev_->SyncCall(&fdf_fake::FakePDev::AddFidlMetadata<fuchsia_hardware_test::Metadata>,
                    metadata_id, std::move(metadata));
  }

  fidl::ClientEnd<fuchsia_hardware_platform_device::Device> TakePDevClient() {
    return std::move(pdev_client_).value();
  }

  void AssertHasMetadata(
      const fuchsia_hardware_test::Metadata& expected_metadata,
      fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata>& metadata_server) {
    auto [client, server] = fidl::Endpoints<fuchsia_driver_metadata::Metadata>::Create();
    fidl::ServerBinding binding{dispatcher(), std::move(server), &metadata_server,
                                fidl::kIgnoreBindingClosure};
    fidl::Client metadata_server_client{std::move(client), dispatcher()};
    metadata_server_client->GetMetadata().Then(
        [expected_metadata](
            fidl::Result<fuchsia_driver_metadata::Metadata::GetMetadata>& response) {
          ASSERT_TRUE(response.is_ok());
          fit::result persisted_metadata =
              fidl::Unpersist<fuchsia_hardware_test::Metadata>(response->metadata());
          ASSERT_TRUE(persisted_metadata.is_ok());
          ASSERT_EQ(persisted_metadata.value(), expected_metadata);
        });
    RunLoopUntilIdle();
  }

 private:
  async::Loop pdev_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  std::optional<async_patterns::TestDispatcherBound<fdf_fake::FakePDev>> pdev_;
  std::optional<fidl::ClientEnd<fuchsia_hardware_platform_device::Device>> pdev_client_;
};

// Verify that the metadata server can retrieve metadata from a platform device if the metadata
// exists.
TEST_F(MetadataServerTest, GetPDevMetadata) {
  const fuchsia_hardware_test::Metadata kMetadata{{.test_property = "test value"}};

  StartPlatformDevice();
  SetPlatformDeviceMetadata(kMetadata);
  auto pdev_client = TakePDevClient();

  // Verify that the metadata server can retrieve platform device metadata using a platform device
  // FIDL client.
  {
    fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata> metadata_server;
    zx::result result = metadata_server.SetMetadataFromPDevIfExists(pdev_client);
    ASSERT_OK(result);
    ASSERT_TRUE(result.value());
    AssertHasMetadata(kMetadata, metadata_server);
  }

  // Verify that the metadata server can retrieve platform device metadata using a `fdf::PDev`
  // instance.
  {
    fdf::PDev pdev{std::move(pdev_client)};
    fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata> metadata_server;
    zx::result result = metadata_server.SetMetadataFromPDevIfExists(pdev);
    ASSERT_OK(result);
    ASSERT_TRUE(result.value());
    AssertHasMetadata(kMetadata, metadata_server);
  }
}

// Verify that `MetadataServer::SetMetadataFromPDevIfExists()` returns false if the platform device
// does not have metadata.
TEST_F(MetadataServerTest, GetNonExistantPDevMetadata) {
  StartPlatformDevice();
  auto pdev_client = TakePDevClient();

  // Verify `MetadataServer::SetMetadataFromPDevIfExists()` returns false if the platform device
  // does not have metadata.
  {
    fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata> metadata_server;
    zx::result result = metadata_server.SetMetadataFromPDevIfExists(pdev_client);
    ASSERT_OK(result);
    ASSERT_FALSE(result.value());
  }

  // Verify `MetadataServer::SetMetadataFromPDevIfExists()` using a `fdf::PDev` instance returns
  // false if the platform device does not have metadata.
  {
    fdf::PDev pdev{std::move(pdev_client)};
    fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata> metadata_server;
    zx::result result = metadata_server.SetMetadataFromPDevIfExists(pdev);
    ASSERT_OK(result);
    ASSERT_FALSE(result.value());
  }
}

}  // namespace fdf_metadata::test

#endif
