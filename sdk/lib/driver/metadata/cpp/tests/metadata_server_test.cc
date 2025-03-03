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
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>

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
    metadata_server_client->GetPersistedMetadata().Then(
        [expected_metadata](
            fidl::Result<fuchsia_driver_metadata::Metadata::GetPersistedMetadata>& response) {
          ASSERT_TRUE(response.is_ok());
          fit::result persisted_metadata =
              fidl::Unpersist<fuchsia_hardware_test::Metadata>(response->persisted_metadata());
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
TEST_F(MetadataServerTest, GetNonExistentPDevMetadata) {
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

class ForwardMetadataTest : public gtest::TestLoopFixture {
 protected:
  void InitIncomingNamespace(bool start_metadata_server) {
    if (start_metadata_server) {
      incoming_namespace_.SyncCall(&IncomingNamespace::StartMetadataServer);
    }

    auto [client, server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
    incoming_namespace_.SyncCall(&IncomingNamespace::Serve, std::move(server));

    std::vector<fuchsia_component_runner::ComponentNamespaceEntry> namespace_entries;
    namespace_entries.emplace_back(fuchsia_component_runner::ComponentNamespaceEntry{
        {.path = "/", .directory = std::move(client)}});
    zx::result incoming = fdf::Namespace::Create(namespace_entries);
    ASSERT_OK(incoming);
    incoming_ = std::make_shared<fdf::Namespace>(std::move(incoming.value()));
  }

  void SetIncomingMetadata(fuchsia_hardware_test::Metadata metadata) {
    incoming_namespace_.SyncCall(&IncomingNamespace::SetMetadata, std::move(metadata));
  }

  const std::shared_ptr<fdf::Namespace>& incoming() { return incoming_; }

  void AssertHasMetadata(
      const fuchsia_hardware_test::Metadata& expected_metadata,
      fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata>& metadata_server) {
    auto [client, server] = fidl::Endpoints<fuchsia_driver_metadata::Metadata>::Create();
    fidl::ServerBinding binding{dispatcher(), std::move(server), &metadata_server,
                                fidl::kIgnoreBindingClosure};
    fidl::Client metadata_server_client{std::move(client), dispatcher()};
    metadata_server_client->GetPersistedMetadata().Then(
        [expected_metadata](
            fidl::Result<fuchsia_driver_metadata::Metadata::GetPersistedMetadata>& response) {
          ASSERT_TRUE(response.is_ok());
          fit::result persisted_metadata =
              fidl::Unpersist<fuchsia_hardware_test::Metadata>(response->persisted_metadata());
          ASSERT_TRUE(persisted_metadata.is_ok());
          ASSERT_EQ(persisted_metadata.value(), expected_metadata);
        });
    RunLoopUntilIdle();
  }

  void AssertDoesNotHaveMetadata(
      fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata>& metadata_server) {
    auto [client, server] = fidl::Endpoints<fuchsia_driver_metadata::Metadata>::Create();
    fidl::ServerBinding binding{dispatcher(), std::move(server), &metadata_server,
                                fidl::kIgnoreBindingClosure};
    fidl::Client metadata_server_client{std::move(client), dispatcher()};
    metadata_server_client->GetPersistedMetadata().Then(
        [](fidl::Result<fuchsia_driver_metadata::Metadata::GetPersistedMetadata>& response) {
          ASSERT_TRUE(response.is_error());
          ASSERT_TRUE(response.error_value().is_domain_error());
          ASSERT_EQ(response.error_value().domain_error(), ZX_ERR_NOT_FOUND);
        });
    RunLoopUntilIdle();
  }

 private:
  class IncomingNamespace {
   public:
    void Serve(fidl::ServerEnd<fuchsia_io::Directory> server) {
      ASSERT_OK(outgoing_.Serve(std::move(server)));
    }

    void SetMetadata(const fuchsia_hardware_test::Metadata& metadata) {
      ASSERT_TRUE(metadata_server_.has_value());
      ASSERT_OK(metadata_server_->SetMetadata(metadata));
    }

    void StartMetadataServer() {
      ASSERT_FALSE(metadata_server_.has_value());
      metadata_server_.emplace();
      ASSERT_OK(
          metadata_server_->Serve(outgoing_, fdf::Dispatcher::GetCurrent()->async_dispatcher()));
    }

   private:
    std::optional<MetadataServer<fuchsia_hardware_test::Metadata>> metadata_server_;
    fdf::OutgoingDirectory outgoing_;
  };

  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher background_driver_dispatcher_ =
      runtime_.StartBackgroundDispatcher();
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_namespace_{
      background_driver_dispatcher_->async_dispatcher(), std::in_place};
  std::shared_ptr<fdf::Namespace> incoming_;

  // Sets the global logger instance which is needed by `fdf_metadata::MetadataServer` in order to
  // make `FDF_LOG` statements.
  fdf_testing::ScopedGlobalLogger logger_;
};

// Verify that the metadata server can forward metadata using
// `fdf_metadata::MetadataServer::ForwardMetadata()`.
TEST_F(ForwardMetadataTest, ForwardMetadata) {
  const fuchsia_hardware_test::Metadata kMetadata{{.test_property = "test value"}};

  InitIncomingNamespace(true);
  SetIncomingMetadata(kMetadata);

  fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata> metadata_server;
  ASSERT_OK(metadata_server.ForwardMetadata(incoming()));
  AssertHasMetadata(kMetadata, metadata_server);
}

// Verify that the metadata server can forward existing metadata using
// `fdf_metadata::MetadataServer::ForwardMetadataIfExists()`.
TEST_F(ForwardMetadataTest, ForwardExistingMetadata) {
  const fuchsia_hardware_test::Metadata kMetadata{{.test_property = "test value"}};

  InitIncomingNamespace(true);
  SetIncomingMetadata(kMetadata);

  fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata> metadata_server;
  zx::result result = metadata_server.ForwardMetadataIfExists(incoming());
  ASSERT_OK(result);
  ASSERT_TRUE(result.value());
  AssertHasMetadata(kMetadata, metadata_server);
}

// Verify that `fdf_metadata::MetadataServer::ForwardMetadataIfExists()` returns false if the
// incoming metadata server does not have metadata.
TEST_F(ForwardMetadataTest, ForwardNonExistentMetadata) {
  InitIncomingNamespace(true);

  fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata> metadata_server;
  zx::result result = metadata_server.ForwardMetadataIfExists(incoming());
  ASSERT_OK(result);
  ASSERT_FALSE(result.value());
  AssertDoesNotHaveMetadata(metadata_server);
}

// Verify that `fdf_metadata::MetadataServer::ForwardMetadataIfExists()` returns false if the
// incoming metadata server does not exist.
TEST_F(ForwardMetadataTest, ForwardNonExistentMetadataServer) {
  InitIncomingNamespace(false);

  fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata> metadata_server;
  zx::result result = metadata_server.ForwardMetadataIfExists(incoming());
  ASSERT_OK(result);
  ASSERT_FALSE(result.value());
  AssertDoesNotHaveMetadata(metadata_server);
}

}  // namespace fdf_metadata::test

#endif
