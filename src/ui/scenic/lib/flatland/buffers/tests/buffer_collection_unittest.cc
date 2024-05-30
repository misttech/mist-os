// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/flatland/buffers/buffer_collection.h"

#include <lib/fdio/directory.h>

#include <gtest/gtest.h>

#include "src/lib/fsl/handles/object_info.h"
#include "src/ui/scenic/lib/flatland/buffers/util.h"

namespace flatland {
namespace test {

const uint32_t kCpuUsageWriteOften = fuchsia::sysmem::cpuUsageWriteOften;

// Common testing base class to be used across different unittests that
// require Vulkan and a SysmemAllocator.
class BufferCollectionTest : public ::testing::Test {
 protected:
  void SetUp() override {
    ::testing::Test::SetUp();
    // Create the SysmemAllocator.
    zx_status_t status = fdio_service_connect(
        "/svc/fuchsia.sysmem2.Allocator", sysmem_allocator_.NewRequest().TakeChannel().release());
    EXPECT_EQ(status, ZX_OK);

    fuchsia::sysmem2::AllocatorSetDebugClientInfoRequest set_debug_request;
    set_debug_request.set_name(fsl::GetCurrentProcessName() + " BufferCollectionTest");
    set_debug_request.set_id(fsl::GetCurrentProcessKoid());
    sysmem_allocator_->SetDebugClientInfo(std::move(set_debug_request));
  }

  void TearDown() override {
    sysmem_allocator_ = nullptr;
    ::testing::Test::TearDown();
  }

  fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator_;
};

// Test the creation of a buffer collection that doesn't have any additional vulkan
// constraints to show that it doesn't need vulkan to be valid.
TEST_F(BufferCollectionTest, CreateCollectionTest) {
  auto tokens = SysmemTokens::Create(sysmem_allocator_.get());
  auto result = BufferCollectionInfo::New(sysmem_allocator_.get(), std::move(tokens.dup_token));
  EXPECT_TRUE(result.is_ok());
}

// This test ensures that the buffer collection can still be allocated even if the server
// does not set extra customizable constraints via a call to GenerateToken(). This is
// necessary due to the fact that the buffer collection keeps around a dummy token in
// case new constraints need to be added, but the existence of the dummy token itself
// prevents allocation until it is closed out. So this test makes sure that when we close
// out the dummy token inside the call to WaitUntilAllocated() that this is enough to ensure
// that we can still allocate the buffer collection.
TEST_F(BufferCollectionTest, AllocationWithoutExtraConstraints) {
  fuchsia::sysmem2::BufferUsage buffer_usage;
  buffer_usage.set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE_OFTEN);
  auto tokens = SysmemTokens::Create(sysmem_allocator_.get());
  auto result = BufferCollectionInfo::New(sysmem_allocator_.get(), std::move(tokens.dup_token),
                                          std::nullopt, std::move(buffer_usage));
  EXPECT_TRUE(result.is_ok());

  auto collection = std::move(result.value());

  // Client hasn't set their constraints yet, so this should be false.
  EXPECT_FALSE(collection.BuffersAreAllocated());

  {
    const uint32_t kWidth = 32;
    const uint32_t kHeight = 64;
    fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;

    fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
    bind_shared_request.set_token(std::move(tokens.local_token));
    bind_shared_request.set_buffer_collection_request(buffer_collection.NewRequest());
    zx_status_t status = sysmem_allocator_->BindSharedCollection(std::move(bind_shared_request));

    fuchsia::sysmem2::NodeSetNameRequest set_name_request;
    set_name_request.set_priority(100u);
    set_name_request.set_name("FlatlandAllocationWithoutExtraConstraints");
    buffer_collection->SetName(std::move(set_name_request));

    EXPECT_EQ(status, ZX_OK);
    fuchsia::sysmem2::BufferCollectionConstraints constraints;
    auto& bmc = *constraints.mutable_buffer_memory_constraints();
    bmc.set_cpu_domain_supported(true);
    bmc.set_ram_domain_supported(true);
    constraints.mutable_usage()->set_cpu(kCpuUsageWriteOften);
    constraints.set_min_buffer_count(1);

    auto& image_constraints = constraints.mutable_image_format_constraints()->emplace_back();
    image_constraints.mutable_color_spaces()->emplace_back(fuchsia::images2::ColorSpace::SRGB);
    image_constraints.set_pixel_format(fuchsia::images2::PixelFormat::B8G8R8A8);
    image_constraints.set_pixel_format_modifier(fuchsia::images2::PixelFormatModifier::LINEAR);

    image_constraints.set_min_size(fuchsia::math::SizeU{kWidth, kHeight});
    image_constraints.set_max_size(fuchsia::math::SizeU{kWidth, kHeight});

    fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.set_constraints(std::move(constraints));
    status = buffer_collection->SetConstraints(std::move(set_constraints_request));
    EXPECT_EQ(status, ZX_OK);

    // Have the client wait for allocation.
    fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
    status = buffer_collection->WaitForAllBuffersAllocated(&wait_result);
    EXPECT_EQ(status, ZX_OK);
    EXPECT_TRUE(wait_result.is_response());

    status = buffer_collection->Release();
    EXPECT_EQ(status, ZX_OK);
  }

  // Checking allocation on the server should return true.
  EXPECT_TRUE(collection.BuffersAreAllocated());
}

// Check to make sure |CreateBufferCollectionAndSetConstraints| returns false if
// an invalid BufferCollectionHandle is provided by the user.
TEST_F(BufferCollectionTest, NullTokenTest) {
  auto result = BufferCollectionInfo::New(sysmem_allocator_.get(),
                                          /*token*/ nullptr);
  EXPECT_TRUE(result.is_error());
}

// We pass in a valid channel to |CreateBufferCollectionAndSetConstraints|, but
// it's not actually a channel to a BufferCollection.
TEST_F(BufferCollectionTest, WrongTokenTypeTest) {
  zx::channel local_endpoint;
  zx::channel remote_endpoint;
  zx::channel::create(0, &local_endpoint, &remote_endpoint);

  // Here we inject a generic channel into a BufferCollectionHandle before passing the
  // handle into |CreateCollectionAndSetConstraints|. So the channel is valid,
  // but it is just not a BufferCollectionToken.
  BufferCollectionHandle handle{std::move(remote_endpoint)};

  // Make sure the handle is valid before passing it in.
  ASSERT_TRUE(handle.is_valid());

  // We should not be able to make a BufferCollectionInfon object with the wrong token type
  // passed in as a parameter.
  auto result = BufferCollectionInfo::New(sysmem_allocator_.get(), std::move(handle));
  EXPECT_TRUE(result.is_error());
}

// If the client sets constraints on the buffer collection that are incompatible
// with the constraints set on the server-side by the renderer, then waiting on
// the buffers to be allocated should fail.
TEST_F(BufferCollectionTest, IncompatibleConstraintsTest) {
  auto tokens = SysmemTokens::Create(sysmem_allocator_.get());
  auto result = BufferCollectionInfo::New(sysmem_allocator_.get(), std::move(tokens.dup_token));
  EXPECT_TRUE(result.is_ok());

  auto collection = std::move(result.value());

  // Create a client-side handle to the buffer collection and set the client
  // constraints. We set it to have a max of zero buffers and to not use
  // vulkan sampling, which the server side will specify is necessary.
  {
    fuchsia::sysmem2::BufferCollectionSyncPtr client_collection;

    fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
    bind_shared_request.set_token(std::move(tokens.local_token));
    bind_shared_request.set_buffer_collection_request(client_collection.NewRequest());
    zx_status_t status = sysmem_allocator_->BindSharedCollection(std::move(bind_shared_request));
    EXPECT_EQ(status, ZX_OK);

    fuchsia::sysmem2::NodeSetNameRequest set_name_request;
    set_name_request.set_priority(100u);
    set_name_request.set_name("FlatlandIncompatibleConstraintsTest");
    client_collection->SetName(std::move(set_name_request));

    fuchsia::sysmem2::BufferCollectionConstraints constraints;
    auto& bmc = *constraints.mutable_buffer_memory_constraints();
    bmc.set_cpu_domain_supported(true);
    bmc.set_ram_domain_supported(true);
    constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE_OFTEN);

    // Need at least one buffer normally.
    constraints.set_min_buffer_count(0);
    constraints.set_max_buffer_count(0);

    // TODO: Is setting 0 here the intent? (the "!" was preserved during sysmem2 migration)
    constraints.mutable_usage()->set_vulkan(!fuchsia::sysmem2::VULKAN_IMAGE_USAGE_SAMPLED);

    auto& image_constraints = constraints.mutable_image_format_constraints()->emplace_back();

    image_constraints.set_pixel_format(fuchsia::images2::PixelFormat::R8G8B8A8);
    image_constraints.set_pixel_format_modifier(fuchsia::images2::PixelFormatModifier::LINEAR);

    // The renderer requires that the the buffer can at least have a
    // width/height of 1, which is not possible here.
    image_constraints.set_required_min_size(fuchsia::math::SizeU{0, 0});
    image_constraints.set_required_max_size(::fuchsia::math::SizeU{0, 0});
    image_constraints.set_max_size(fuchsia::math::SizeU{.width = 0, .height = 0});
    image_constraints.set_max_bytes_per_row(0x0);

    fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.set_constraints(std::move(constraints));
    status = client_collection->SetConstraints(std::move(set_constraints_request));
    EXPECT_EQ(status, ZX_OK);

    // Have the client wait for allocation.
    fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
    status = client_collection->WaitForAllBuffersAllocated(&wait_result);

    // Sysmem reports the error here through |status|.
    EXPECT_NE(status, ZX_OK);
    EXPECT_FALSE(wait_result.is_response());
  }

  // This should fail as sysmem won't be able to allocate anything.
  EXPECT_FALSE(collection.BuffersAreAllocated());
}

}  // namespace test
}  // namespace flatland
