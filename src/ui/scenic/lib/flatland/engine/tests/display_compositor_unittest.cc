// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/display/cpp/fidl.h>

#include <memory>

#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/fxl/strings/join_strings.h"
#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"
#include "src/ui/scenic/lib/display/tests/mock_display_controller.h"
#include "src/ui/scenic/lib/flatland/buffers/util.h"
#include "src/ui/scenic/lib/flatland/engine/tests/common.h"
#include "src/ui/scenic/lib/flatland/engine/tests/mock_display_controller.h"
#include "src/ui/scenic/lib/flatland/renderer/mock_renderer.h"
#include "src/ui/scenic/lib/utils/helpers.h"

using ::testing::_;
using ::testing::Return;

using allocation::BufferCollectionUsage;
using allocation::ImageMetadata;
using flatland::LinkSystem;
using flatland::MockDisplayCoordinator;
using flatland::Renderer;
using flatland::TransformGraph;
using flatland::TransformHandle;
using flatland::UberStruct;
using flatland::UberStructSystem;
using fuchsia::ui::composition::ChildViewStatus;
using fuchsia::ui::composition::ChildViewWatcher;
using fuchsia::ui::composition::ImageFlip;
using fuchsia::ui::composition::LayoutInfo;
using fuchsia::ui::composition::ParentViewportWatcher;
using fuchsia::ui::views::ViewCreationToken;
using fuchsia::ui::views::ViewportCreationToken;
using fhd_Transform = fuchsia::hardware::display::Transform;
using fuchsia::sysmem::BufferUsage;

namespace flatland::test {

namespace {

// FIDL HLCPP non-resource structs (e.g. LayerId) cannot be compared directly
// using Eq() matcher. This implements a matcher for FIDL type comparison.
//
// Example:
//
// fuchsia::hardware::display::LayerId kId = {.value = 1};
// fuchsia::hardware::display::LayerId kAnotherId = {.value = 1};
// EXPECT_THAT(kId, FidlEquals(kAnotherId));
//
MATCHER_P(FidlEquals, value,
          fxl::JoinStrings(std::vector<std::string>{"FIDL values", negation ? " don't " : " ",
                                                    "match"})) {
  return fidl::Equals(arg, value);
}

fuchsia::sysmem::BufferCollectionTokenPtr DuplicateToken(
    fuchsia::sysmem::BufferCollectionTokenSyncPtr& token) {
  std::vector<fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>> dup_tokens;
  const auto status = token->DuplicateSync({ZX_RIGHT_SAME_RIGHTS}, &dup_tokens);
  FX_CHECK(status == ZX_OK);
  FX_CHECK(dup_tokens.size() == 1u);
  return dup_tokens.at(0).Bind();
}

void SetConstraintsAndClose(fuchsia::sysmem::AllocatorSyncPtr& sysmem_allocator,
                            fuchsia::sysmem::BufferCollectionTokenSyncPtr token,
                            fuchsia::sysmem::BufferCollectionConstraints constraints) {
  fuchsia::sysmem::BufferCollectionSyncPtr collection;
  ASSERT_EQ(sysmem_allocator->BindSharedCollection(std::move(token), collection.NewRequest()),
            ZX_OK);
  ASSERT_EQ(collection->SetConstraints(true, constraints), ZX_OK);
  // If SetConstraints() fails there's a race where Sysmem may drop the channel. Don't assert on the
  // success of Close().
  collection->Close();
}

// Creates a thread that waits until |num_messages| have been received by |mock|, then calls join()
// on destruction. Returns a unique_ptr<> with a custom deleter.
// TODO(fxbug.dev/71264): Use function call counters from MockDisplayCoordinator instead of counting
// them manually.
auto CreateServerWaitingForMessages(flatland::MockDisplayCoordinator& mock,
                                    const uint32_t num_messages) {
  return std::unique_ptr<std::thread, std::function<void(std::thread*)>>(
      new std::thread([&mock, num_messages]() mutable {
        for (uint32_t i = 0; i < num_messages; i++) {
          mock.WaitForMessage();
        }
      }),
      [](std::thread* thread) {
        thread->join();
        delete thread;
      });
}

}  // namespace

class DisplayCompositorTest : public DisplayCompositorTestBase {
 public:
  void SetUp() override {
    DisplayCompositorTestBase::SetUp();

    sysmem_allocator_ = utils::CreateSysmemAllocatorSyncPtr("DisplayCompositorTest");

    renderer_ = std::make_shared<flatland::MockRenderer>();

    zx::channel device_channel_server;
    zx::channel device_channel_client;
    FX_CHECK(ZX_OK == zx::channel::create(0, &device_channel_server, &device_channel_client));
    zx::channel coordinator_channel_server;
    zx::channel coordinator_channel_client;
    FX_CHECK(ZX_OK ==
             zx::channel::create(0, &coordinator_channel_server, &coordinator_channel_client));

    mock_display_coordinator_ = std::make_unique<flatland::MockDisplayCoordinator>();
    mock_display_coordinator_->Bind(std::move(device_channel_server),
                                    std::move(coordinator_channel_server));

    auto shared_display_coordinator =
        std::make_shared<fuchsia::hardware::display::CoordinatorSyncPtr>();
    shared_display_coordinator->Bind(std::move(coordinator_channel_client));

    display_compositor_ = std::make_shared<flatland::DisplayCompositor>(
        dispatcher(), std::move(shared_display_coordinator), renderer_,
        utils::CreateSysmemAllocatorSyncPtr("display_compositor_unittest"),
        /*enable_display_composition*/ true);
  }

  void TearDown() override {
    renderer_.reset();
    display_compositor_.reset();
    mock_display_coordinator_.reset();

    DisplayCompositorTestBase::TearDown();
  }

  fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> CreateToken() {
    fuchsia::sysmem::BufferCollectionTokenSyncPtr token;
    zx_status_t status = sysmem_allocator_->AllocateSharedCollection(token.NewRequest());
    FX_DCHECK(status == ZX_OK);
    status = token->Sync();
    FX_DCHECK(status == ZX_OK);
    return token;
  }

  void SetDisplaySupported(allocation::GlobalBufferCollectionId id, bool is_supported) {
    std::scoped_lock lock(display_compositor_->lock_);
    display_compositor_->buffer_collection_supports_display_[id] = is_supported;
    display_compositor_->buffer_collection_pixel_format_[id] = fuchsia::sysmem::PixelFormat{
        .type = fuchsia::sysmem::PixelFormatType::BGRA32,
    };
  }

  void ForceRendererOnlyMode(bool force_renderer_only) {
    display_compositor_->enable_display_composition_ = !force_renderer_only;
  }

  void SendOnVsyncEvent(fuchsia::hardware::display::ConfigStamp stamp) {
    display_compositor_->OnVsync(zx::time(), stamp);
  }

  std::deque<DisplayCompositor::ApplyConfigInfo> GetPendingApplyConfigs() {
    return display_compositor_->pending_apply_configs_;
  }

  bool BufferCollectionSupportsDisplay(allocation::GlobalBufferCollectionId id) {
    std::scoped_lock lock(display_compositor_->lock_);
    return display_compositor_->buffer_collection_supports_display_.count(id) &&
           display_compositor_->buffer_collection_supports_display_[id];
  }

 protected:
  static constexpr fuchsia_images2::PixelFormat kPixelFormat =
      fuchsia_images2::PixelFormat::kBgra32;
  std::unique_ptr<flatland::MockDisplayCoordinator> mock_display_coordinator_;
  std::shared_ptr<flatland::MockRenderer> renderer_;
  std::shared_ptr<flatland::DisplayCompositor> display_compositor_;

  // Only for use on the main thread. Establish a new connection when on the MockDisplayCoordinator
  // thread.
  fuchsia::sysmem::AllocatorSyncPtr sysmem_allocator_;

  void HardwareFrameCorrectnessWithRotationTester(glm::mat3 transform_matrix, ImageFlip image_flip,
                                                  fuchsia::hardware::display::Frame expected_dst,
                                                  fhd_Transform expected_transform);
};

TEST_F(DisplayCompositorTest, ImportAndReleaseBufferCollectionTest) {
  // Expected calls: ImportBufferCollection(), SetBufferCollectionConstraints(),
  // ReleaseBufferCollection(), CheckConfig(), and one for deletion.
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 5);

  const allocation::GlobalBufferCollectionId kGlobalBufferCollectionId = 15;

  EXPECT_CALL(*mock_display_coordinator_, ImportBufferCollection(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t, fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(ZX_OK);
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t collection_id, fuchsia::hardware::display::ImageConfig config,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(ZX_OK);
          }));
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId, fuchsia::sysmem::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                             BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        token_ref = std::move(token);
        return true;
      });
  display_compositor_->ImportBufferCollection(kGlobalBufferCollectionId, sysmem_allocator_.get(),
                                              CreateToken(), BufferCollectionUsage::kClientImage,
                                              std::nullopt);

  EXPECT_CALL(*mock_display_coordinator_, ReleaseBufferCollection(kGlobalBufferCollectionId))
      .WillOnce(Return());
  EXPECT_CALL(*renderer_, ReleaseBufferCollection(kGlobalBufferCollectionId, _)).WillOnce(Return());
  display_compositor_->ReleaseBufferCollection(kGlobalBufferCollectionId,
                                               BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::ConfigResult result =
            fuchsia::hardware::display::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  display_compositor_.reset();
}

// This test makes sure the buffer negotiations work as intended.
// There are three participants: the client, the display and the renderer.
// Each participant sets {min_buffer_count, max_buffer_count} constraints like so:
// Client: {1, 3}
// Display: {2, 3}
// Renderer: {1, 2}
// Since 2 is the only valid overlap between all of them we expect 2 buffers to be allocated.
TEST_F(DisplayCompositorTest,
       SysmemNegotiationTest_WhenDisplayConstraintsCompatible_TheyShouldBeIncluded) {
  // Expected calls: ImportBufferCollection(), SetBufferCollectionConstraints(),
  // ImportImage(), CheckConfig(), and one for deletion.
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 5);

  // Create two tokens: one for acting as the "client" and inspecting allocation results with, and
  // one to send to the display compositor.
  fuchsia::sysmem::BufferCollectionTokenSyncPtr client_token = CreateToken().BindSync();
  fuchsia::sysmem::BufferCollectionTokenPtr compositor_token = DuplicateToken(client_token);

  // Set "client" constraints.
  fuchsia::sysmem::BufferCollectionSyncPtr client_collection;
  ASSERT_EQ(sysmem_allocator_->BindSharedCollection(std::move(client_token),
                                                    client_collection.NewRequest()),
            ZX_OK);
  ASSERT_EQ(client_collection->SetConstraints(
                true,
                fuchsia::sysmem::BufferCollectionConstraints{
                    .usage{.cpu = fuchsia::sysmem::cpuUsageWrite},
                    .min_buffer_count = 1,
                    .max_buffer_count = 3,
                    .has_buffer_memory_constraints = true,
                    .buffer_memory_constraints{
                        .min_size_bytes = 1,
                        .max_size_bytes = 20,
                    },
                    .image_format_constraints_count = 1,
                    .image_format_constraints{
                        {{.pixel_format{.type = fuchsia::sysmem::PixelFormatType::BGRA32},
                          .color_spaces_count = 1,
                          .color_space{{{.type = fuchsia::sysmem::ColorSpaceType::SRGB}}},
                          .min_coded_width = 1,
                          .min_coded_height = 1}}}}),
            ZX_OK);

  const auto kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();

  fuchsia::sysmem::BufferCollectionTokenSyncPtr display_token;
  EXPECT_CALL(*mock_display_coordinator_, ImportBufferCollection(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [&display_token](uint64_t,
                           fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                           MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            display_token = token.BindSync();
            callback(ZX_OK);
          }));

  // Set display constraints.
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [&display_token](
              uint64_t collection_id, fuchsia::hardware::display::ImageConfig config,
              MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            auto sysmem_allocator = utils::CreateSysmemAllocatorSyncPtr("MockDisplayCoordinator");
            SetConstraintsAndClose(sysmem_allocator, std::move(display_token),
                                   fuchsia::sysmem::BufferCollectionConstraints{
                                       .usage{.cpu = fuchsia::sysmem::cpuUsageWrite},
                                       .min_buffer_count = 2,
                                       .max_buffer_count = 3,
                                   });
            callback(ZX_OK);
          }));
  EXPECT_CALL(*mock_display_coordinator_, ImportImage(_, kGlobalBufferCollectionId, _, 0, _))
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::ImageConfig, uint64_t, uint64_t, uint32_t,
             MockDisplayCoordinator::ImportImageCallback callback) { callback(ZX_OK); }));
  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        callback(fuchsia::hardware::display::ConfigResult::OK, /*ops=*/{});
      }));

  // Set renderer constraints.
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([this](allocation::GlobalBufferCollectionId, fuchsia::sysmem::Allocator_Sync*,
                       fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> renderer_token,
                       BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        SetConstraintsAndClose(sysmem_allocator_, renderer_token.BindSync(),
                               fuchsia::sysmem::BufferCollectionConstraints{
                                   .usage{.cpu = fuchsia::sysmem::cpuUsageWrite},
                                   .min_buffer_count = 1,
                                   .max_buffer_count = 2,
                               });
        return true;
      });

  ASSERT_TRUE(display_compositor_->ImportBufferCollection(
      kGlobalBufferCollectionId, sysmem_allocator_.get(), std::move(compositor_token),
      BufferCollectionUsage::kClientImage, std::nullopt));

  {
    fuchsia::sysmem::BufferCollectionInfo_2 buffer_collection_info{};
    zx_status_t allocation_status = ZX_OK;
    ASSERT_EQ(
        client_collection->WaitForBuffersAllocated(&allocation_status, &buffer_collection_info),
        ZX_OK);
    EXPECT_EQ(allocation_status, ZX_OK);
    EXPECT_EQ(buffer_collection_info.buffer_count, 2u);
  }

  // ImportBufferImage() to confirm that the allocation was handled correctly.
  EXPECT_CALL(*renderer_, ImportBufferImage(_, _)).WillOnce([](...) { return true; });
  ASSERT_TRUE(display_compositor_->ImportBufferImage(
      ImageMetadata{.collection_id = kGlobalBufferCollectionId,
                    .identifier = 1,
                    .vmo_index = 0,
                    .width = 1,
                    .height = 1},
      BufferCollectionUsage::kClientImage));
  EXPECT_TRUE(BufferCollectionSupportsDisplay(kGlobalBufferCollectionId));

  display_compositor_.reset();
}

// This test makes sure the buffer negotiations work as intended.
// There are three participants: the client, the display and the renderer.
// Each participant sets {min_buffer_count, max_buffer_count} constraints like so:
// Client: {1, 2}
// Display: {1, 1}
// Renderer: {2, 2}
// Since there is no valid overlap between all participants the display should drop out and we
// expect 2 buffers to be allocated (the only valid overlap between client and renderer).
TEST_F(DisplayCompositorTest,
       SysmemNegotiationTest_WhenDisplayConstraintsIncompatible_TheyShouldBeExcluded) {
  // Expected calls: ImportBufferCollection(), SetBufferCollectionConstraints(),
  // CheckConfig(), and one for deletion.
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 4);

  // Create two tokens: one for acting as the "client" and inspecting allocation results with, and
  // one to send to the display compositor.
  fuchsia::sysmem::BufferCollectionTokenSyncPtr client_token = CreateToken().BindSync();
  fuchsia::sysmem::BufferCollectionTokenPtr compositor_token = DuplicateToken(client_token);

  // Set "client" constraints.
  fuchsia::sysmem::BufferCollectionSyncPtr client_collection;
  ASSERT_EQ(sysmem_allocator_->BindSharedCollection(std::move(client_token),
                                                    client_collection.NewRequest()),
            ZX_OK);
  ASSERT_EQ(client_collection->SetConstraints(
                true,
                fuchsia::sysmem::BufferCollectionConstraints{
                    .usage{.cpu = fuchsia::sysmem::cpuUsageWrite},
                    .min_buffer_count = 1,
                    .max_buffer_count = 2,
                    .has_buffer_memory_constraints = true,
                    .buffer_memory_constraints{
                        .min_size_bytes = 1,
                        .max_size_bytes = 20,
                    },
                    .image_format_constraints_count = 1,
                    .image_format_constraints{
                        {{.pixel_format{.type = fuchsia::sysmem::PixelFormatType::BGRA32},
                          .color_spaces_count = 1,
                          .color_space{{{.type = fuchsia::sysmem::ColorSpaceType::SRGB}}},
                          .min_coded_width = 1,
                          .min_coded_height = 1}}}}),
            ZX_OK);

  const auto kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();

  fuchsia::sysmem::BufferCollectionTokenSyncPtr display_token;
  EXPECT_CALL(*mock_display_coordinator_, ImportBufferCollection(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [&display_token](uint64_t,
                           fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                           MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            display_token = token.BindSync();
            callback(ZX_OK);
          }));

  // Set display constraints.
  fuchsia::sysmem::BufferCollectionSyncPtr display_collection;
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [&display_token](
              uint64_t collection_id, fuchsia::hardware::display::ImageConfig config,
              MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            auto sysmem_allocator = utils::CreateSysmemAllocatorSyncPtr("MockDisplayCoordinator");
            SetConstraintsAndClose(sysmem_allocator, std::move(display_token),
                                   fuchsia::sysmem::BufferCollectionConstraints{
                                       .usage{.cpu = fuchsia::sysmem::cpuUsageWrite},
                                       .min_buffer_count = 1,
                                       .max_buffer_count = 1,
                                   });
            callback(ZX_OK);
          }));
  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        callback(fuchsia::hardware::display::ConfigResult::OK, /*ops=*/{});
      }));

  // Set renderer constraints.
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([this](allocation::GlobalBufferCollectionId, fuchsia::sysmem::Allocator_Sync*,
                       fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> renderer_token,
                       BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        SetConstraintsAndClose(sysmem_allocator_, renderer_token.BindSync(),
                               fuchsia::sysmem::BufferCollectionConstraints{
                                   .usage{.cpu = fuchsia::sysmem::cpuUsageWrite},
                                   .min_buffer_count = 2,
                                   .max_buffer_count = 2,
                               });
        return true;
      });

  ASSERT_TRUE(display_compositor_->ImportBufferCollection(
      kGlobalBufferCollectionId, sysmem_allocator_.get(), std::move(compositor_token),
      BufferCollectionUsage::kClientImage, std::nullopt));

  {
    fuchsia::sysmem::BufferCollectionInfo_2 buffer_collection_info{};
    zx_status_t allocation_status = ZX_OK;
    ASSERT_EQ(
        client_collection->WaitForBuffersAllocated(&allocation_status, &buffer_collection_info),
        ZX_OK);
    EXPECT_EQ(allocation_status, ZX_OK);
    EXPECT_EQ(buffer_collection_info.buffer_count, 2u);
  }

  // ImportBufferImage() to confirm that the allocation was handled correctly.
  EXPECT_CALL(*renderer_, ImportBufferImage(_, _)).WillOnce([](...) { return true; });
  ASSERT_TRUE(display_compositor_->ImportBufferImage(
      ImageMetadata{.collection_id = kGlobalBufferCollectionId,
                    .identifier = 1,
                    .vmo_index = 0,
                    .width = 1,
                    .height = 1},
      BufferCollectionUsage::kClientImage));
  EXPECT_FALSE(BufferCollectionSupportsDisplay(kGlobalBufferCollectionId));

  display_compositor_.reset();
}

TEST_F(DisplayCompositorTest, SysmemNegotiationTest_InRendererOnlyMode_DisplayShouldExcludeItself) {
  ForceRendererOnlyMode(true);

  // Expected calls: CheckConfig(), and one for deletion.
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 2);

  // Create two tokens: one for acting as the "client" and inspecting allocation results with, and
  // one to send to the display compositor.
  fuchsia::sysmem::BufferCollectionTokenSyncPtr client_token = CreateToken().BindSync();
  fuchsia::sysmem::BufferCollectionTokenPtr compositor_token = DuplicateToken(client_token);

  // Set "client" constraints.
  fuchsia::sysmem::BufferCollectionSyncPtr client_collection;
  ASSERT_EQ(sysmem_allocator_->BindSharedCollection(std::move(client_token),
                                                    client_collection.NewRequest()),
            ZX_OK);

  ASSERT_EQ(client_collection->SetConstraints(
                true,
                fuchsia::sysmem::BufferCollectionConstraints{
                    .usage{.cpu = fuchsia::sysmem::cpuUsageWrite},
                    .has_buffer_memory_constraints = true,
                    .buffer_memory_constraints{
                        .min_size_bytes = 1,
                        .max_size_bytes = 20,
                    },
                    .image_format_constraints_count = 1,
                    .image_format_constraints{
                        {{.pixel_format{.type = fuchsia::sysmem::PixelFormatType::BGRA32},
                          .color_spaces_count = 1,
                          .color_space{{{.type = fuchsia::sysmem::ColorSpaceType::SRGB}}},
                          .min_coded_width = 1,
                          .min_coded_height = 1}}}}),
            ZX_OK);

  const auto kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        callback(fuchsia::hardware::display::ConfigResult::OK, /*ops=*/{});
      }));

  // Set renderer constraints.
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([this](allocation::GlobalBufferCollectionId, fuchsia::sysmem::Allocator_Sync*,
                       fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> renderer_token,
                       BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        SetConstraintsAndClose(sysmem_allocator_, renderer_token.BindSync(),
                               fuchsia::sysmem::BufferCollectionConstraints{
                                   .usage{.cpu = fuchsia::sysmem::cpuUsageWrite},
                                   .min_buffer_count = 2,
                                   .max_buffer_count = 2,
                               });
        return true;
      });

  // Import BufferCollection and image to trigger constraint setting and handling of allocations.
  ASSERT_TRUE(display_compositor_->ImportBufferCollection(
      kGlobalBufferCollectionId, sysmem_allocator_.get(), std::move(compositor_token),
      BufferCollectionUsage::kClientImage, std::nullopt));

  {
    fuchsia::sysmem::BufferCollectionInfo_2 buffer_collection_info{};
    zx_status_t allocation_status = ZX_OK;
    ASSERT_EQ(
        client_collection->WaitForBuffersAllocated(&allocation_status, &buffer_collection_info),
        ZX_OK);
    EXPECT_EQ(allocation_status, ZX_OK);
    EXPECT_EQ(buffer_collection_info.buffer_count, 2u);
  }

  // ImportBufferImage() to confirm that the allocation was handled correctly.
  EXPECT_CALL(*renderer_, ImportBufferImage(_, _)).WillOnce([](...) { return true; });
  ASSERT_TRUE(display_compositor_->ImportBufferImage(
      ImageMetadata{.collection_id = kGlobalBufferCollectionId,
                    .identifier = 1,
                    .vmo_index = 0,
                    .width = 1,
                    .height = 1},
      BufferCollectionUsage::kClientImage));
  EXPECT_FALSE(BufferCollectionSupportsDisplay(kGlobalBufferCollectionId));

  display_compositor_.reset();
}

TEST_F(DisplayCompositorTest, ClientDropSysmemToken) {
  // Wait once for call to deleter.
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 1);

  const auto kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();
  fuchsia::sysmem::BufferCollectionTokenSyncPtr dup_token;
  // Let client drop token.
  {
    auto token = CreateToken();
    auto sync_token = token.BindSync();
    sync_token->Duplicate(ZX_RIGHT_SAME_RIGHTS, dup_token.NewRequest());
    sync_token->Sync();
  }

  // Save token to avoid early token failure in Renderer import.
  fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token_ref;
  ON_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillByDefault(
          [&token_ref](allocation::GlobalBufferCollectionId, fuchsia::sysmem::Allocator_Sync*,
                       fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                       BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
            token_ref = std::move(token);
            return true;
          });
  EXPECT_FALSE(display_compositor_->ImportBufferCollection(
      kGlobalBufferCollectionId, sysmem_allocator_.get(), std::move(dup_token),
      BufferCollectionUsage::kClientImage, std::nullopt));

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::ConfigResult result =
            fuchsia::hardware::display::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));
  display_compositor_.reset();
}

TEST_F(DisplayCompositorTest, ImageIsValidAfterReleaseBufferCollection) {
  // Expected calls: ImportBufferCollection(), SetBufferCollectionConstraints(),
  // ReleaseBufferCollection(), CheckConfig() and one for deletion.
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 5);

  const auto kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();

  // Import buffer collection.
  EXPECT_CALL(*mock_display_coordinator_, ImportBufferCollection(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t, fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(ZX_OK);
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t collection_id, fuchsia::hardware::display::ImageConfig config,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(ZX_OK);
          }));
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId, fuchsia::sysmem::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                             BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        token_ref = std::move(token);
        return true;
      });
  display_compositor_->ImportBufferCollection(kGlobalBufferCollectionId, sysmem_allocator_.get(),
                                              CreateToken(), BufferCollectionUsage::kClientImage,
                                              std::nullopt);
  SetDisplaySupported(kGlobalBufferCollectionId, true);

  // Import image.
  ImageMetadata image_metadata = ImageMetadata{
      .collection_id = kGlobalBufferCollectionId,
      .identifier = allocation::GenerateUniqueImageId(),
      .vmo_index = 0,
      .width = 128,
      .height = 256,
      .blend_mode = fuchsia::ui::composition::BlendMode::SRC,
  };
  EXPECT_CALL(*mock_display_coordinator_, ImportImage(_, kGlobalBufferCollectionId, _, 0, _))
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::ImageConfig, uint64_t, uint64_t, uint32_t,
             MockDisplayCoordinator::ImportImageCallback callback) { callback(ZX_OK); }));
  EXPECT_CALL(*renderer_, ImportBufferImage(image_metadata, _)).WillOnce(Return(true));
  display_compositor_->ImportBufferImage(image_metadata, BufferCollectionUsage::kClientImage);

  // Release buffer collection. Make sure that does not release Image.
  EXPECT_CALL(*mock_display_coordinator_, ReleaseImage(image_metadata.identifier)).Times(0);
  EXPECT_CALL(*mock_display_coordinator_, ReleaseBufferCollection(kGlobalBufferCollectionId))
      .WillOnce(Return());
  EXPECT_CALL(*renderer_, ReleaseBufferCollection(kGlobalBufferCollectionId, _)).WillOnce(Return());
  display_compositor_->ReleaseBufferCollection(kGlobalBufferCollectionId,
                                               BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::ConfigResult result =
            fuchsia::hardware::display::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  display_compositor_.reset();
}

TEST_F(DisplayCompositorTest, ImportImageErrorCases) {
  const allocation::GlobalBufferCollectionId kGlobalBufferCollectionId =
      allocation::GenerateUniqueBufferCollectionId();
  const allocation::GlobalImageId kImageId = allocation::GenerateUniqueImageId();
  const uint32_t kVmoCount = 2;
  const uint32_t kVmoIdx = 1;
  const uint32_t kMaxWidth = 100;
  const uint32_t kMaxHeight = 200;
  uint32_t num_times_import_image_called = 0;

  EXPECT_CALL(*mock_display_coordinator_, ImportBufferCollection(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t, fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(ZX_OK);
          }));

  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t collection_id, fuchsia::hardware::display::ImageConfig config,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(ZX_OK);
          }));
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId, fuchsia::sysmem::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                             BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        token_ref = std::move(token);
        return true;
      });

  // Wait once for call to ImportBufferCollection, once for setting
  // the buffer collection constraints, a single valid call to
  // ImportBufferImage() 1 invalid call to ImportBufferImage(), and a single
  // call to ReleaseBufferImage(). Although there are more than three
  // invalid calls to ImportBufferImage() below, only 3 of them make it
  // all the way to the display coordinator, which is why we only
  // have to wait 3 times. Finally add one call for the deleter.
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 6);

  display_compositor_->ImportBufferCollection(kGlobalBufferCollectionId, sysmem_allocator_.get(),
                                              CreateToken(), BufferCollectionUsage::kClientImage,
                                              std::nullopt);
  SetDisplaySupported(kGlobalBufferCollectionId, true);

  ImageMetadata metadata = {
      .collection_id = kGlobalBufferCollectionId,
      .identifier = kImageId,
      .vmo_index = kVmoIdx,
      .width = 20,
      .height = 30,
      .blend_mode = fuchsia::ui::composition::BlendMode::SRC,
  };

  // Make sure that the engine returns true if the display coordinator returns true.
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(_, kGlobalBufferCollectionId, kImageId, kVmoIdx, _))
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::ImageConfig image_config, uint64_t collection_id,
             uint64_t image_id, uint32_t index,
             MockDisplayCoordinator::ImportImageCallback callback) { callback(ZX_OK); }));

  EXPECT_CALL(*renderer_, ImportBufferImage(metadata, _)).WillOnce(Return(true));

  auto result =
      display_compositor_->ImportBufferImage(metadata, BufferCollectionUsage::kClientImage);
  EXPECT_TRUE(result);

  // Make sure we can release the image properly.
  EXPECT_CALL(*mock_display_coordinator_, ReleaseImage(kImageId)).WillOnce(Return());
  EXPECT_CALL(*renderer_, ReleaseBufferImage(metadata.identifier)).WillOnce(Return());

  display_compositor_->ReleaseBufferImage(metadata.identifier);

  // Make sure that the engine returns false if the display coordinator returns an error
  EXPECT_CALL(*mock_display_coordinator_, ImportImage(_, kGlobalBufferCollectionId, _, kVmoIdx, _))
      .WillOnce(testing::Invoke([](fuchsia::hardware::display::ImageConfig image_config,
                                   uint64_t collection_id, uint64_t image_id, uint32_t index,
                                   MockDisplayCoordinator::ImportImageCallback callback) {
        callback(ZX_ERR_INVALID_ARGS);
      }));

  // This should still return false for the engine even if the renderer returns true.
  EXPECT_CALL(*renderer_, ImportBufferImage(metadata, _)).WillOnce(Return(true));

  result = display_compositor_->ImportBufferImage(metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Collection ID can't be invalid. This shouldn't reach the display coordinator.
  EXPECT_CALL(*mock_display_coordinator_, ImportImage(_, kGlobalBufferCollectionId, _, kVmoIdx, _))
      .Times(0);
  auto copy_metadata = metadata;
  copy_metadata.collection_id = allocation::kInvalidId;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Image Id can't be 0. This shouldn't reach the display coordinator.
  EXPECT_CALL(*mock_display_coordinator_, ImportImage(_, kGlobalBufferCollectionId, _, kVmoIdx, _))
      .Times(0);
  copy_metadata = metadata;
  copy_metadata.identifier = allocation::kInvalidImageId;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Width can't be 0. This shouldn't reach the display coordinator.
  EXPECT_CALL(*mock_display_coordinator_, ImportImage(_, kGlobalBufferCollectionId, _, kVmoIdx, _))
      .Times(0);
  copy_metadata = metadata;
  copy_metadata.width = 0;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Height can't be 0. This shouldn't reach the display coordinator.
  EXPECT_CALL(*mock_display_coordinator_, ImportImage(_, _, _, 0, _)).Times(0);
  copy_metadata = metadata;
  copy_metadata.height = 0;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::ConfigResult result =
            fuchsia::hardware::display::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  display_compositor_.reset();
}

// This test checks that DisplayCompositor properly processes ConfigStamp from Vsync.
TEST_F(DisplayCompositorTest, VsyncConfigStampAreProcessed) {
  auto session = CreateSession();
  const TransformHandle root_handle = session.graph().CreateTransform();
  uint64_t display_id = 1;
  glm::uvec2 resolution(1024, 768);
  DisplayInfo display_info = {resolution, {kPixelFormat}};

  // We have to wait for times:
  // - 2 calls to DiscardConfig
  // - 2 calls to CheckConfig
  // - 2 calls to ApplyConfig
  // - 2 calls to GetLatestAppliedConfigStamp
  // - 1 call to DiscardConfig
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 9);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .WillRepeatedly(
          testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
            fuchsia::hardware::display::ConfigResult result =
                fuchsia::hardware::display::ConfigResult::OK;
            std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
            callback(result, ops);
          }));
  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig()).WillRepeatedly(Return());

  const uint64_t kConfigStamp1 = 234;
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCallback callback) {
            fuchsia::hardware::display::ConfigStamp stamp = {kConfigStamp1};
            callback(stamp);
          }));
  display_compositor_->RenderFrame(1, zx::time(1), {}, {}, [](const scheduling::Timestamps&) {});

  const uint64_t kConfigStamp2 = 123;
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCallback callback) {
            fuchsia::hardware::display::ConfigStamp stamp = {kConfigStamp2};
            callback(stamp);
          }));
  display_compositor_->RenderFrame(2, zx::time(2), {}, {}, [](const scheduling::Timestamps&) {});

  EXPECT_EQ(2u, GetPendingApplyConfigs().size());

  // Sending another vsync should be skipped.
  const uint64_t kConfigStamp3 = 345;
  SendOnVsyncEvent({kConfigStamp3});
  EXPECT_EQ(2u, GetPendingApplyConfigs().size());

  // Sending later vsync should signal and remove the earlier one too.
  SendOnVsyncEvent({kConfigStamp2});
  EXPECT_EQ(0u, GetPendingApplyConfigs().size());

  display_compositor_.reset();
}

// When compositing directly to a hardware display layer, the display coordinator
// takes in source and destination Frame object types, which mirrors flatland usage.
// The source frames are nonnormalized UV coordinates and the destination frames are
// screenspace coordinates given in pixels. So this test makes sure that the rectangle
// and frame data that is generated by flatland sends along to the display coordinator
// the proper source and destination frame data. Each source and destination frame pair
// should be added to its own layer on the display.
TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessTest) {
  const uint64_t kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();

  // Create a parent and child session.
  auto parent_session = CreateSession();
  auto child_session = CreateSession();

  // Create a link between the two.
  auto link_to_child = child_session.CreateView(parent_session);

  // Create the root handle for the parent and a handle that will have an image attached.
  const TransformHandle parent_root_handle = parent_session.graph().CreateTransform();
  const TransformHandle parent_image_handle = parent_session.graph().CreateTransform();

  // Add the two children to the parent root: link, then image.
  parent_session.graph().AddChild(parent_root_handle, link_to_child.GetInternalLinkHandle());
  parent_session.graph().AddChild(parent_root_handle, parent_image_handle);

  // Create an image handle for the child.
  const TransformHandle child_image_handle = child_session.graph().CreateTransform();

  // Attach that image handle to the child link transform handle.
  child_session.graph().AddChild(child_session.GetLinkChildTransformHandle(), child_image_handle);

  // Get an UberStruct for the parent session.
  auto parent_struct = parent_session.CreateUberStructWithCurrentTopology(parent_root_handle);

  // Add an image.
  ImageMetadata parent_image_metadata = ImageMetadata{
      .collection_id = kGlobalBufferCollectionId,
      .identifier = allocation::GenerateUniqueImageId(),
      .vmo_index = 0,
      .width = 128,
      .height = 256,
      .blend_mode = fuchsia::ui::composition::BlendMode::SRC,
  };
  parent_struct->images[parent_image_handle] = parent_image_metadata;

  parent_struct->local_matrices[parent_image_handle] =
      glm::scale(glm::translate(glm::mat3(1.0), glm::vec2(9, 13)), glm::vec2(10, 20));
  parent_struct->local_image_sample_regions[parent_image_handle] = {0, 0, 128, 256};

  // Submit the UberStruct.
  parent_session.PushUberStruct(std::move(parent_struct));

  // Get an UberStruct for the child session. Note that the argument will be ignored anyway.
  auto child_struct = child_session.CreateUberStructWithCurrentTopology(
      child_session.GetLinkChildTransformHandle());

  // Add an image.
  ImageMetadata child_image_metadata = ImageMetadata{
      .collection_id = kGlobalBufferCollectionId,
      .identifier = allocation::GenerateUniqueImageId(),
      .vmo_index = 1,
      .width = 512,
      .height = 1024,
      .blend_mode = fuchsia::ui::composition::BlendMode::SRC,
  };
  child_struct->images[child_image_handle] = child_image_metadata;
  child_struct->local_matrices[child_image_handle] =
      glm::scale(glm::translate(glm::mat3(1), glm::vec2(5, 7)), glm::vec2(30, 40));
  child_struct->local_image_sample_regions[child_image_handle] = {0, 0, 512, 1024};

  // Submit the UberStruct.
  child_session.PushUberStruct(std::move(child_struct));

  constexpr fuchsia::hardware::display::DisplayId kDisplayId = {.value = 1};
  glm::uvec2 resolution(1024, 768);

  // We will end up with 2 source frames, 2 destination frames, and two layers beind sent to the
  // display.
  fuchsia::hardware::display::Frame sources[2] = {
      {.x_pos = 0u, .y_pos = 0u, .width = 512, .height = 1024u},
      {.x_pos = 0u, .y_pos = 0u, .width = 128u, .height = 256u}};

  fuchsia::hardware::display::Frame destinations[2] = {
      {.x_pos = 5u, .y_pos = 7u, .width = 30, .height = 40u},
      {.x_pos = 9u, .y_pos = 13u, .width = 10u, .height = 20u}};

  // Since we have 2 rectangles with images with 1 buffer collection, we have to wait
  // for...:
  // - 2 calls for importing and setting constraints on the collection
  // - 2 calls to import the images
  // - 2 calls to create layers.
  // - 1 call to discard the config.
  // - 1 call to set the layers on the display
  // - 2 calls to import events for images.
  // - 2 calls to set each layer image
  // - 2 calls to set the layer primary config
  // - 2 calls to set the layer primary positions
  // - 2 calls to set the layer primary alpha.
  // - 1 call to SetDisplayColorConversion
  // - 1 call to check the config
  // - 1 call to apply the config
  // - 1 call to GetLatestAppliedConfigStamp
  // - 1 call to DiscardConfig
  // - 2 calls to destroy layer.
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 25);

  EXPECT_CALL(*mock_display_coordinator_, ImportBufferCollection(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t, fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(ZX_OK);
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t collection_id, fuchsia::hardware::display::ImageConfig config,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(ZX_OK);
          }));
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId, fuchsia::sysmem::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                             BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        token_ref = std::move(token);
        return true;
      });
  display_compositor_->ImportBufferCollection(kGlobalBufferCollectionId, sysmem_allocator_.get(),
                                              CreateToken(), BufferCollectionUsage::kClientImage,
                                              std::nullopt);
  SetDisplaySupported(kGlobalBufferCollectionId, true);

  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(_, kGlobalBufferCollectionId, parent_image_metadata.identifier, 0, _))
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::ImageConfig, uint64_t, uint64_t, uint32_t,
             MockDisplayCoordinator::ImportImageCallback callback) { callback(ZX_OK); }));

  EXPECT_CALL(*renderer_, ImportBufferImage(parent_image_metadata, _)).WillOnce(Return(true));

  display_compositor_->ImportBufferImage(parent_image_metadata,
                                         BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(_, kGlobalBufferCollectionId, child_image_metadata.identifier, 1, _))
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::ImageConfig, uint64_t, uint64_t, uint32_t,
             MockDisplayCoordinator::ImportImageCallback callback) { callback(ZX_OK); }));

  EXPECT_CALL(*renderer_, ImportBufferImage(child_image_metadata, _)).WillOnce(Return(true));
  display_compositor_->ImportBufferImage(child_image_metadata, BufferCollectionUsage::kClientImage);

  display_compositor_->SetColorConversionValues({1, 0, 0, 0, 1, 0, 0, 0, 1}, {0.1f, 0.2f, 0.3f},
                                                {-0.3f, -0.2f, -0.1f});

  // Setup the EXPECT_CALLs for gmock.
  uint64_t layer_id_value = 1;
  EXPECT_CALL(*mock_display_coordinator_, CreateLayer(_))
      .WillRepeatedly(testing::Invoke([&](MockDisplayCoordinator::CreateLayerCallback callback) {
        callback(ZX_OK, {.value = layer_id_value++});
      }));

  std::vector<fuchsia::hardware::display::LayerId> layers = {{.value = 1}, {.value = 2}};
  EXPECT_CALL(*mock_display_coordinator_,
              SetDisplayLayers(FidlEquals(kDisplayId),
                               testing::ElementsAre(FidlEquals(layers[0]), FidlEquals(layers[1]))))
      .Times(1);

  // Make sure each layer has all of its components set properly.
  uint64_t collection_ids[] = {child_image_metadata.identifier, parent_image_metadata.identifier};
  for (uint32_t i = 0; i < 2; i++) {
    EXPECT_CALL(*mock_display_coordinator_, SetLayerPrimaryConfig(FidlEquals(layers[i]), _))
        .Times(1);
    EXPECT_CALL(*mock_display_coordinator_,
                SetLayerPrimaryPosition(FidlEquals(layers[i]), fhd_Transform::IDENTITY, _, _))
        .WillOnce(testing::Invoke(
            [sources, destinations, index = i](fuchsia::hardware::display::LayerId layer_id,
                                               fuchsia::hardware::display::Transform transform,
                                               fuchsia::hardware::display::Frame src_frame,
                                               fuchsia::hardware::display::Frame dest_frame) {
              EXPECT_TRUE(fidl::Equals(src_frame, sources[index]));
              EXPECT_TRUE(fidl::Equals(dest_frame, destinations[index]));
            }));
    EXPECT_CALL(*mock_display_coordinator_, SetLayerPrimaryAlpha(FidlEquals(layers[i]), _, _))
        .Times(1);
    EXPECT_CALL(*mock_display_coordinator_,
                SetLayerImage(FidlEquals(layers[i]), collection_ids[i], _, _))
        .Times(1);
  }
  EXPECT_CALL(*mock_display_coordinator_, ImportEvent(_, _)).Times(2);

  EXPECT_CALL(*mock_display_coordinator_, SetDisplayColorConversion(_, _, _, _)).Times(1);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(false, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::ConfigResult result =
            fuchsia::hardware::display::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  EXPECT_CALL(*renderer_, ChoosePreferredPixelFormat(_));

  DisplayInfo display_info = {resolution, {kPixelFormat}};
  scenic_impl::display::Display display(kDisplayId, resolution.x, resolution.y);
  display_compositor_->AddDisplay(&display, display_info, /*num_vmos*/ 0,
                                  /*out_buffer_collection*/ nullptr);

  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig()).WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCallback callback) {
            fuchsia::hardware::display::ConfigStamp stamp = {1};
            callback(stamp);
          }));

  display_compositor_->RenderFrame(
      1, zx::time(1),
      GenerateDisplayListForTest({{kDisplayId.value, {display_info, parent_root_handle}}}), {},
      [](const scheduling::Timestamps&) {});

  for (uint32_t i = 0; i < 2; i++) {
    EXPECT_CALL(*mock_display_coordinator_, DestroyLayer(FidlEquals(layers[i])));
  }

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::ConfigResult result =
            fuchsia::hardware::display::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  display_compositor_.reset();
}

void DisplayCompositorTest::HardwareFrameCorrectnessWithRotationTester(
    glm::mat3 transform_matrix, ImageFlip image_flip,
    fuchsia::hardware::display::Frame expected_dst, fhd_Transform expected_transform) {
  const uint64_t kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();

  // Create a parent session.
  auto parent_session = CreateSession();

  // Create the root handle for the parent and a handle that will have an image attached.
  const TransformHandle parent_root_handle = parent_session.graph().CreateTransform();
  const TransformHandle parent_image_handle = parent_session.graph().CreateTransform();

  // Add the image to the parent.
  parent_session.graph().AddChild(parent_root_handle, parent_image_handle);

  // Get an UberStruct for the parent session.
  auto parent_struct = parent_session.CreateUberStructWithCurrentTopology(parent_root_handle);

  // Add an image.
  ImageMetadata parent_image_metadata = ImageMetadata{
      .collection_id = kGlobalBufferCollectionId,
      .identifier = allocation::GenerateUniqueImageId(),
      .vmo_index = 0,
      .width = 128,
      .height = 256,
      .blend_mode = fuchsia::ui::composition::BlendMode::SRC,
      .flip = image_flip,
  };
  parent_struct->images[parent_image_handle] = parent_image_metadata;

  parent_struct->local_matrices[parent_image_handle] = std::move(transform_matrix);
  parent_struct->local_image_sample_regions[parent_image_handle] = {0, 0, 128, 256};

  // Submit the UberStruct.
  parent_session.PushUberStruct(std::move(parent_struct));

  constexpr fuchsia::hardware::display::DisplayId kDisplayId = {.value = 1};
  glm::uvec2 resolution(1024, 768);

  // We will end up with 1 source frame, 1 destination frame, and one layer being sent to the
  // display.
  fuchsia::hardware::display::Frame source = {
      .x_pos = 0u, .y_pos = 0u, .width = 128u, .height = 256u};

  // Since we have 1 rectangles with images with 1 buffer collection, we have to wait
  // for...:
  // - 2 calls for importing and setting constraints on the collection
  // - 1 calls to import the images
  // - 2 calls to create layers (a new display creates two layers upfront).
  // - 1 call to discard the config.
  // - 1 call to set the layers on the display
  // - 1 calls to import events for images.
  // - 1 calls to set each layer image
  // - 1 calls to set the layer primary config
  // - 1 calls to set the layer primary positions
  // - 1 calls to set the layer primary alpha.
  // - 1 call to SetDisplayColorConversion
  // - 1 call to check the config
  // - 1 call to apply the config
  // - 1 call to GetLatestAppliedConfigStamp
  // - 1 call to DiscardConfig
  // - 1 calls to destroy layer.
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 18);

  EXPECT_CALL(*mock_display_coordinator_, ImportBufferCollection(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t, fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(ZX_OK);
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t collection_id, fuchsia::hardware::display::ImageConfig config,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(ZX_OK);
          }));
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId, fuchsia::sysmem::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                             BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        token_ref = std::move(token);
        return true;
      });
  display_compositor_->ImportBufferCollection(kGlobalBufferCollectionId, sysmem_allocator_.get(),
                                              CreateToken(), BufferCollectionUsage::kClientImage,
                                              std::nullopt);
  SetDisplaySupported(kGlobalBufferCollectionId, true);

  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(_, kGlobalBufferCollectionId, parent_image_metadata.identifier, 0, _))
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::ImageConfig, uint64_t, uint64_t, uint32_t,
             MockDisplayCoordinator::ImportImageCallback callback) { callback(ZX_OK); }));

  EXPECT_CALL(*renderer_, ImportBufferImage(parent_image_metadata, _)).WillOnce(Return(true));

  display_compositor_->ImportBufferImage(parent_image_metadata,
                                         BufferCollectionUsage::kClientImage);

  display_compositor_->SetColorConversionValues({1, 0, 0, 0, 1, 0, 0, 0, 1}, {0.1f, 0.2f, 0.3f},
                                                {-0.3f, -0.2f, -0.1f});

  // Setup the EXPECT_CALLs for gmock.
  // Note that a couple of layers are created upfront for the display.
  uint64_t layer_id_value = 1;
  EXPECT_CALL(*mock_display_coordinator_, CreateLayer(_))
      .WillRepeatedly(testing::Invoke([&](MockDisplayCoordinator::CreateLayerCallback callback) {
        callback(ZX_OK, {.value = layer_id_value++});
      }));

  // However, we only set one display layer for the image.
  std::vector<fuchsia::hardware::display::LayerId> layers = {{.value = 1}};
  EXPECT_CALL(*mock_display_coordinator_,
              SetDisplayLayers(FidlEquals(kDisplayId), testing::ElementsAre(FidlEquals(layers[0]))))
      .Times(1);

  uint64_t collection_id = parent_image_metadata.identifier;
  EXPECT_CALL(*mock_display_coordinator_, SetLayerPrimaryConfig(FidlEquals(layers[0]), _)).Times(1);
  EXPECT_CALL(*mock_display_coordinator_,
              SetLayerPrimaryPosition(FidlEquals(layers[0]), expected_transform, _, _))
      .WillOnce(
          testing::Invoke([source, expected_dst](fuchsia::hardware::display::LayerId layer_id,
                                                 fuchsia::hardware::display::Transform transform,
                                                 fuchsia::hardware::display::Frame src_frame,
                                                 fuchsia::hardware::display::Frame dest_frame) {
            EXPECT_TRUE(fidl::Equals(src_frame, source));
            EXPECT_TRUE(fidl::Equals(dest_frame, expected_dst));
          }));
  EXPECT_CALL(*mock_display_coordinator_, SetLayerPrimaryAlpha(FidlEquals(layers[0]), _, _))
      .Times(1);
  EXPECT_CALL(*mock_display_coordinator_, SetLayerImage(FidlEquals(layers[0]), collection_id, _, _))
      .Times(1);
  EXPECT_CALL(*mock_display_coordinator_, ImportEvent(_, _)).Times(1);

  EXPECT_CALL(*mock_display_coordinator_, SetDisplayColorConversion(_, _, _, _)).Times(1);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(false, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::ConfigResult result =
            fuchsia::hardware::display::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  EXPECT_CALL(*renderer_, ChoosePreferredPixelFormat(_));

  DisplayInfo display_info = {resolution, {kPixelFormat}};
  scenic_impl::display::Display display(kDisplayId, resolution.x, resolution.y);
  display_compositor_->AddDisplay(&display, display_info, /*num_vmos*/ 0,
                                  /*out_buffer_collection*/ nullptr);

  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig()).WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCallback callback) {
            fuchsia::hardware::display::ConfigStamp stamp = {1};
            callback(stamp);
          }));

  display_compositor_->RenderFrame(
      1, zx::time(1),
      GenerateDisplayListForTest({{kDisplayId.value, {display_info, parent_root_handle}}}), {},
      [](const scheduling::Timestamps&) {});

  for (uint64_t i = 1; i < layer_id_value; ++i) {
    EXPECT_CALL(*mock_display_coordinator_, DestroyLayer(testing::FieldsAre(i)));
  }

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::ConfigResult result =
            fuchsia::hardware::display::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  display_compositor_.reset();
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWith90DegreeRotationTest) {
  // After scale and 90 CCW rotation, the new top-left corner would be (0, -10). Translate back to
  // position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(0, 10));
  matrix = glm::rotate(matrix, -glm::half_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::NONE, expected_dst,
                                             fhd_Transform::ROT_90);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWith180DegreeRotationTest) {
  // After scale and 180 CCW rotation, the new top-left corner would be (-10, -20). Translate back
  // to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(10, 20));
  matrix = glm::rotate(matrix, -glm::pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 10u, .height = 20u};

  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::NONE, expected_dst,
                                             fhd_Transform::ROT_180);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWith270DegreeRotationTest) {
  // After scale and 270 CCW rotation, the new top-left corner would be (-20, 0). Translate back to
  // position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(20, 0));
  matrix = glm::rotate(matrix, -glm::three_over_two_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::NONE, expected_dst,
                                             fhd_Transform::ROT_270);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlipTest) {
  glm::mat3 matrix = glm::scale(glm::mat3(), glm::vec2(10, 20));

  fuchsia::hardware::display::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 10u, .height = 20u};

  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::LEFT_RIGHT, expected_dst,
                                             fhd_Transform::REFLECT_Y);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlipTest) {
  glm::mat3 matrix = glm::scale(glm::mat3(), glm::vec2(10, 20));

  fuchsia::hardware::display::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 10u, .height = 20u};

  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::UP_DOWN, expected_dst,
                                             fhd_Transform::REFLECT_X);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlip90DegreeRotationTest) {
  // After scale and 90 CCW rotation, the new top-left corner would be (0, -10). Translate back to
  // position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(0, 10));
  matrix = glm::rotate(matrix, -glm::half_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::LEFT_RIGHT, expected_dst,
                                             fhd_Transform::ROT_90_REFLECT_X);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlip90DegreeRotationTest) {
  // After scale and 90 CCW rotation, the new top-left corner would be (0, -10). Translate back to
  // position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(0, 10));
  matrix = glm::rotate(matrix, -glm::half_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::UP_DOWN, expected_dst,
                                             fhd_Transform::ROT_90_REFLECT_Y);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlip180DegreeRotationTest) {
  // After scale and 180 CCW rotation, the new top-left corner would be (-10, -20). Translate back
  // to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(10, 20));
  matrix = glm::rotate(matrix, -glm::pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 10u, .height = 20u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::LEFT_RIGHT, expected_dst,
                                             fhd_Transform::REFLECT_X);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlip180DegreeRotationTest) {
  // After scale and 180 CCW rotation, the new top-left corner would be (-10, -20). Translate back
  // to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(10, 20));
  matrix = glm::rotate(matrix, -glm::pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 10u, .height = 20u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::UP_DOWN, expected_dst,
                                             fhd_Transform::REFLECT_Y);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlip270DegreeRotationTest) {
  // After scale and 270 CCW rotation, the new top-left corner would be (-20, 0). Translate back to
  // position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(20, 0));
  matrix = glm::rotate(matrix, -glm::three_over_two_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::LEFT_RIGHT, expected_dst,
                                             fhd_Transform::ROT_90_REFLECT_Y);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlip270DegreeRotationTest) {
  // After scale and 270 CCW rotation, the new top-left corner would be (-20, 0). Translate back to
  // position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(20, 0));
  matrix = glm::rotate(matrix, -glm::three_over_two_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::UP_DOWN, expected_dst,
                                             fhd_Transform::ROT_90_REFLECT_X);
}

TEST_F(DisplayCompositorTest, ChecksDisplayImageSignalFences) {
  const uint64_t kGlobalBufferCollectionId = 1;
  auto session = CreateSession();

  // Create the root handle and a handle that will have an image attached.
  const TransformHandle root_handle = session.graph().CreateTransform();
  const TransformHandle image_handle = session.graph().CreateTransform();
  session.graph().AddChild(root_handle, image_handle);

  // Get an UberStruct for the session.
  auto uber_struct = session.CreateUberStructWithCurrentTopology(root_handle);

  // Add an image.
  ImageMetadata image_metadata = ImageMetadata{
      .collection_id = kGlobalBufferCollectionId,
      .identifier = allocation::GenerateUniqueImageId(),
      .vmo_index = 0,
      .width = 128,
      .height = 256,
      .blend_mode = fuchsia::ui::composition::BlendMode::SRC,
  };
  uber_struct->images[image_handle] = image_metadata;
  uber_struct->local_matrices[image_handle] =
      glm::scale(glm::translate(glm::mat3(1.0), glm::vec2(9, 13)), glm::vec2(10, 20));
  uber_struct->local_image_sample_regions[image_handle] = {0, 0, 128, 256};

  // Submit the UberStruct.
  session.PushUberStruct(std::move(uber_struct));

  // Since we have 1 rectangles with image with 1 buffer collection, we have to wait
  // for...:
  // - 2 call for importing and setting constraints on the collection.
  // - 2 call to create layers.
  // - 1 call to import the image.
  // - 1 call to discard the config.
  // - 1 call to set the layers on the display.
  // - 1 call to import event for image.
  // - 1 call to set the layer image.
  // - 1 call to set the layer primary config.
  // - 1 call to set the layer primary alpha.
  // - 1 call to set the layer primary position.
  // - 1 call to check the config.
  // - 1 call to apply the config.
  // - 1 call to GetLatestAppliedConfigStamp
  // - 2 calls to discard the config.
  // - 1 call to discard the config.
  // - 2 calls to destroy layer.
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 21);

  // Import buffer collection.
  EXPECT_CALL(*mock_display_coordinator_, ImportBufferCollection(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t, fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(ZX_OK);
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(kGlobalBufferCollectionId, _, _))
      .WillOnce(testing::Invoke(
          [](uint64_t collection_id, fuchsia::hardware::display::ImageConfig config,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(ZX_OK);
          }));
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId, fuchsia::sysmem::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                             BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        token_ref = std::move(token);
        return true;
      });
  display_compositor_->ImportBufferCollection(kGlobalBufferCollectionId, sysmem_allocator_.get(),
                                              CreateToken(), BufferCollectionUsage::kClientImage,
                                              std::nullopt);
  SetDisplaySupported(kGlobalBufferCollectionId, true);

  // Import image.
  EXPECT_CALL(*mock_display_coordinator_, ImportImage(_, kGlobalBufferCollectionId, _, 0, _))
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::ImageConfig, uint64_t, uint64_t, uint32_t,
             MockDisplayCoordinator::ImportImageCallback callback) { callback(ZX_OK); }));

  EXPECT_CALL(*renderer_, ImportBufferImage(image_metadata, _)).WillOnce(Return(true));
  display_compositor_->ImportBufferImage(image_metadata, BufferCollectionUsage::kClientImage);

  // We start the frame by clearing the config.
  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(true, _))
      .WillRepeatedly(
          testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
            fuchsia::hardware::display::ConfigResult result =
                fuchsia::hardware::display::ConfigResult::OK;
            std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
            callback(result, ops);
          }));

  // Set expectation for CreateLayer calls.
  uint64_t layer_id_value = 1;
  std::vector<fuchsia::hardware::display::LayerId> layers = {{.value = 1}, {.value = 2}};
  EXPECT_CALL(*mock_display_coordinator_, CreateLayer(_))
      .Times(2)
      .WillRepeatedly(testing::Invoke([&](MockDisplayCoordinator::CreateLayerCallback callback) {
        callback(ZX_OK, {.value = layer_id_value++});
      }));
  EXPECT_CALL(*renderer_, ChoosePreferredPixelFormat(_));

  // Add display.
  constexpr fuchsia::hardware::display::DisplayId kDisplayId = {.value = 1};
  glm::uvec2 kResolution(1024, 768);
  DisplayInfo display_info = {kResolution, {kPixelFormat}};
  scenic_impl::display::Display display(kDisplayId, kResolution.x, kResolution.y);
  display_compositor_->AddDisplay(&display, display_info, /*num_vmos*/ 0,
                                  /*out_buffer_collection*/ nullptr);

  // Set expectation for rendering image on layer.
  std::vector<fuchsia::hardware::display::LayerId> active_layers = {{.value = 1}};
  zx::event imported_event;
  EXPECT_CALL(*mock_display_coordinator_, ImportEvent(_, _))
      .WillOnce(testing::Invoke(
          [&imported_event](zx::event event, uint64_t) { imported_event = std::move(event); }));
  EXPECT_CALL(
      *mock_display_coordinator_,
      SetDisplayLayers(FidlEquals(kDisplayId), testing::ElementsAre(FidlEquals(active_layers[0]))))
      .Times(1);
  EXPECT_CALL(*mock_display_coordinator_, SetLayerPrimaryConfig(FidlEquals(layers[0]), _)).Times(1);
  EXPECT_CALL(*mock_display_coordinator_, SetLayerPrimaryPosition(FidlEquals(layers[0]), _, _, _))
      .Times(1);
  EXPECT_CALL(*mock_display_coordinator_, SetLayerPrimaryAlpha(FidlEquals(layers[0]), _, _))
      .Times(1);
  EXPECT_CALL(*mock_display_coordinator_, SetLayerImage(FidlEquals(layers[0]), _, _, _)).Times(1);
  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(false, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::ConfigResult result =
            fuchsia::hardware::display::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));
  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig()).WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCallback callback) {
            fuchsia::hardware::display::ConfigStamp stamp = {1};
            callback(stamp);
          }));

  // Render image. This should end up in display.
  const auto& display_list =
      GenerateDisplayListForTest({{kDisplayId.value, {display_info, root_handle}}});
  display_compositor_->RenderFrame(1, zx::time(1), display_list, {},
                                   [](const scheduling::Timestamps&) {});

  // Try rendering again. Because |imported_event| isn't signaled and no render targets were created
  // when adding display, we should fail.
  auto status = imported_event.wait_one(ZX_EVENT_SIGNALED, zx::time(), nullptr);
  EXPECT_NE(status, ZX_OK);
  display_compositor_->RenderFrame(1, zx::time(1), display_list, {},
                                   [](const scheduling::Timestamps&) {});

  for (uint32_t i = 0; i < 2; i++) {
    EXPECT_CALL(*mock_display_coordinator_, DestroyLayer(FidlEquals(layers[i])));
  }
  display_compositor_.reset();
}

// Tests that RenderOnly mode does not attempt to ImportBufferCollection() to display.
TEST_F(DisplayCompositorTest, RendererOnly_ImportAndReleaseBufferCollectionTest) {
  ForceRendererOnlyMode(true);

  // Wait once for call to ReleaseBufferCollection and once for the deleter.
  const auto server = CreateServerWaitingForMessages(*mock_display_coordinator_, 2);

  const allocation::GlobalBufferCollectionId kGlobalBufferCollectionId = 15;

  EXPECT_CALL(*mock_display_coordinator_, ImportBufferCollection(kGlobalBufferCollectionId, _, _))
      .Times(0);
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId, fuchsia::sysmem::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                             BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        token_ref = std::move(token);
        return true;
      });
  display_compositor_->ImportBufferCollection(kGlobalBufferCollectionId, sysmem_allocator_.get(),
                                              CreateToken(), BufferCollectionUsage::kClientImage,
                                              std::nullopt);

  EXPECT_CALL(*mock_display_coordinator_, ReleaseBufferCollection(kGlobalBufferCollectionId))
      .WillOnce(Return());
  EXPECT_CALL(*renderer_, ReleaseBufferCollection(kGlobalBufferCollectionId, _)).WillOnce(Return());
  display_compositor_->ReleaseBufferCollection(kGlobalBufferCollectionId,
                                               BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::ConfigResult result =
            fuchsia::hardware::display::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));
  display_compositor_.reset();
}

}  // namespace flatland::test
