// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/hardware/display/types/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>
#include <lib/zx/time.h>

#include <memory>

#include <gmock/gmock.h>

#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/fxl/strings/join_strings.h"
#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"
#include "src/ui/scenic/lib/allocation/id.h"
#include "src/ui/scenic/lib/display/tests/mock_display_coordinator.h"
#include "src/ui/scenic/lib/flatland/buffers/util.h"
#include "src/ui/scenic/lib/flatland/engine/tests/common.h"
#include "src/ui/scenic/lib/flatland/engine/tests/mock_display_coordinator.h"
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
using fhd_Transform = fuchsia::hardware::display::types::Transform;
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

bool RunWithTimeoutOrUntil(fit::function<bool()> condition, zx::duration timeout,
                           zx::duration step) {
  zx::duration wait_time = zx::msec(0);
  while (wait_time <= timeout) {
    if (condition())
      return true;
    zx::nanosleep(zx::deadline_after(step));
    wait_time += step;
  }

  return condition();
}

}  // namespace

class DisplayCompositorTest : public DisplayCompositorTestBase {
 public:
  void SetUp() override {
    DisplayCompositorTestBase::SetUp();

    sysmem_allocator_ = utils::CreateSysmemAllocatorSyncPtr("DisplayCompositorTest");

    renderer_ = std::make_shared<flatland::MockRenderer>();

    zx::channel coordinator_channel_server;
    zx::channel coordinator_channel_client;
    FX_CHECK(ZX_OK ==
             zx::channel::create(0, &coordinator_channel_server, &coordinator_channel_client));

    mock_display_coordinator_ =
        std::make_unique<testing::StrictMock<flatland::MockDisplayCoordinator>>(
            std::move(coordinator_channel_server), display_coordinator_loop_.dispatcher());
    display_coordinator_loop_.StartThread("display-coordinator-loop");

    auto shared_display_coordinator =
        std::make_shared<fuchsia::hardware::display::CoordinatorSyncPtr>();
    shared_display_coordinator->Bind(std::move(coordinator_channel_client));

    display_compositor_ = std::make_shared<flatland::DisplayCompositor>(
        dispatcher(), std::move(shared_display_coordinator), renderer_,
        utils::CreateSysmemAllocatorSyncPtr("display_compositor_unittest"),
        /*enable_display_composition*/ true, /*max_display_layers=*/2);
  }

  void TearDown() override {
    renderer_.reset();
    display_compositor_.reset();

    // This is to make sure that the display coordinator loop has finished
    // handling all its pending tasks.
    ASSERT_TRUE(RunWithTimeoutOrUntil([&] { return !mock_display_coordinator_->IsBound(); },
                                      /*timeout=*/zx::sec(5), /*step=*/zx::msec(5)));
    display_coordinator_loop_.Shutdown();
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

  void SendOnVsyncEvent(fuchsia::hardware::display::types::ConfigStamp stamp) {
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
      fuchsia_images2::PixelFormat::kB8G8R8A8;

  async::Loop display_coordinator_loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::unique_ptr<flatland::MockDisplayCoordinator> mock_display_coordinator_;
  std::shared_ptr<flatland::MockRenderer> renderer_;
  std::shared_ptr<flatland::DisplayCompositor> display_compositor_;

  // Only for use on the main thread. Establish a new connection when on the MockDisplayCoordinator
  // thread.
  fuchsia::sysmem::AllocatorSyncPtr sysmem_allocator_;

  void HardwareFrameCorrectnessWithRotationTester(
      glm::mat3 transform_matrix, ImageFlip image_flip,
      fuchsia::hardware::display::types::Frame expected_dst, fhd_Transform expected_transform);
};

// TODO(https://fxbug.dev/324688770): Dispatch all DisplayCompositor methods
// to the test loop.

TEST_F(DisplayCompositorTest, ImportAndReleaseBufferCollectionTest) {
  constexpr allocation::GlobalBufferCollectionId kGlobalBufferCollectionId = 15;
  constexpr fuchsia::hardware::display::BufferCollectionId kDisplayBufferCollectionId =
      allocation::ToDisplayBufferCollectionId(kGlobalBufferCollectionId);

  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(
          testing::Invoke([](fuchsia::hardware::display::BufferCollectionId,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
                             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(
                fuchsia::hardware::display::Coordinator_ImportBufferCollection_Result::WithResponse(
                    {}));
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::BufferCollectionId collection_id,
             fuchsia::hardware::display::types::ImageBufferUsage image_buffer_usage,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(fuchsia::hardware::display::Coordinator_SetBufferCollectionConstraints_Result::
                         WithResponse({}));
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

  EXPECT_CALL(*mock_display_coordinator_,
              ReleaseBufferCollection(FidlEquals(kDisplayBufferCollectionId)))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(*renderer_, ReleaseBufferCollection(kGlobalBufferCollectionId, _)).WillOnce(Return());
  display_compositor_->ReleaseBufferCollection(kGlobalBufferCollectionId,
                                               BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::types::ConfigResult result =
            fuchsia::hardware::display::types::ConfigResult::OK;
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
  const fuchsia::hardware::display::BufferCollectionId kDisplayBufferCollectionId =
      allocation::ToDisplayBufferCollectionId(kGlobalBufferCollectionId);

  fuchsia::sysmem::BufferCollectionTokenSyncPtr display_token;
  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&display_token](fuchsia::hardware::display::BufferCollectionId,
                           fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                           MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            display_token = token.BindSync();
            callback(
                fuchsia::hardware::display::Coordinator_ImportBufferCollection_Result::WithResponse(
                    {}));
          }));

  // Set display constraints.
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&display_token](
              fuchsia::hardware::display::BufferCollectionId collection_id,
              fuchsia::hardware::display::types::ImageBufferUsage image_buffer_usage,
              MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            auto sysmem_allocator = utils::CreateSysmemAllocatorSyncPtr("MockDisplayCoordinator");
            SetConstraintsAndClose(sysmem_allocator, std::move(display_token),
                                   fuchsia::sysmem::BufferCollectionConstraints{
                                       .usage{.cpu = fuchsia::sysmem::cpuUsageWrite},
                                       .min_buffer_count = 2,
                                       .max_buffer_count = 3,
                                   });
            callback(fuchsia::hardware::display::Coordinator_SetBufferCollectionConstraints_Result::
                         WithResponse({}));
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(_, testing::FieldsAre(FidlEquals(kDisplayBufferCollectionId), 0), _, _))
      .Times(1)
      .WillOnce(testing::Invoke([](fuchsia::hardware::display::types::ImageMetadata,
                                   fuchsia::hardware::display::BufferId,
                                   fuchsia::hardware::display::ImageId,
                                   MockDisplayCoordinator::ImportImageCallback callback) {
        callback(fuchsia::hardware::display::Coordinator_ImportImage_Result::WithResponse({}));
      }));
  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        callback(fuchsia::hardware::display::types::ConfigResult::OK, /*ops=*/{});
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
  const fuchsia::hardware::display::BufferCollectionId kDisplayBufferCollectionId =
      allocation::ToDisplayBufferCollectionId(kGlobalBufferCollectionId);

  fuchsia::sysmem::BufferCollectionTokenSyncPtr display_token;
  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&display_token](fuchsia::hardware::display::BufferCollectionId,
                           fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                           MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            display_token = token.BindSync();
            callback(
                fuchsia::hardware::display::Coordinator_ImportBufferCollection_Result::WithResponse(
                    {}));
          }));

  // Set display constraints.
  fuchsia::sysmem::BufferCollectionSyncPtr display_collection;
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&display_token](
              fuchsia::hardware::display::BufferCollectionId collection_id,
              fuchsia::hardware::display::types::ImageBufferUsage image_buffer_usage,
              MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            auto sysmem_allocator = utils::CreateSysmemAllocatorSyncPtr("MockDisplayCoordinator");
            SetConstraintsAndClose(sysmem_allocator, std::move(display_token),
                                   fuchsia::sysmem::BufferCollectionConstraints{
                                       .usage{.cpu = fuchsia::sysmem::cpuUsageWrite},
                                       .min_buffer_count = 1,
                                       .max_buffer_count = 1,
                                   });
            callback(fuchsia::hardware::display::Coordinator_SetBufferCollectionConstraints_Result::
                         WithResponse({}));
          }));
  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        callback(fuchsia::hardware::display::types::ConfigResult::OK, /*ops=*/{});
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
  const fuchsia::hardware::display::BufferCollectionId kDisplayBufferCollectionId =
      allocation::ToDisplayBufferCollectionId(kGlobalBufferCollectionId);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        callback(fuchsia::hardware::display::types::ConfigResult::OK, /*ops=*/{});
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
  const auto kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();
  const fuchsia::hardware::display::BufferCollectionId kDisplayBufferCollectionId =
      allocation::ToDisplayBufferCollectionId(kGlobalBufferCollectionId);

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
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::types::ConfigResult result =
            fuchsia::hardware::display::types::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));
  display_compositor_.reset();
}

TEST_F(DisplayCompositorTest, ImageIsValidAfterReleaseBufferCollection) {
  const auto kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();
  const fuchsia::hardware::display::BufferCollectionId kDisplayBufferCollectionId =
      allocation::ToDisplayBufferCollectionId(kGlobalBufferCollectionId);

  // Import buffer collection.
  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(
          testing::Invoke([](fuchsia::hardware::display::BufferCollectionId,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
                             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(
                fuchsia::hardware::display::Coordinator_ImportBufferCollection_Result::WithResponse(
                    {}));
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::BufferCollectionId collection_id,
             fuchsia::hardware::display::types::ImageBufferUsage image_buffer_usage,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(fuchsia::hardware::display::Coordinator_SetBufferCollectionConstraints_Result::
                         WithResponse({}));
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
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(_, testing::FieldsAre(FidlEquals(kDisplayBufferCollectionId), 0), _, _))
      .Times(1)
      .WillOnce(testing::Invoke([](fuchsia::hardware::display::types::ImageMetadata,
                                   fuchsia::hardware::display::BufferId,
                                   fuchsia::hardware::display::ImageId,
                                   MockDisplayCoordinator::ImportImageCallback callback) {
        callback(fuchsia::hardware::display::Coordinator_ImportImage_Result::WithResponse({}));
      }));
  EXPECT_CALL(*renderer_, ImportBufferImage(image_metadata, _)).WillOnce(Return(true));
  display_compositor_->ImportBufferImage(image_metadata, BufferCollectionUsage::kClientImage);

  // Release buffer collection. Make sure that does not release Image.
  const fuchsia::hardware::display::ImageId kFidlImageId =
      allocation::ToFidlImageId(image_metadata.identifier);
  EXPECT_CALL(*mock_display_coordinator_, ReleaseImage(FidlEquals(kFidlImageId))).Times(0);
  EXPECT_CALL(*mock_display_coordinator_,
              ReleaseBufferCollection(FidlEquals(kDisplayBufferCollectionId)))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(*renderer_, ReleaseBufferCollection(kGlobalBufferCollectionId, _)).WillOnce(Return());
  display_compositor_->ReleaseBufferCollection(kGlobalBufferCollectionId,
                                               BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::types::ConfigResult result =
            fuchsia::hardware::display::types::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  display_compositor_.reset();
}

TEST_F(DisplayCompositorTest, ImportImageErrorCases) {
  const allocation::GlobalBufferCollectionId kGlobalBufferCollectionId =
      allocation::GenerateUniqueBufferCollectionId();
  const fuchsia::hardware::display::BufferCollectionId kDisplayBufferCollectionId =
      allocation::ToDisplayBufferCollectionId(kGlobalBufferCollectionId);

  const allocation::GlobalImageId kImageId = allocation::GenerateUniqueImageId();
  const fuchsia::hardware::display::ImageId kFidlImageId = allocation::ToFidlImageId(kImageId);
  const uint32_t kVmoCount = 2;
  const uint32_t kVmoIdx = 1;
  const uint32_t kMaxWidth = 100;
  const uint32_t kMaxHeight = 200;
  uint32_t num_times_import_image_called = 0;

  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(FidlEquals(kDisplayBufferCollectionId), _, _))
      .WillOnce(
          testing::Invoke([](fuchsia::hardware::display::BufferCollectionId,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
                             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(
                fuchsia::hardware::display::Coordinator_ImportBufferCollection_Result::WithResponse(
                    {}));
          }));

  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(FidlEquals(kDisplayBufferCollectionId), _, _))
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::BufferCollectionId collection_id,
             fuchsia::hardware::display::types::ImageBufferUsage image_buffer_usage,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(fuchsia::hardware::display::Coordinator_SetBufferCollectionConstraints_Result::
                         WithResponse({}));
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
              ImportImage(_, testing::FieldsAre(FidlEquals(kDisplayBufferCollectionId), kVmoIdx),
                          FidlEquals(kFidlImageId), _))
      .Times(1)
      .WillOnce(testing::Invoke([](fuchsia::hardware::display::types::ImageMetadata image_metadata,
                                   fuchsia::hardware::display::BufferId buffer_id,
                                   fuchsia::hardware::display::ImageId image_id,
                                   MockDisplayCoordinator::ImportImageCallback callback) {
        callback(fuchsia::hardware::display::Coordinator_ImportImage_Result::WithResponse({}));
      }));

  EXPECT_CALL(*renderer_, ImportBufferImage(metadata, _)).WillOnce(Return(true));

  auto result =
      display_compositor_->ImportBufferImage(metadata, BufferCollectionUsage::kClientImage);
  EXPECT_TRUE(result);

  // Make sure we can release the image properly.
  EXPECT_CALL(*mock_display_coordinator_, ReleaseImage(FidlEquals(kFidlImageId)))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(*renderer_, ReleaseBufferImage(metadata.identifier)).WillOnce(Return());

  display_compositor_->ReleaseBufferImage(metadata.identifier);

  // Make sure that the engine returns false if the display coordinator returns an error
  EXPECT_CALL(
      *mock_display_coordinator_,
      ImportImage(_, testing::FieldsAre(FidlEquals(kDisplayBufferCollectionId), kVmoIdx), _, _))
      .Times(1)
      .WillOnce(testing::Invoke([](fuchsia::hardware::display::types::ImageMetadata image_metadata,
                                   fuchsia::hardware::display::BufferId buffer_id,
                                   fuchsia::hardware::display::ImageId image_id,
                                   MockDisplayCoordinator::ImportImageCallback callback) {
        callback(fuchsia::hardware::display::Coordinator_ImportImage_Result::WithErr(
            ZX_ERR_INVALID_ARGS));
      }));

  // This should still return false for the engine even if the renderer returns true.
  EXPECT_CALL(*renderer_, ImportBufferImage(metadata, _)).WillOnce(Return(true));

  result = display_compositor_->ImportBufferImage(metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Collection ID can't be invalid. This shouldn't reach the display coordinator.
  EXPECT_CALL(
      *mock_display_coordinator_,
      ImportImage(_, testing::FieldsAre(FidlEquals(kDisplayBufferCollectionId), kVmoIdx), _, _))
      .Times(0);
  auto copy_metadata = metadata;
  copy_metadata.collection_id = allocation::kInvalidId;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Image Id can't be 0. This shouldn't reach the display coordinator.
  EXPECT_CALL(
      *mock_display_coordinator_,
      ImportImage(_, testing::FieldsAre(FidlEquals(kDisplayBufferCollectionId), kVmoIdx), _, _))
      .Times(0);
  copy_metadata = metadata;
  copy_metadata.identifier = allocation::kInvalidImageId;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Width can't be 0. This shouldn't reach the display coordinator.
  EXPECT_CALL(
      *mock_display_coordinator_,
      ImportImage(_, testing::FieldsAre(FidlEquals(kDisplayBufferCollectionId), kVmoIdx), _, _))
      .Times(0);
  copy_metadata = metadata;
  copy_metadata.width = 0;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Height can't be 0. This shouldn't reach the display coordinator.
  EXPECT_CALL(*mock_display_coordinator_, ImportImage(_, testing::FieldsAre(_, 0), _, _)).Times(0);
  copy_metadata = metadata;
  copy_metadata.height = 0;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::types::ConfigResult result =
            fuchsia::hardware::display::types::ConfigResult::OK;
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

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(3)
      .WillRepeatedly(
          testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
            fuchsia::hardware::display::types::ConfigResult result =
                fuchsia::hardware::display::types::ConfigResult::OK;
            std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
            callback(result, ops);
          }));
  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig()).Times(2).WillRepeatedly(Return());

  const uint64_t kConfigStamp1 = 234;
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCallback callback) {
            fuchsia::hardware::display::types::ConfigStamp stamp = {kConfigStamp1};
            callback(stamp);
          }));
  display_compositor_->RenderFrame(1, zx::time(1), {}, {}, [](const scheduling::Timestamps&) {});

  const uint64_t kConfigStamp2 = 123;
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCallback callback) {
            fuchsia::hardware::display::types::ConfigStamp stamp = {kConfigStamp2};
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
  const fuchsia::hardware::display::BufferCollectionId kDisplayBufferCollectionId =
      allocation::ToDisplayBufferCollectionId(kGlobalBufferCollectionId);

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

  constexpr fuchsia::hardware::display::types::DisplayId kDisplayId = {.value = 1};
  glm::uvec2 resolution(1024, 768);

  // We will end up with 2 source frames, 2 destination frames, and two layers beind sent to the
  // display.
  fuchsia::hardware::display::types::Frame sources[2] = {
      {.x_pos = 0u, .y_pos = 0u, .width = 512, .height = 1024u},
      {.x_pos = 0u, .y_pos = 0u, .width = 128u, .height = 256u}};

  fuchsia::hardware::display::types::Frame destinations[2] = {
      {.x_pos = 5u, .y_pos = 7u, .width = 30, .height = 40u},
      {.x_pos = 9u, .y_pos = 13u, .width = 10u, .height = 20u}};

  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(
          testing::Invoke([](fuchsia::hardware::display::BufferCollectionId,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
                             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(
                fuchsia::hardware::display::Coordinator_ImportBufferCollection_Result::WithResponse(
                    {}));
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::BufferCollectionId collection_id,
             fuchsia::hardware::display::types::ImageBufferUsage image_buffer_usage,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(fuchsia::hardware::display::Coordinator_SetBufferCollectionConstraints_Result::
                         WithResponse({}));
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

  const fuchsia::hardware::display::ImageId fidl_parent_image_id =
      allocation::ToFidlImageId(parent_image_metadata.identifier);
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(_, testing::FieldsAre(FidlEquals(kDisplayBufferCollectionId), 0),
                          FidlEquals(fidl_parent_image_id), _))
      .Times(1)
      .WillOnce(testing::Invoke([](fuchsia::hardware::display::types::ImageMetadata,
                                   fuchsia::hardware::display::BufferId,
                                   fuchsia::hardware::display::ImageId,
                                   MockDisplayCoordinator::ImportImageCallback callback) {
        callback(fuchsia::hardware::display::Coordinator_ImportImage_Result::WithResponse({}));
      }));

  EXPECT_CALL(*renderer_, ImportBufferImage(parent_image_metadata, _)).WillOnce(Return(true));

  display_compositor_->ImportBufferImage(parent_image_metadata,
                                         BufferCollectionUsage::kClientImage);

  const fuchsia::hardware::display::ImageId fidl_child_image_id =
      allocation::ToFidlImageId(child_image_metadata.identifier);
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(_, testing::FieldsAre(FidlEquals(kDisplayBufferCollectionId), 1),
                          FidlEquals(fidl_child_image_id), _))
      .Times(1)
      .WillOnce(testing::Invoke([](fuchsia::hardware::display::types::ImageMetadata,
                                   fuchsia::hardware::display::BufferId,
                                   fuchsia::hardware::display::ImageId,
                                   MockDisplayCoordinator::ImportImageCallback callback) {
        callback(fuchsia::hardware::display::Coordinator_ImportImage_Result::WithResponse({}));
      }));

  EXPECT_CALL(*renderer_, ImportBufferImage(child_image_metadata, _)).WillOnce(Return(true));
  display_compositor_->ImportBufferImage(child_image_metadata, BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*renderer_, SetColorConversionValues(_, _, _)).WillOnce(Return());
  display_compositor_->SetColorConversionValues({1, 0, 0, 0, 1, 0, 0, 0, 1}, {0.1f, 0.2f, 0.3f},
                                                {-0.3f, -0.2f, -0.1f});

  // Setup the EXPECT_CALLs for gmock.
  uint64_t layer_id_value = 1;
  EXPECT_CALL(*mock_display_coordinator_, CreateLayer(_))
      .Times(2)
      .WillRepeatedly(testing::Invoke([&](MockDisplayCoordinator::CreateLayerCallback callback) {
        auto result = fuchsia::hardware::display::Coordinator_CreateLayer_Result::WithResponse(
            fuchsia::hardware::display::Coordinator_CreateLayer_Response(
                {.value = layer_id_value++}));
        callback(std::move(result));
      }));

  std::vector<fuchsia::hardware::display::LayerId> layers = {{.value = 1}, {.value = 2}};
  EXPECT_CALL(*mock_display_coordinator_,
              SetDisplayLayers(FidlEquals(kDisplayId),
                               testing::ElementsAre(FidlEquals(layers[0]), FidlEquals(layers[1]))))
      .Times(1);

  // Make sure each layer has all of its components set properly.
  fuchsia::hardware::display::ImageId fidl_image_ids[] = {
      allocation::ToFidlImageId(child_image_metadata.identifier),
      allocation::ToFidlImageId(parent_image_metadata.identifier)};
  for (uint32_t i = 0; i < 2; i++) {
    EXPECT_CALL(*mock_display_coordinator_, SetLayerPrimaryConfig(FidlEquals(layers[i]), _))
        .Times(1);
    EXPECT_CALL(*mock_display_coordinator_,
                SetLayerPrimaryPosition(FidlEquals(layers[i]), fhd_Transform::IDENTITY, _, _))
        .Times(1)
        .WillOnce(testing::Invoke([sources, destinations, index = i](
                                      fuchsia::hardware::display::LayerId layer_id,
                                      fuchsia::hardware::display::types::Transform transform,
                                      fuchsia::hardware::display::types::Frame src_frame,
                                      fuchsia::hardware::display::types::Frame dest_frame) {
          EXPECT_TRUE(fidl::Equals(src_frame, sources[index]));
          EXPECT_TRUE(fidl::Equals(dest_frame, destinations[index]));
        }));
    EXPECT_CALL(*mock_display_coordinator_, SetLayerPrimaryAlpha(FidlEquals(layers[i]), _, _))
        .Times(1);
    EXPECT_CALL(*mock_display_coordinator_,
                SetLayerImage(FidlEquals(layers[i]), FidlEquals(fidl_image_ids[i]), _, _))
        .Times(1);
  }
  EXPECT_CALL(*mock_display_coordinator_, ImportEvent(_, _)).Times(2);

  EXPECT_CALL(*mock_display_coordinator_, SetDisplayColorConversion(_, _, _, _)).Times(1);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(false, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::types::ConfigResult result =
            fuchsia::hardware::display::types::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  EXPECT_CALL(*renderer_, ChoosePreferredPixelFormat(_));

  DisplayInfo display_info = {resolution, {kPixelFormat}};
  scenic_impl::display::Display display(kDisplayId, resolution.x, resolution.y);
  display_compositor_->AddDisplay(&display, display_info, /*num_vmos*/ 0,
                                  /*out_buffer_collection*/ nullptr);

  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig()).Times(1).WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCallback callback) {
            fuchsia::hardware::display::types::ConfigStamp stamp = {1};
            callback(stamp);
          }));

  display_compositor_->RenderFrame(
      1, zx::time(1),
      GenerateDisplayListForTest({{kDisplayId.value, {display_info, parent_root_handle}}}), {},
      [](const scheduling::Timestamps&) {});

  for (uint32_t i = 0; i < 2; i++) {
    EXPECT_CALL(*mock_display_coordinator_, DestroyLayer(FidlEquals(layers[i]))).Times(1);
  }

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::types::ConfigResult result =
            fuchsia::hardware::display::types::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  display_compositor_.reset();
}

void DisplayCompositorTest::HardwareFrameCorrectnessWithRotationTester(
    glm::mat3 transform_matrix, ImageFlip image_flip,
    fuchsia::hardware::display::types::Frame expected_dst, fhd_Transform expected_transform) {
  const uint64_t kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();
  const fuchsia::hardware::display::BufferCollectionId kDisplayBufferCollectionId =
      allocation::ToDisplayBufferCollectionId(kGlobalBufferCollectionId);

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

  constexpr fuchsia::hardware::display::types::DisplayId kDisplayId = {.value = 1};
  glm::uvec2 resolution(1024, 768);

  // We will end up with 1 source frame, 1 destination frame, and one layer being sent to the
  // display.
  fuchsia::hardware::display::types::Frame source = {
      .x_pos = 0u, .y_pos = 0u, .width = 128u, .height = 256u};

  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(
          testing::Invoke([](fuchsia::hardware::display::BufferCollectionId,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
                             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(
                fuchsia::hardware::display::Coordinator_ImportBufferCollection_Result::WithResponse(
                    {}));
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::BufferCollectionId collection_id,
             fuchsia::hardware::display::types::ImageBufferUsage image_buffer_usage,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(fuchsia::hardware::display::Coordinator_SetBufferCollectionConstraints_Result::
                         WithResponse({}));
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

  const fuchsia::hardware::display::ImageId fidl_parent_image_id =
      allocation::ToFidlImageId(parent_image_metadata.identifier);
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(_, testing::FieldsAre(FidlEquals(kDisplayBufferCollectionId), 0),
                          FidlEquals(fidl_parent_image_id), _))
      .Times(1)
      .WillOnce(testing::Invoke([](fuchsia::hardware::display::types::ImageMetadata,
                                   fuchsia::hardware::display::BufferId,
                                   fuchsia::hardware::display::ImageId,
                                   MockDisplayCoordinator::ImportImageCallback callback) {
        callback(fuchsia::hardware::display::Coordinator_ImportImage_Result::WithResponse({}));
      }));

  EXPECT_CALL(*renderer_, ImportBufferImage(parent_image_metadata, _)).WillOnce(Return(true));

  display_compositor_->ImportBufferImage(parent_image_metadata,
                                         BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*renderer_, SetColorConversionValues(_, _, _)).WillOnce(Return());

  display_compositor_->SetColorConversionValues({1, 0, 0, 0, 1, 0, 0, 0, 1}, {0.1f, 0.2f, 0.3f},
                                                {-0.3f, -0.2f, -0.1f});

  // Setup the EXPECT_CALLs for gmock.
  // Note that a couple of layers are created upfront for the display.
  uint64_t layer_id_value = 1;
  EXPECT_CALL(*mock_display_coordinator_, CreateLayer(_))
      .Times(2)
      .WillRepeatedly(testing::Invoke([&](MockDisplayCoordinator::CreateLayerCallback callback) {
        auto result = fuchsia::hardware::display::Coordinator_CreateLayer_Result::WithResponse(
            fuchsia::hardware::display::Coordinator_CreateLayer_Response(
                {.value = layer_id_value++}));
        callback(std::move(result));
      }));

  // However, we only set one display layer for the image.
  std::vector<fuchsia::hardware::display::LayerId> layers = {{.value = 1}};
  EXPECT_CALL(*mock_display_coordinator_,
              SetDisplayLayers(FidlEquals(kDisplayId), testing::ElementsAre(FidlEquals(layers[0]))))
      .Times(1);

  const fuchsia::hardware::display::ImageId fidl_collection_image_id =
      allocation::ToFidlImageId(parent_image_metadata.identifier);
  EXPECT_CALL(*mock_display_coordinator_, SetLayerPrimaryConfig(FidlEquals(layers[0]), _)).Times(1);
  EXPECT_CALL(*mock_display_coordinator_,
              SetLayerPrimaryPosition(FidlEquals(layers[0]), expected_transform, _, _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [source, expected_dst](fuchsia::hardware::display::LayerId layer_id,
                                 fuchsia::hardware::display::types::Transform transform,
                                 fuchsia::hardware::display::types::Frame src_frame,
                                 fuchsia::hardware::display::types::Frame dest_frame) {
            EXPECT_TRUE(fidl::Equals(src_frame, source));
            EXPECT_TRUE(fidl::Equals(dest_frame, expected_dst));
          }));
  EXPECT_CALL(*mock_display_coordinator_, SetLayerPrimaryAlpha(FidlEquals(layers[0]), _, _))
      .Times(1);
  EXPECT_CALL(*mock_display_coordinator_,
              SetLayerImage(FidlEquals(layers[0]), FidlEquals(fidl_collection_image_id), _, _))
      .Times(1);
  EXPECT_CALL(*mock_display_coordinator_, ImportEvent(_, _)).Times(1);

  EXPECT_CALL(*mock_display_coordinator_, SetDisplayColorConversion(_, _, _, _)).Times(1);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(false, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::types::ConfigResult result =
            fuchsia::hardware::display::types::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  EXPECT_CALL(*renderer_, ChoosePreferredPixelFormat(_));

  DisplayInfo display_info = {resolution, {kPixelFormat}};
  scenic_impl::display::Display display(kDisplayId, resolution.x, resolution.y);
  display_compositor_->AddDisplay(&display, display_info, /*num_vmos*/ 0,
                                  /*out_buffer_collection*/ nullptr);

  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig()).Times(1).WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCallback callback) {
            fuchsia::hardware::display::types::ConfigStamp stamp = {1};
            callback(stamp);
          }));

  display_compositor_->RenderFrame(
      1, zx::time(1),
      GenerateDisplayListForTest({{kDisplayId.value, {display_info, parent_root_handle}}}), {},
      [](const scheduling::Timestamps&) {});

  for (uint64_t i = 1; i < layer_id_value; ++i) {
    EXPECT_CALL(*mock_display_coordinator_, DestroyLayer(testing::FieldsAre(i))).Times(1);
  }

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::types::ConfigResult result =
            fuchsia::hardware::display::types::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));

  display_compositor_.reset();
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWith90DegreeRotationTest) {
  // After scale and 90 CCW rotation, the new top-left corner would be (0, -10). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(0, 10));
  matrix = glm::rotate(matrix, -glm::half_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::types::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::NONE, expected_dst,
                                             fhd_Transform::ROT_90);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWith180DegreeRotationTest) {
  // After scale and 180 CCW rotation, the new top-left corner would be (-10, -20).
  // Translate back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(10, 20));
  matrix = glm::rotate(matrix, -glm::pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::types::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 10u, .height = 20u};

  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::NONE, expected_dst,
                                             fhd_Transform::ROT_180);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWith270DegreeRotationTest) {
  // After scale and 270 CCW rotation, the new top-left corner would be (-20, 0). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(20, 0));
  matrix = glm::rotate(matrix, -glm::three_over_two_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::types::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::NONE, expected_dst,
                                             fhd_Transform::ROT_270);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlipTest) {
  glm::mat3 matrix = glm::scale(glm::mat3(), glm::vec2(10, 20));

  fuchsia::hardware::display::types::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 10u, .height = 20u};

  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::LEFT_RIGHT, expected_dst,
                                             fhd_Transform::REFLECT_Y);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlipTest) {
  glm::mat3 matrix = glm::scale(glm::mat3(), glm::vec2(10, 20));

  fuchsia::hardware::display::types::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 10u, .height = 20u};

  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::UP_DOWN, expected_dst,
                                             fhd_Transform::REFLECT_X);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlip90DegreeRotationTest) {
  // After scale and 90 CCW rotation, the new top-left corner would be (0, -10). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(0, 10));
  matrix = glm::rotate(matrix, -glm::half_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::types::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::LEFT_RIGHT, expected_dst,
                                             fhd_Transform::ROT_90_REFLECT_X);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlip90DegreeRotationTest) {
  // After scale and 90 CCW rotation, the new top-left corner would be (0, -10). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(0, 10));
  matrix = glm::rotate(matrix, -glm::half_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::types::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::UP_DOWN, expected_dst,
                                             fhd_Transform::ROT_90_REFLECT_Y);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlip180DegreeRotationTest) {
  // After scale and 180 CCW rotation, the new top-left corner would be (-10, -20).
  // Translate back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(10, 20));
  matrix = glm::rotate(matrix, -glm::pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::types::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 10u, .height = 20u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::LEFT_RIGHT, expected_dst,
                                             fhd_Transform::REFLECT_X);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlip180DegreeRotationTest) {
  // After scale and 180 CCW rotation, the new top-left corner would be (-10, -20).
  // Translate back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(10, 20));
  matrix = glm::rotate(matrix, -glm::pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::types::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 10u, .height = 20u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::UP_DOWN, expected_dst,
                                             fhd_Transform::REFLECT_Y);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlip270DegreeRotationTest) {
  // After scale and 270 CCW rotation, the new top-left corner would be (-20, 0). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(20, 0));
  matrix = glm::rotate(matrix, -glm::three_over_two_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::types::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::LEFT_RIGHT, expected_dst,
                                             fhd_Transform::ROT_90_REFLECT_Y);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlip270DegreeRotationTest) {
  // After scale and 270 CCW rotation, the new top-left corner would be (-20, 0). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(20, 0));
  matrix = glm::rotate(matrix, -glm::three_over_two_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  fuchsia::hardware::display::types::Frame expected_dst = {
      .x_pos = 0u, .y_pos = 0u, .width = 20u, .height = 10u};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(matrix, ImageFlip::UP_DOWN, expected_dst,
                                             fhd_Transform::ROT_90_REFLECT_X);
}

TEST_F(DisplayCompositorTest, ChecksDisplayImageSignalFences) {
  const uint64_t kGlobalBufferCollectionId = 1;
  const fuchsia::hardware::display::BufferCollectionId kDisplayBufferCollectionId =
      allocation::ToDisplayBufferCollectionId(kGlobalBufferCollectionId);

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

  // Import buffer collection.
  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(
          testing::Invoke([](fuchsia::hardware::display::BufferCollectionId,
                             fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>,
                             MockDisplayCoordinator::ImportBufferCollectionCallback callback) {
            callback(
                fuchsia::hardware::display::Coordinator_ImportBufferCollection_Result::WithResponse(
                    {}));
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(FidlEquals(kDisplayBufferCollectionId), _, _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](fuchsia::hardware::display::BufferCollectionId collection_id,
             fuchsia::hardware::display::types::ImageBufferUsage image_buffer_usage,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCallback callback) {
            callback(fuchsia::hardware::display::Coordinator_SetBufferCollectionConstraints_Result::
                         WithResponse({}));
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
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(_, testing::FieldsAre(FidlEquals(kDisplayBufferCollectionId), 0), _, _))
      .Times(1)
      .WillOnce(testing::Invoke([](fuchsia::hardware::display::types::ImageMetadata,
                                   fuchsia::hardware::display::BufferId,
                                   fuchsia::hardware::display::ImageId,
                                   MockDisplayCoordinator::ImportImageCallback callback) {
        callback(fuchsia::hardware::display::Coordinator_ImportImage_Result::WithResponse({}));
      }));

  EXPECT_CALL(*renderer_, ImportBufferImage(image_metadata, _)).WillOnce(Return(true));
  display_compositor_->ImportBufferImage(image_metadata, BufferCollectionUsage::kClientImage);

  // We start the frame by clearing the config.
  // In the end when the DisplayCompositor is destroyed, the config will also
  // be discarded.
  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(true, _))
      .Times(2)
      .WillRepeatedly(
          testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
            fuchsia::hardware::display::types::ConfigResult result =
                fuchsia::hardware::display::types::ConfigResult::OK;
            std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
            callback(result, ops);
          }));

  // Set expectation for CreateLayer calls.
  uint64_t layer_id_value = 1;
  std::vector<fuchsia::hardware::display::LayerId> layers = {{.value = 1}, {.value = 2}};
  EXPECT_CALL(*mock_display_coordinator_, CreateLayer(_))
      .Times(2)
      .WillRepeatedly(testing::Invoke([&](MockDisplayCoordinator::CreateLayerCallback callback) {
        auto result = fuchsia::hardware::display::Coordinator_CreateLayer_Result::WithResponse(
            fuchsia::hardware::display::Coordinator_CreateLayer_Response(
                {.value = layer_id_value++}));
        callback(std::move(result));
      }));
  EXPECT_CALL(*renderer_, ChoosePreferredPixelFormat(_));

  // Add display.
  constexpr fuchsia::hardware::display::types::DisplayId kDisplayId = {.value = 1};
  glm::uvec2 kResolution(1024, 768);
  DisplayInfo display_info = {kResolution, {kPixelFormat}};
  scenic_impl::display::Display display(kDisplayId, kResolution.x, kResolution.y);
  display_compositor_->AddDisplay(&display, display_info, /*num_vmos*/ 0,
                                  /*out_buffer_collection*/ nullptr);

  // Set expectation for rendering image on layer.
  std::vector<fuchsia::hardware::display::LayerId> active_layers = {{.value = 1}};
  zx::event imported_event;
  EXPECT_CALL(*mock_display_coordinator_, ImportEvent(_, _))
      .Times(1)
      .WillOnce(
          testing::Invoke([&imported_event](zx::event event, fuchsia::hardware::display::EventId) {
            imported_event = std::move(event);
          }));
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
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::types::ConfigResult result =
            fuchsia::hardware::display::types::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));
  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig()).Times(1).WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCallback callback) {
            fuchsia::hardware::display::types::ConfigStamp stamp = {1};
            callback(stamp);
          }));

  // Render image. This should end up in display.
  const auto& display_list =
      GenerateDisplayListForTest({{kDisplayId.value, {display_info, root_handle}}});
  display_compositor_->RenderFrame(1, zx::time(1), display_list, {},
                                   [](const scheduling::Timestamps&) {});

  // Try rendering again. Because |imported_event| isn't signaled and no render targets
  // were created when adding display, we should fail.
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

  const allocation::GlobalBufferCollectionId kGlobalBufferCollectionId = 15;
  const fuchsia::hardware::display::BufferCollectionId kDisplayBufferCollectionId =
      allocation::ToDisplayBufferCollectionId(kGlobalBufferCollectionId);

  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(FidlEquals(kDisplayBufferCollectionId), _, _))
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

  EXPECT_CALL(*mock_display_coordinator_,
              ReleaseBufferCollection(FidlEquals(kDisplayBufferCollectionId)))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(*renderer_, ReleaseBufferCollection(kGlobalBufferCollectionId, _)).WillOnce(Return());
  display_compositor_->ReleaseBufferCollection(kGlobalBufferCollectionId,
                                               BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](bool, MockDisplayCoordinator::CheckConfigCallback callback) {
        fuchsia::hardware::display::types::ConfigResult result =
            fuchsia::hardware::display::types::ConfigResult::OK;
        std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
        callback(result, ops);
      }));
  display_compositor_.reset();
}

}  // namespace flatland::test
