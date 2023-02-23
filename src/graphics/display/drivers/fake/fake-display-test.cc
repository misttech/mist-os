// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.sysmem/cpp/wire.h>
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

TEST_F(FakeDisplayTest, ImportBufferCollection) {
  zx::result sysmem_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
  ASSERT_OK(sysmem_endpoints);
  auto& [sysmem_client, sysmem_server] = sysmem_endpoints.value();
  EXPECT_TRUE(sysmem_fidl()->ConnectV1(std::move(sysmem_server)).ok());

  fidl::WireSyncClient sysmem(std::move(sysmem_client));

  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_OK(token_endpoints);
  auto& [token_client, token_server] = token_endpoints.value();
  fidl::Status allocate_token_status = sysmem->AllocateSharedCollection(std::move(token_server));
  ASSERT_TRUE(allocate_token_status.ok());

  EXPECT_TRUE(fidl::WireCall(token_client)->Sync().ok());

  // At least one sysmem participant should specify buffer memory constraints.
  // The driver may not specify buffer memory constraints, so the test should
  // always provide one for sysmem.
  //
  // Here we duplicate the token to set buffer collection constraints in
  // the test.
  std::vector<zx_rights_t> rights = {ZX_RIGHT_SAME_RIGHTS};
  fidl::WireResult duplicate_result =
      fidl::WireCall(token_client)
          ->DuplicateSync(fidl::VectorView<zx_rights_t>::FromExternal(rights));
  ASSERT_TRUE(duplicate_result.ok());
  auto& duplicate_value = duplicate_result.value();
  ASSERT_EQ(duplicate_value.tokens.count(), 1u);

  // Bind duplicated token to BufferCollection client.
  zx::result collection_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollection>();
  ASSERT_OK(collection_endpoints);
  auto& [collection_client, collection_server] = collection_endpoints.value();
  fidl::Status bind_status = sysmem->BindSharedCollection(std::move(duplicate_value.tokens[0]),
                                                          std::move(collection_server));
  EXPECT_TRUE(bind_status.ok());

  // Test ImportBufferCollection().
  constexpr uint64_t kValidBufferCollectionId = 1u;
  EXPECT_OK(display()->DisplayControllerImplImportBufferCollection(kValidBufferCollectionId,
                                                                   token_client.TakeChannel()));

  // `collection_id` must be unused.
  zx::result another_token_endpoints =
      fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_OK(another_token_endpoints);
  EXPECT_EQ(display()->DisplayControllerImplImportBufferCollection(
                kValidBufferCollectionId, another_token_endpoints->client.TakeChannel()),
            ZX_ERR_ALREADY_EXISTS);

  // Set BufferCollection buffer memory constraints.
  fidl::WireSyncClient buffer_collection(std::move(collection_client));
  fidl::Status set_constraints_status = buffer_collection->SetConstraints(
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
  EXPECT_TRUE(buffer_collection->WaitForBuffersAllocated().ok());

  // Test ReleaseBufferCollection().
  constexpr uint64_t kInvalidBufferCollectionId = 2u;
  EXPECT_EQ(display()->DisplayControllerImplReleaseBufferCollection(kInvalidBufferCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display()->DisplayControllerImplReleaseBufferCollection(kValidBufferCollectionId));
}

}  // namespace fake_display
