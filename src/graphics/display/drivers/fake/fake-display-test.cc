// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/fake-display.h"

#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <zircon/errors.h>
#include <zircon/pixelformat.h>
#include <zircon/rights.h>

#include <memory>

#include <zxtest/zxtest.h>

#include "src/devices/sysmem/drivers/sysmem/device.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/drivers/fake/mock-display-device-tree.h"
#include "src/graphics/display/drivers/fake/sysmem-device-wrapper.h"

namespace fake_display {

class FakeDisplayTest : public zxtest::Test {
 public:
  FakeDisplayTest() = default;

  void SetUp() override {
    std::shared_ptr<zx_device> mock_root = MockDevice::FakeRootParent();
    auto sysmem = std::make_unique<display::GenericSysmemDeviceWrapper<sysmem_driver::Device>>(
        mock_root.get());
    tree_ =
        std::make_unique<display::MockDisplayDeviceTree>(std::move(mock_root), std::move(sysmem),
                                                         /*start_vsync=*/false);
  }

  void TearDown() override {
    tree_->AsyncShutdown();
    tree_.reset();
  }

  fake_display::FakeDisplay* display() { return tree_->display(); }

  const fidl::WireSyncClient<fuchsia_sysmem2::DriverConnector>& sysmem_fidl() {
    return tree_->sysmem_client();
  }

 private:
  std::unique_ptr<display::MockDisplayDeviceTree> tree_;
};

class FakeDisplaySysmemTest : public FakeDisplayTest {
 public:
  FakeDisplaySysmemTest() = default;
  ~FakeDisplaySysmemTest() override = default;

  void SetUp() override {
    FakeDisplayTest::SetUp();

    zx::result sysmem_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
    ASSERT_OK(sysmem_endpoints);
    auto& [sysmem_client, sysmem_server] = sysmem_endpoints.value();
    EXPECT_TRUE(sysmem_fidl()->ConnectV1(std::move(sysmem_server)).ok());
    sysmem_ = fidl::WireSyncClient(std::move(sysmem_client));

    zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
    ASSERT_OK(token_endpoints);
    auto& [token_client, token_server] = token_endpoints.value();
    fidl::Status allocate_token_status = sysmem_->AllocateSharedCollection(std::move(token_server));
    ASSERT_TRUE(allocate_token_status.ok());
    token_client_ = std::move(token_client);

    EXPECT_TRUE(fidl::WireCall(token_client_)->Sync().ok());

    // At least one sysmem participant should specify buffer memory constraints.
    // The driver may not specify buffer memory constraints, so the test should
    // always provide one for sysmem through `buffer_collection_` client.
    //
    // Here we duplicate the token to set buffer collection constraints in
    // the test.
    std::vector<zx_rights_t> rights = {ZX_RIGHT_SAME_RIGHTS};
    fidl::WireResult duplicate_result =
        fidl::WireCall(token_client_)
            ->DuplicateSync(fidl::VectorView<zx_rights_t>::FromExternal(rights));
    ASSERT_TRUE(duplicate_result.ok());
    auto& duplicate_value = duplicate_result.value();
    ASSERT_EQ(duplicate_value.tokens.count(), 1u);

    // Bind duplicated token to BufferCollection client.
    zx::result collection_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollection>();
    ASSERT_OK(collection_endpoints);
    auto& [collection_client, collection_server] = collection_endpoints.value();
    fidl::Status bind_status = sysmem_->BindSharedCollection(std::move(duplicate_value.tokens[0]),
                                                             std::move(collection_server));
    EXPECT_TRUE(bind_status.ok());
    buffer_collection_ = fidl::WireSyncClient(std::move(collection_client));
  }

  void TearDown() override {
    EXPECT_TRUE(buffer_collection_->Close().ok());
    buffer_collection_ = {};
    sysmem_ = {};
    FakeDisplayTest::TearDown();
  }

  const fidl::WireSyncClient<fuchsia_sysmem::Allocator>& sysmem() const { return sysmem_; }
  const fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>& buffer_collection() const {
    return buffer_collection_;
  }
  fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken> take_token_client() {
    return std::move(token_client_);
  }

 private:
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_;
  fidl::WireSyncClient<fuchsia_sysmem::BufferCollection> buffer_collection_;

  fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken> token_client_;
};

TEST_F(FakeDisplaySysmemTest, ImportBufferCollection) {
  // Test ImportBufferCollection().
  constexpr uint64_t kValidBufferCollectionId = 1u;
  EXPECT_OK(display()->DisplayControllerImplImportBufferCollection(
      kValidBufferCollectionId, take_token_client().TakeChannel()));

  // `collection_id` must be unused.
  zx::result another_token_endpoints =
      fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_OK(another_token_endpoints);
  EXPECT_EQ(display()->DisplayControllerImplImportBufferCollection(
                kValidBufferCollectionId, another_token_endpoints->client.TakeChannel()),
            ZX_ERR_ALREADY_EXISTS);

  // Driver sets BufferCollection buffer memory constraints.
  const image_t kDefaultConfig = {
      .width = 1024,
      .height = 768,
      .pixel_format = ZX_PIXEL_FORMAT_ARGB_8888,
      .type = IMAGE_TYPE_SIMPLE,
      .handle = 0,
  };
  EXPECT_OK(display()->DisplayControllerImplSetBufferCollectionConstraints2(
      &kDefaultConfig, kValidBufferCollectionId));

  // Set BufferCollection buffer memory constraints.
  fidl::Status set_constraints_status = buffer_collection()->SetConstraints(
      /* has_constraints= */ true, fuchsia_sysmem::wire::BufferCollectionConstraints{
                                       .usage =
                                           {
                                               .cpu = fuchsia_sysmem::wire::kCpuUsageRead,
                                           },
                                       .min_buffer_count = 1,
                                       .max_buffer_count = 1,
                                       .has_buffer_memory_constraints = true,
                                       .buffer_memory_constraints =
                                           {
                                               .min_size_bytes = 4096,
                                               .max_size_bytes = 0xffffffff,
                                               .ram_domain_supported = true,
                                               .cpu_domain_supported = true,
                                               .inaccessible_domain_supported = true,
                                           },
                                   });
  EXPECT_TRUE(set_constraints_status.ok());

  // Both the test-side client and the driver have  set the constraints.
  // The buffer should be allocated correctly in sysmem.
  EXPECT_TRUE(buffer_collection()->WaitForBuffersAllocated().ok());

  // Test ReleaseBufferCollection().
  constexpr uint64_t kInvalidBufferCollectionId = 2u;
  EXPECT_EQ(display()->DisplayControllerImplReleaseBufferCollection(kInvalidBufferCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display()->DisplayControllerImplReleaseBufferCollection(kValidBufferCollectionId));
}

TEST_F(FakeDisplaySysmemTest, ImportImage) {
  constexpr uint64_t kBufferCollectionId = 1u;
  EXPECT_OK(display()->DisplayControllerImplImportBufferCollection(
      kBufferCollectionId, take_token_client().TakeChannel()));

  // Driver sets BufferCollection buffer memory constraints.
  const image_t kDefaultConfig = {
      .width = 1024,
      .height = 768,
      .pixel_format = ZX_PIXEL_FORMAT_ARGB_8888,
      .type = IMAGE_TYPE_SIMPLE,
      .handle = 0,
  };
  EXPECT_OK(display()->DisplayControllerImplSetBufferCollectionConstraints2(&kDefaultConfig,
                                                                            kBufferCollectionId));

  // Set BufferCollection buffer memory constraints.
  fidl::Status set_constraints_status = buffer_collection()->SetConstraints(
      /* has_constraints= */ true,
      fuchsia_sysmem::wire::BufferCollectionConstraints{
          .usage =
              {
                  .cpu = fuchsia_sysmem::wire::kCpuUsageRead,
              },
          .min_buffer_count = 1,
          .max_buffer_count = 1,
          .has_buffer_memory_constraints = true,
          .buffer_memory_constraints =
              {
                  .min_size_bytes = kDefaultConfig.width * kDefaultConfig.height *
                                    ZX_PIXEL_FORMAT_BYTES(kDefaultConfig.pixel_format),
                  .max_size_bytes = 0xffffffff,
                  .ram_domain_supported = true,
                  .cpu_domain_supported = true,
                  .inaccessible_domain_supported = true,
              },
      });
  EXPECT_TRUE(set_constraints_status.ok());

  // Both the test-side client and the driver have set the constraints.
  // The buffer should be allocated correctly in sysmem.
  EXPECT_TRUE(buffer_collection()->WaitForBuffersAllocated().ok());

  // Invalid import: Bad image type.
  image_t invalid_config = kDefaultConfig;
  invalid_config.type = IMAGE_TYPE_CAPTURE;
  EXPECT_EQ(display()->DisplayControllerImplImportImage2(&invalid_config, kBufferCollectionId,
                                                         /*index=*/0),
            ZX_ERR_INVALID_ARGS);

  // Invalid import: Invalid collection ID.
  invalid_config = kDefaultConfig;
  constexpr uint64_t kInvalidBufferCollectionId = 100u;
  EXPECT_EQ(display()->DisplayControllerImplImportImage2(&invalid_config,
                                                         kInvalidBufferCollectionId, /*index=*/0),
            ZX_ERR_NOT_FOUND);

  // Invalid import: Invalid buffer collection index.
  invalid_config = kDefaultConfig;
  constexpr uint64_t kInvalidBufferCollectionIndex = 100u;
  EXPECT_EQ(display()->DisplayControllerImplImportImage2(&invalid_config, kBufferCollectionId,
                                                         kInvalidBufferCollectionIndex),
            ZX_ERR_OUT_OF_RANGE);

  // Valid import.
  image_t valid_config = kDefaultConfig;
  EXPECT_EQ(valid_config.handle, 0u);
  EXPECT_OK(display()->DisplayControllerImplImportImage2(&valid_config, kBufferCollectionId,
                                                         /*index=*/0));
  EXPECT_NE(valid_config.handle, 0u);

  // Release the image.
  display()->DisplayControllerImplReleaseImage(&valid_config);

  EXPECT_OK(display()->DisplayControllerImplReleaseBufferCollection(kBufferCollectionId));
}

}  // namespace fake_display
