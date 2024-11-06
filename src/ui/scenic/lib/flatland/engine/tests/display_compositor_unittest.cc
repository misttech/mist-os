// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.math/cpp/fidl.h>
#include <fidl/fuchsia.ui.composition/cpp/fidl.h>
#include <fuchsia/math/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>
#include <lib/sync/cpp/completion.h>
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
using ::testing::Eq;
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
using fuchsia::sysmem::BufferUsage;
using fuchsia::ui::composition::ChildViewStatus;
using fuchsia::ui::composition::ChildViewWatcher;
using fuchsia::ui::composition::LayoutInfo;
using fuchsia::ui::composition::ParentViewportWatcher;
using fuchsia::ui::views::ViewCreationToken;
using fuchsia::ui::views::ViewportCreationToken;
using fuchsia_ui_composition::BlendMode;
using fuchsia_ui_composition::ImageFlip;

namespace flatland::test {

namespace {

// Returns a matcher matching the `field` from [`fuchsia.hardware.display/Coordinator.FunctionName`]
// FIDL request.
#define MatchRequestField(FunctionName, field, matcher)                                         \
  testing::Property(&fidl::Request<fuchsia_hardware_display::Coordinator::FunctionName>::field, \
                    (matcher))

fuchsia::sysmem2::BufferCollectionTokenPtr DuplicateToken(
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr& token) {
  fuchsia::sysmem2::BufferCollectionTokenDuplicateSyncRequest dup_sync_request;
  dup_sync_request.set_rights_attenuation_masks({ZX_RIGHT_SAME_RIGHTS});
  fuchsia::sysmem2::BufferCollectionToken_DuplicateSync_Result dup_sync_result;
  const auto status = token->DuplicateSync(std::move(dup_sync_request), &dup_sync_result);
  FX_CHECK(status == ZX_OK);
  FX_CHECK(dup_sync_result.is_response());
  FX_CHECK(dup_sync_result.response().tokens().size() == 1u);
  return dup_sync_result.response().mutable_tokens()->at(0).Bind();
}

void SetConstraintsAndClose(fuchsia::sysmem2::AllocatorSyncPtr& sysmem_allocator,
                            fuchsia::sysmem2::BufferCollectionTokenSyncPtr token,
                            fuchsia::sysmem2::BufferCollectionConstraints constraints) {
  fuchsia::sysmem2::BufferCollectionSyncPtr collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(token));
  bind_shared_request.set_buffer_collection_request(collection.NewRequest());
  ASSERT_EQ(sysmem_allocator->BindSharedCollection(std::move(bind_shared_request)), ZX_OK);

  fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.set_constraints(std::move(constraints));
  ASSERT_EQ(collection->SetConstraints(std::move(set_constraints_request)), ZX_OK);
  // If SetConstraints() fails there's a race where Sysmem may drop the channel. Don't assert on the
  // success of Close().
  collection->Release();
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

    zx::result endpoints_result = fidl::CreateEndpoints<fuchsia_hardware_display::Coordinator>();
    FX_CHECK(endpoints_result.is_ok())
        << "Failed to create FIDL endpoints for the display coordinator: "
        << endpoints_result.status_string();
    auto [coordinator_client, coordinator_server] = std::move(endpoints_result).value();

    mock_display_coordinator_ =
        std::make_unique<testing::StrictMock<flatland::MockDisplayCoordinator>>();
    // The fidl::Server requires the binding and teardown to occur on the
    // same thread where the FIDL server runs.
    libsync::Completion completion;
    async::PostTask(
        display_coordinator_loop_.dispatcher(),
        [this, &completion, coordinator_server = std::move(coordinator_server)]() mutable {
          mock_display_coordinator_->Bind(std::move(coordinator_server),
                                          display_coordinator_loop_.dispatcher());
          completion.Signal();
        });
    display_coordinator_loop_.StartThread("display-coordinator-loop");
    completion.Wait();

    auto shared_display_coordinator =
        std::make_shared<fidl::SyncClient<fuchsia_hardware_display::Coordinator>>(
            std::move(coordinator_client));

    display_compositor_ = std::make_shared<flatland::DisplayCompositor>(
        dispatcher(), std::move(shared_display_coordinator), renderer_,
        utils::CreateSysmemAllocatorSyncPtr("display_compositor_unittest"),
        /*enable_display_composition*/ true, /*max_display_layers=*/2, /*visual_debug_level=*/0);
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

  fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> CreateToken() {
    fuchsia::sysmem2::BufferCollectionTokenSyncPtr token;
    fuchsia::sysmem2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
    allocate_shared_request.set_token_request(token.NewRequest());
    zx_status_t status =
        sysmem_allocator_->AllocateSharedCollection(std::move(allocate_shared_request));
    FX_DCHECK(status == ZX_OK);
    fuchsia::sysmem2::Node_Sync_Result sync_result;
    status = token->Sync(&sync_result);
    FX_DCHECK(status == ZX_OK);
    FX_DCHECK(sync_result.is_response());
    return token;
  }

  void SetDisplaySupported(allocation::GlobalBufferCollectionId id, bool is_supported) {
    std::scoped_lock lock(display_compositor_->lock_);
    display_compositor_->buffer_collection_supports_display_[id] = is_supported;
    display_compositor_->buffer_collection_pixel_format_modifier_[id] =
        fuchsia::images2::PixelFormatModifier::LINEAR;
  }

  void ForceRendererOnlyMode(bool force_renderer_only) {
    display_compositor_->enable_display_composition_ = !force_renderer_only;
  }

  void SendOnVsyncEvent(fuchsia_hardware_display_types::ConfigStamp stamp) {
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
  fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator_;

  void HardwareFrameCorrectnessWithRotationTester(
      glm::mat3 transform_matrix, ImageFlip image_flip, fuchsia_math::RectU expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation expected_transform);
};

// TODO(https://fxbug.dev/324688770): Dispatch all DisplayCompositor methods
// to the test loop.

TEST_F(DisplayCompositorTest, ImportAndReleaseBufferCollectionTest) {
  constexpr allocation::GlobalBufferCollectionId kGlobalBufferCollectionId = 15;
  const fuchsia_hardware_display::BufferCollectionId kDisplayBufferCollectionId =
      scenic_impl::ToDisplayFidlBufferCollectionId(kGlobalBufferCollectionId);

  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(MatchRequestField(ImportBufferCollection, buffer_collection_id,
                                                       Eq(kDisplayBufferCollectionId)),
                                     _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::ImportBufferCollectionRequest&,
             MockDisplayCoordinator::ImportBufferCollectionCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));

  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(
                  MatchRequestField(SetBufferCollectionConstraints, buffer_collection_id,
                                    Eq(kDisplayBufferCollectionId)),
                  _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::SetBufferCollectionConstraintsRequest&,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));

  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId,
                             fuchsia::sysmem2::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token,
                             BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        token_ref = std::move(token);
        return true;
      });
  display_compositor_->ImportBufferCollection(kGlobalBufferCollectionId, sysmem_allocator_.get(),
                                              CreateToken(), BufferCollectionUsage::kClientImage,
                                              std::nullopt);

  EXPECT_CALL(
      *mock_display_coordinator_,
      ReleaseBufferCollection(MatchRequestField(ReleaseBufferCollection, buffer_collection_id,
                                                Eq(kDisplayBufferCollectionId)),
                              _))
      .Times(1)
      .WillOnce(Return());

  EXPECT_CALL(*renderer_, ReleaseBufferCollection(kGlobalBufferCollectionId, _)).WillOnce(Return());
  display_compositor_->ReleaseBufferCollection(kGlobalBufferCollectionId,
                                               BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([](MockDisplayCoordinator::CheckConfigRequest&,
                                   MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
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
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr client_token = CreateToken().BindSync();
  fuchsia::sysmem2::BufferCollectionTokenPtr compositor_token = DuplicateToken(client_token);

  // Set "client" constraints.
  fuchsia::sysmem2::BufferCollectionSyncPtr client_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(client_token));
  bind_shared_request.set_buffer_collection_request(client_collection.NewRequest());
  ASSERT_EQ(sysmem_allocator_->BindSharedCollection(std::move(bind_shared_request)), ZX_OK);

  fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  auto& constraints = *set_constraints_request.mutable_constraints();
  constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE);
  constraints.set_min_buffer_count(1);
  constraints.set_max_buffer_count(3);
  auto& bmc = *constraints.mutable_buffer_memory_constraints();
  bmc.set_min_size_bytes(1);
  bmc.set_max_size_bytes(20);
  auto& ifc = constraints.mutable_image_format_constraints()->emplace_back();
  ifc.set_pixel_format(fuchsia::images2::PixelFormat::B8G8R8A8);
  ifc.mutable_color_spaces()->emplace_back(fuchsia::images2::ColorSpace::SRGB);
  ifc.set_min_size(fuchsia::math::SizeU{.width = 1, .height = 1});
  ASSERT_EQ(client_collection->SetConstraints(std::move(set_constraints_request)), ZX_OK);

  const auto kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();
  const fuchsia_hardware_display::BufferCollectionId kDisplayBufferCollectionId =
      scenic_impl::ToDisplayFidlBufferCollectionId(kGlobalBufferCollectionId);

  fuchsia::sysmem2::BufferCollectionTokenSyncPtr display_token;
  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(MatchRequestField(ImportBufferCollection, buffer_collection_id,
                                                       Eq(kDisplayBufferCollectionId)),
                                     _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&display_token](
              MockDisplayCoordinator::ImportBufferCollectionRequest& request,
              MockDisplayCoordinator::ImportBufferCollectionCompleter::Sync& completer) {
            display_token.Bind(request.buffer_collection_token().TakeChannel());
            completer.Reply(fit::ok());
          }));

  // Set display constraints.
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(
                  MatchRequestField(SetBufferCollectionConstraints, buffer_collection_id,
                                    Eq(kDisplayBufferCollectionId)),
                  _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&display_token](
              MockDisplayCoordinator::SetBufferCollectionConstraintsRequest&,
              MockDisplayCoordinator::SetBufferCollectionConstraintsCompleter::Sync& completer) {
            fuchsia::sysmem2::BufferCollectionConstraints constraints;
            constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE);
            constraints.set_min_buffer_count(2);
            constraints.set_max_buffer_count(3);
            auto sysmem_allocator = utils::CreateSysmemAllocatorSyncPtr("MockDisplayCoordinator");
            SetConstraintsAndClose(sysmem_allocator, std::move(display_token),
                                   std::move(constraints));
            completer.Reply(fit::ok());
          }));

  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(MatchRequestField(ImportImage, buffer_id,
                                            Eq(fuchsia_hardware_display::BufferId{{
                                                .buffer_collection_id = kDisplayBufferCollectionId,
                                                .buffer_index = 0,
                                            }})),
                          _))
      .Times(1)
      .WillOnce(testing::Invoke([](MockDisplayCoordinator::ImportImageRequest&,
                                   MockDisplayCoordinator::ImportImageCompleter::Sync& completer) {
        completer.Reply(fit::ok());
      }));

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
      }));

  // Set renderer constraints.
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce(
          [this](allocation::GlobalBufferCollectionId, fuchsia::sysmem2::Allocator_Sync*,
                 fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> renderer_token,
                 BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
            fuchsia::sysmem2::BufferCollectionConstraints constraints;
            constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE);
            constraints.set_min_buffer_count(1);
            constraints.set_max_buffer_count(2);
            SetConstraintsAndClose(sysmem_allocator_, renderer_token.BindSync(),
                                   std::move(constraints));
            return true;
          });

  ASSERT_TRUE(display_compositor_->ImportBufferCollection(
      kGlobalBufferCollectionId, sysmem_allocator_.get(), std::move(compositor_token),
      BufferCollectionUsage::kClientImage, std::nullopt));

  {
    fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
    ASSERT_EQ(client_collection->WaitForAllBuffersAllocated(&wait_result), ZX_OK);
    EXPECT_TRUE(wait_result.is_response());
    EXPECT_EQ(wait_result.response().buffer_collection_info().buffers().size(), 2u);
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
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr client_token = CreateToken().BindSync();
  fuchsia::sysmem2::BufferCollectionTokenPtr compositor_token = DuplicateToken(client_token);

  // Set "client" constraints.
  fuchsia::sysmem2::BufferCollectionSyncPtr client_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(client_token));
  bind_shared_request.set_buffer_collection_request(client_collection.NewRequest());
  ASSERT_EQ(sysmem_allocator_->BindSharedCollection(std::move(bind_shared_request)), ZX_OK);

  fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  auto& constraints = *set_constraints_request.mutable_constraints();
  constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE);
  constraints.set_min_buffer_count(1);
  constraints.set_max_buffer_count(2);
  auto& bmc = *constraints.mutable_buffer_memory_constraints();
  bmc.set_min_size_bytes(1);
  bmc.set_max_size_bytes(20);
  auto& ifc = constraints.mutable_image_format_constraints()->emplace_back();
  ifc.set_pixel_format(fuchsia::images2::PixelFormat::B8G8R8A8);
  ifc.mutable_color_spaces()->emplace_back(fuchsia::images2::ColorSpace::SRGB);
  ifc.set_min_size(fuchsia::math::SizeU{.width = 1, .height = 1});
  ASSERT_EQ(client_collection->SetConstraints(std::move(set_constraints_request)), ZX_OK);

  const auto kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();
  const fuchsia_hardware_display::BufferCollectionId kDisplayBufferCollectionId =
      scenic_impl::ToDisplayFidlBufferCollectionId(kGlobalBufferCollectionId);

  fuchsia::sysmem2::BufferCollectionTokenSyncPtr display_token;
  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(MatchRequestField(ImportBufferCollection, buffer_collection_id,
                                                       Eq(kDisplayBufferCollectionId)),
                                     _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&display_token](
              MockDisplayCoordinator::ImportBufferCollectionRequest& request,
              MockDisplayCoordinator::ImportBufferCollectionCompleter::Sync& completer) {
            display_token.Bind(request.buffer_collection_token().TakeChannel());
            completer.Reply(fit::ok());
          }));

  // Set display constraints.
  fuchsia::sysmem2::BufferCollectionSyncPtr display_collection;
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(
                  MatchRequestField(SetBufferCollectionConstraints, buffer_collection_id,
                                    Eq(kDisplayBufferCollectionId)),
                  _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&display_token](
              MockDisplayCoordinator::SetBufferCollectionConstraintsRequest&,
              MockDisplayCoordinator::SetBufferCollectionConstraintsCompleter::Sync& completer) {
            fuchsia::sysmem2::BufferCollectionConstraints constraints;
            constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE);
            constraints.set_min_buffer_count(1);
            constraints.set_max_buffer_count(1);
            auto sysmem_allocator = utils::CreateSysmemAllocatorSyncPtr("MockDisplayCoordinator");
            SetConstraintsAndClose(sysmem_allocator, std::move(display_token),
                                   std::move(constraints));
            completer.Reply(fit::ok());
          }));

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
      }));

  // Set renderer constraints.
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce(
          [this](allocation::GlobalBufferCollectionId, fuchsia::sysmem2::Allocator_Sync*,
                 fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> renderer_token,
                 BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
            fuchsia::sysmem2::BufferCollectionConstraints constraints;
            constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE);
            constraints.set_min_buffer_count(2);
            constraints.set_max_buffer_count(2);
            SetConstraintsAndClose(sysmem_allocator_, renderer_token.BindSync(),
                                   std::move(constraints));
            return true;
          });

  ASSERT_TRUE(display_compositor_->ImportBufferCollection(
      kGlobalBufferCollectionId, sysmem_allocator_.get(), std::move(compositor_token),
      BufferCollectionUsage::kClientImage, std::nullopt));

  {
    fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
    ASSERT_EQ(client_collection->WaitForAllBuffersAllocated(&wait_result), ZX_OK);
    EXPECT_TRUE(wait_result.is_response());
    EXPECT_EQ(wait_result.response().buffer_collection_info().buffers().size(), 2u);
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
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr client_token = CreateToken().BindSync();
  fuchsia::sysmem2::BufferCollectionTokenPtr compositor_token = DuplicateToken(client_token);

  // Set "client" constraints.
  fuchsia::sysmem2::BufferCollectionSyncPtr client_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(client_token));
  bind_shared_request.set_buffer_collection_request(client_collection.NewRequest());
  ASSERT_EQ(sysmem_allocator_->BindSharedCollection(std::move(bind_shared_request)), ZX_OK);

  fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  auto& constraints = *set_constraints_request.mutable_constraints();
  constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE);
  auto& bmc = *constraints.mutable_buffer_memory_constraints();
  bmc.set_min_size_bytes(1);
  bmc.set_max_size_bytes(20);
  auto& ifc = constraints.mutable_image_format_constraints()->emplace_back();
  ifc.set_pixel_format(fuchsia::images2::PixelFormat::B8G8R8A8);
  ifc.mutable_color_spaces()->emplace_back(fuchsia::images2::ColorSpace::SRGB);
  ifc.set_min_size(fuchsia::math::SizeU{.width = 1, .height = 1});
  ASSERT_EQ(client_collection->SetConstraints(std::move(set_constraints_request)), ZX_OK);

  const auto kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();
  const fuchsia_hardware_display::BufferCollectionId kDisplayBufferCollectionId =
      scenic_impl::ToDisplayFidlBufferCollectionId(kGlobalBufferCollectionId);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
      }));

  // Set renderer constraints.
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce(
          [this](allocation::GlobalBufferCollectionId, fuchsia::sysmem2::Allocator_Sync*,
                 fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> renderer_token,
                 BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
            fuchsia::sysmem2::BufferCollectionConstraints constraints;
            constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE);
            constraints.set_min_buffer_count(2);
            constraints.set_max_buffer_count(2);
            SetConstraintsAndClose(sysmem_allocator_, renderer_token.BindSync(),
                                   std::move(constraints));
            return true;
          });

  // Import BufferCollection and image to trigger constraint setting and handling of allocations.
  ASSERT_TRUE(display_compositor_->ImportBufferCollection(
      kGlobalBufferCollectionId, sysmem_allocator_.get(), std::move(compositor_token),
      BufferCollectionUsage::kClientImage, std::nullopt));

  {
    fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
    ASSERT_EQ(client_collection->WaitForAllBuffersAllocated(&wait_result), ZX_OK);
    EXPECT_TRUE(wait_result.is_response());
    EXPECT_EQ(wait_result.response().buffer_collection_info().buffers().size(), 2u);
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
  const fuchsia_hardware_display::BufferCollectionId kDisplayBufferCollectionId =
      scenic_impl::ToDisplayFidlBufferCollectionId(kGlobalBufferCollectionId);

  fuchsia::sysmem2::BufferCollectionTokenSyncPtr dup_token;
  // Let client drop token.
  {
    auto token = CreateToken();
    auto sync_token = token.BindSync();
    fuchsia::sysmem2::BufferCollectionTokenDuplicateSyncRequest dup_request;
    fuchsia::sysmem2::BufferCollectionToken_DuplicateSync_Result dup_result;
    dup_request.set_rights_attenuation_masks({ZX_RIGHT_SAME_RIGHTS});
    zx_status_t status = sync_token->DuplicateSync(std::move(dup_request), &dup_result);
    ASSERT_EQ(status, ZX_OK);
    ASSERT_TRUE(dup_result.is_response());
    ASSERT_TRUE(dup_result.response().has_tokens());
    ASSERT_EQ(dup_result.response().tokens().size(), 1u);

    dup_token = dup_result.response().mutable_tokens()->at(0).BindSync();
  }

  // Make sure that the Sysmem driver has been aware of the fact that
  // `sync_token` is destroyed, in which case it returns an error
  // when the duplicated token `Sync()`s.
  fuchsia::sysmem2::Node_Sync_Result sync_result;

  EXPECT_TRUE(RunWithTimeoutOrUntil(
      [&] {
        zx_status_t status = dup_token->Sync(&sync_result);
        return (status != ZX_OK || sync_result.is_framework_err());
      },
      zx::duration::infinite(), zx::msec(50)));

  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _)).Times(0);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
      }));

  EXPECT_CALL(*mock_display_coordinator_, ImportBufferCollection(_, _)).Times(0);

  EXPECT_FALSE(display_compositor_->ImportBufferCollection(
      kGlobalBufferCollectionId, sysmem_allocator_.get(), std::move(dup_token),
      BufferCollectionUsage::kClientImage, std::nullopt));

  display_compositor_.reset();
}

TEST_F(DisplayCompositorTest, ImageIsValidAfterReleaseBufferCollection) {
  const auto kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();
  const fuchsia_hardware_display::BufferCollectionId kDisplayBufferCollectionId =
      scenic_impl::ToDisplayFidlBufferCollectionId(kGlobalBufferCollectionId);

  // Import buffer collection.
  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(MatchRequestField(ImportBufferCollection, buffer_collection_id,
                                                       Eq(kDisplayBufferCollectionId)),
                                     _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::ImportBufferCollectionRequest& request,
             MockDisplayCoordinator::ImportBufferCollectionCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(
                  MatchRequestField(SetBufferCollectionConstraints, buffer_collection_id,
                                    Eq(kDisplayBufferCollectionId)),
                  _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::SetBufferCollectionConstraintsRequest&,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId,
                             fuchsia::sysmem2::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token,
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
      .blend_mode = BlendMode::kSrc,
  };
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(MatchRequestField(ImportImage, buffer_id,
                                            Eq(fuchsia_hardware_display::BufferId{{
                                                .buffer_collection_id = kDisplayBufferCollectionId,
                                                .buffer_index = 0,
                                            }})),
                          _))
      .Times(1)
      .WillOnce(testing::Invoke([](MockDisplayCoordinator::ImportImageRequest&,
                                   MockDisplayCoordinator::ImportImageCompleter::Sync& completer) {
        completer.Reply(fit::ok());
      }));
  EXPECT_CALL(*renderer_, ImportBufferImage(image_metadata, _)).WillOnce(Return(true));
  display_compositor_->ImportBufferImage(image_metadata, BufferCollectionUsage::kClientImage);

  // Release buffer collection. Make sure that does not release Image.
  const fuchsia_hardware_display::ImageId kFidlImageId =
      scenic_impl::ToDisplayFidlImageId(image_metadata.identifier);
  EXPECT_CALL(*mock_display_coordinator_,
              ReleaseImage(MatchRequestField(ReleaseImage, image_id, Eq(kFidlImageId)), _))
      .Times(0);
  EXPECT_CALL(
      *mock_display_coordinator_,
      ReleaseBufferCollection(MatchRequestField(ReleaseBufferCollection, buffer_collection_id,
                                                Eq(kDisplayBufferCollectionId)),
                              _))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(*renderer_, ReleaseBufferCollection(kGlobalBufferCollectionId, _)).WillOnce(Return());
  display_compositor_->ReleaseBufferCollection(kGlobalBufferCollectionId,
                                               BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
      }));

  display_compositor_.reset();
}

TEST_F(DisplayCompositorTest, ImportImageErrorCases) {
  const allocation::GlobalBufferCollectionId kGlobalBufferCollectionId =
      allocation::GenerateUniqueBufferCollectionId();
  const fuchsia_hardware_display::BufferCollectionId kDisplayBufferCollectionId =
      scenic_impl::ToDisplayFidlBufferCollectionId(kGlobalBufferCollectionId);

  const allocation::GlobalImageId kImageId = allocation::GenerateUniqueImageId();
  const fuchsia_hardware_display::ImageId kFidlImageId =
      scenic_impl::ToDisplayFidlImageId(kImageId);
  const uint32_t kVmoCount = 2;
  const uint32_t kVmoIdx = 1;
  const uint32_t kMaxWidth = 100;
  const uint32_t kMaxHeight = 200;
  uint32_t num_times_import_image_called = 0;

  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(MatchRequestField(ImportBufferCollection, buffer_collection_id,
                                                       Eq(kDisplayBufferCollectionId)),
                                     _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::ImportBufferCollectionRequest& request,
             MockDisplayCoordinator::ImportBufferCollectionCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(
                  MatchRequestField(SetBufferCollectionConstraints, buffer_collection_id,
                                    Eq(kDisplayBufferCollectionId)),
                  _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::SetBufferCollectionConstraintsRequest&,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId,
                             fuchsia::sysmem2::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token,
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
      .blend_mode = BlendMode::kSrc,
  };

  // Make sure that the engine returns true if the display coordinator returns true.
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(MatchRequestField(ImportImage, buffer_id,
                                            Eq(fuchsia_hardware_display::BufferId{{
                                                .buffer_collection_id = kDisplayBufferCollectionId,
                                                .buffer_index = kVmoIdx,
                                            }})),
                          _))
      .Times(1)
      .WillOnce(testing::Invoke([](MockDisplayCoordinator::ImportImageRequest&,
                                   MockDisplayCoordinator::ImportImageCompleter::Sync& completer) {
        completer.Reply(fit::ok());
      }));

  EXPECT_CALL(*renderer_, ImportBufferImage(metadata, _)).WillOnce(Return(true));

  auto result =
      display_compositor_->ImportBufferImage(metadata, BufferCollectionUsage::kClientImage);
  EXPECT_TRUE(result);

  // Make sure we can release the image properly.
  EXPECT_CALL(*mock_display_coordinator_,
              ReleaseImage(MatchRequestField(ReleaseImage, image_id, Eq(kFidlImageId)), _))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(*renderer_, ReleaseBufferImage(metadata.identifier)).WillOnce(Return());

  display_compositor_->ReleaseBufferImage(metadata.identifier);

  // Make sure that the engine returns false if the display coordinator returns an error
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(MatchRequestField(ImportImage, buffer_id,
                                            Eq(fuchsia_hardware_display::BufferId{{
                                                .buffer_collection_id = kDisplayBufferCollectionId,
                                                .buffer_index = kVmoIdx,
                                            }})),
                          _))
      .Times(1)
      .WillOnce(testing::Invoke([](MockDisplayCoordinator::ImportImageRequest&,
                                   MockDisplayCoordinator::ImportImageCompleter::Sync& completer) {
        completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
      }));

  // This should still return false for the engine even if the renderer returns true.
  EXPECT_CALL(*renderer_, ImportBufferImage(metadata, _)).WillOnce(Return(true));

  result = display_compositor_->ImportBufferImage(metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Collection ID can't be invalid. This shouldn't reach the display coordinator.
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(MatchRequestField(ImportImage, buffer_id,
                                            Eq(fuchsia_hardware_display::BufferId{{
                                                .buffer_collection_id = kDisplayBufferCollectionId,
                                                .buffer_index = kVmoIdx,
                                            }})),
                          _))
      .Times(0);
  auto copy_metadata = metadata;
  copy_metadata.collection_id = allocation::kInvalidId;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Image Id can't be 0. This shouldn't reach the display coordinator.
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(MatchRequestField(ImportImage, buffer_id,
                                            Eq(fuchsia_hardware_display::BufferId{{
                                                .buffer_collection_id = kDisplayBufferCollectionId,
                                                .buffer_index = kVmoIdx,
                                            }})),
                          _))
      .Times(0);
  copy_metadata = metadata;
  copy_metadata.identifier = allocation::kInvalidImageId;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Width can't be 0. This shouldn't reach the display coordinator.
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(MatchRequestField(ImportImage, buffer_id,
                                            Eq(fuchsia_hardware_display::BufferId{{
                                                .buffer_collection_id = kDisplayBufferCollectionId,
                                                .buffer_index = kVmoIdx,
                                            }})),
                          _))
      .Times(0);
  copy_metadata = metadata;
  copy_metadata.width = 0;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  // Height can't be 0. This shouldn't reach the display coordinator.
  EXPECT_CALL(*mock_display_coordinator_,
              ImportImage(MatchRequestField(ImportImage, buffer_id,
                                            Eq(fuchsia_hardware_display::BufferId{{
                                                .buffer_collection_id = kDisplayBufferCollectionId,
                                                .buffer_index = kVmoIdx,
                                            }})),
                          _))
      .Times(0);
  copy_metadata = metadata;
  copy_metadata.height = 0;
  result =
      display_compositor_->ImportBufferImage(copy_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(result);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
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
          testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                              MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
            completer.Reply(
                {{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
          }));
  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig(_)).Times(2).WillRepeatedly(Return());

  const fuchsia_hardware_display_types::ConfigStamp kConfigStamp1 = {{.value = 234}};
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCompleter::Sync& completer) {
            completer.Reply({{.stamp = kConfigStamp1}});
          }));
  display_compositor_->RenderFrame(1, zx::time(1), {}, {}, [](const scheduling::Timestamps&) {});

  const fuchsia_hardware_display_types::ConfigStamp kConfigStamp2 = {{.value = 123}};
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCompleter::Sync& completer) {
            completer.Reply({{.stamp = kConfigStamp2}});
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
  const fuchsia_hardware_display::BufferCollectionId kDisplayBufferCollectionId =
      scenic_impl::ToDisplayFidlBufferCollectionId(kGlobalBufferCollectionId);

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
      .blend_mode = BlendMode::kSrc,
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
      .blend_mode = BlendMode::kSrc,
  };
  child_struct->images[child_image_handle] = child_image_metadata;
  child_struct->local_matrices[child_image_handle] =
      glm::scale(glm::translate(glm::mat3(1), glm::vec2(5, 7)), glm::vec2(30, 40));
  child_struct->local_image_sample_regions[child_image_handle] = {0, 0, 512, 1024};

  // Submit the UberStruct.
  child_session.PushUberStruct(std::move(child_struct));

  const fuchsia_hardware_display_types::DisplayId kDisplayId = {{.value = 1}};
  glm::uvec2 resolution(1024, 768);

  // We will end up with 2 source frames, 2 destination frames, and two layers being sent to the
  // display.
  const fuchsia_math::RectU sources[2] = {{{.x = 0u, .y = 0u, .width = 512, .height = 1024u}},
                                          {{.x = 0u, .y = 0u, .width = 128u, .height = 256u}}};

  const fuchsia_math::RectU destinations[2] = {{{.x = 5u, .y = 7u, .width = 30, .height = 40u}},
                                               {{.x = 9u, .y = 13u, .width = 10u, .height = 20u}}};

  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(MatchRequestField(ImportBufferCollection, buffer_collection_id,
                                                       Eq(kDisplayBufferCollectionId)),
                                     _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::ImportBufferCollectionRequest& request,
             MockDisplayCoordinator::ImportBufferCollectionCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(
                  MatchRequestField(SetBufferCollectionConstraints, buffer_collection_id,
                                    Eq(kDisplayBufferCollectionId)),
                  _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::SetBufferCollectionConstraintsRequest&,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId,
                             fuchsia::sysmem2::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token,
                             BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        token_ref = std::move(token);
        return true;
      });
  display_compositor_->ImportBufferCollection(kGlobalBufferCollectionId, sysmem_allocator_.get(),
                                              CreateToken(), BufferCollectionUsage::kClientImage,
                                              std::nullopt);
  SetDisplaySupported(kGlobalBufferCollectionId, true);

  const fuchsia_hardware_display::ImageId fidl_parent_image_id =
      scenic_impl::ToDisplayFidlImageId(parent_image_metadata.identifier);
  EXPECT_CALL(
      *mock_display_coordinator_,
      ImportImage(
          testing::AllOf(MatchRequestField(ImportImage, buffer_id,
                                           Eq(fuchsia_hardware_display::BufferId{{
                                               .buffer_collection_id = kDisplayBufferCollectionId,
                                               .buffer_index = 0,
                                           }})),
                         MatchRequestField(ImportImage, image_id, Eq(fidl_parent_image_id))),
          _))
      .Times(1)
      .WillOnce(testing::Invoke([](MockDisplayCoordinator::ImportImageRequest&,
                                   MockDisplayCoordinator::ImportImageCompleter::Sync& completer) {
        completer.Reply(fit::ok());
      }));
  EXPECT_CALL(*renderer_, ImportBufferImage(parent_image_metadata, _)).WillOnce(Return(true));

  display_compositor_->ImportBufferImage(parent_image_metadata,
                                         BufferCollectionUsage::kClientImage);

  const fuchsia_hardware_display::ImageId fidl_child_image_id =
      scenic_impl::ToDisplayFidlImageId(child_image_metadata.identifier);

  EXPECT_CALL(
      *mock_display_coordinator_,
      ImportImage(
          testing::AllOf(MatchRequestField(ImportImage, buffer_id,
                                           Eq(fuchsia_hardware_display::BufferId{{
                                               .buffer_collection_id = kDisplayBufferCollectionId,
                                               .buffer_index = 1,
                                           }})),
                         MatchRequestField(ImportImage, image_id, Eq(fidl_child_image_id))),
          _))
      .Times(1)
      .WillOnce(testing::Invoke([](MockDisplayCoordinator::ImportImageRequest&,
                                   MockDisplayCoordinator::ImportImageCompleter::Sync& completer) {
        completer.Reply(fit::ok());
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
      .WillRepeatedly(
          testing::Invoke([&](MockDisplayCoordinator::CreateLayerCompleter::Sync& completer) {
            fuchsia_hardware_display::CoordinatorCreateLayerResponse response(
                fuchsia_hardware_display::LayerId{{.value = layer_id_value++}});
            completer.Reply(fit::ok(std::move(response)));
          }));

  std::vector<fuchsia_hardware_display::LayerId> layers = {{{.value = 1}}, {{.value = 2}}};

  EXPECT_CALL(*mock_display_coordinator_,
              SetDisplayLayers(
                  testing::AllOf(MatchRequestField(SetDisplayLayers, display_id, Eq(kDisplayId)),
                                 MatchRequestField(SetDisplayLayers, layer_ids,
                                                   testing::ElementsAreArray(layers))),
                  _))
      .Times(1)
      .WillOnce(Return());

  // Make sure each layer has all of its components set properly.
  fuchsia_hardware_display::ImageId fidl_image_ids[] = {
      scenic_impl::ToDisplayFidlImageId(child_image_metadata.identifier),
      scenic_impl::ToDisplayFidlImageId(parent_image_metadata.identifier)};
  for (uint32_t i = 0; i < 2; i++) {
    EXPECT_CALL(
        *mock_display_coordinator_,
        SetLayerPrimaryConfig(MatchRequestField(SetLayerPrimaryConfig, layer_id, Eq(layers[i])), _))
        .Times(1)
        .WillOnce(Return());

    EXPECT_CALL(
        *mock_display_coordinator_,
        SetLayerPrimaryPosition(
            testing::AllOf(
                MatchRequestField(SetLayerPrimaryPosition, layer_id, Eq(layers[i])),
                MatchRequestField(
                    SetLayerPrimaryPosition, image_source_transformation,
                    Eq(fuchsia_hardware_display_types::CoordinateTransformation::kIdentity))),
            _))
        .Times(1)
        .WillOnce(testing::Invoke(
            [sources, destinations, index = i](
                MockDisplayCoordinator::SetLayerPrimaryPositionRequest& request,
                MockDisplayCoordinator::SetLayerPrimaryPositionCompleter::Sync& completer) {
              EXPECT_EQ(request.image_source(), sources[index]);
              EXPECT_EQ(request.display_destination(), destinations[index]);
            }));

    EXPECT_CALL(
        *mock_display_coordinator_,
        SetLayerPrimaryAlpha(MatchRequestField(SetLayerPrimaryAlpha, layer_id, Eq(layers[i])), _))
        .Times(1)
        .WillOnce(Return());

    EXPECT_CALL(
        *mock_display_coordinator_,
        SetLayerImage(
            testing::AllOf(MatchRequestField(SetLayerImage, layer_id, Eq(layers[i])),
                           MatchRequestField(SetLayerImage, image_id, Eq(fidl_image_ids[i]))),
            _))
        .Times(1)
        .WillOnce(Return());
  }
  EXPECT_CALL(*mock_display_coordinator_, ImportEvent(_, _)).Times(2);

  EXPECT_CALL(*mock_display_coordinator_, SetDisplayColorConversion(_, _)).Times(1);

  EXPECT_CALL(*mock_display_coordinator_,
              CheckConfig(MatchRequestField(CheckConfig, discard, false), _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
      }));

  EXPECT_CALL(*renderer_, ChoosePreferredRenderTargetFormat(_));

  DisplayInfo display_info = {resolution, {kPixelFormat}};
  scenic_impl::display::Display display(kDisplayId, resolution.x, resolution.y);
  display_compositor_->AddDisplay(&display, display_info, /*num_vmos*/ 0,
                                  /*out_buffer_collection*/ nullptr);

  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig(_)).Times(1).WillOnce(Return());

  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCompleter::Sync& completer) {
            const fuchsia_hardware_display_types::ConfigStamp stamp = {1};
            completer.Reply({{.stamp = stamp}});
          }));

  display_compositor_->RenderFrame(
      1, zx::time(1),
      GenerateDisplayListForTest({{kDisplayId.value(), {display_info, parent_root_handle}}}), {},
      [](const scheduling::Timestamps&) {});

  for (uint32_t i = 0; i < 2; i++) {
    EXPECT_CALL(*mock_display_coordinator_,
                DestroyLayer(MatchRequestField(DestroyLayer, layer_id, Eq(layers[i])), _))
        .Times(1)
        .WillOnce(Return());
  }

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
      }));

  display_compositor_.reset();
}

void DisplayCompositorTest::HardwareFrameCorrectnessWithRotationTester(
    glm::mat3 transform_matrix, ImageFlip image_flip, const fuchsia_math::RectU expected_dst,
    fuchsia_hardware_display_types::CoordinateTransformation expected_transform) {
  const uint64_t kGlobalBufferCollectionId = allocation::GenerateUniqueBufferCollectionId();
  const fuchsia_hardware_display::BufferCollectionId kDisplayBufferCollectionId =
      scenic_impl::ToDisplayFidlBufferCollectionId(kGlobalBufferCollectionId);

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
      .blend_mode = BlendMode::kSrc,
      .flip = image_flip,
  };
  parent_struct->images[parent_image_handle] = parent_image_metadata;

  parent_struct->local_matrices[parent_image_handle] = std::move(transform_matrix);
  parent_struct->local_image_sample_regions[parent_image_handle] = {0, 0, 128, 256};

  // Submit the UberStruct.
  parent_session.PushUberStruct(std::move(parent_struct));

  const fuchsia_hardware_display_types::DisplayId kDisplayId = {{.value = 1}};
  glm::uvec2 resolution(1024, 768);

  // We will end up with 1 source frame, 1 destination frame, and one layer being sent to the
  // display.
  fuchsia_math::RectU expected_source = {{.x = 0u, .y = 0u, .width = 128u, .height = 256u}};

  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(MatchRequestField(ImportBufferCollection, buffer_collection_id,
                                                       Eq(kDisplayBufferCollectionId)),
                                     _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::ImportBufferCollectionRequest& request,
             MockDisplayCoordinator::ImportBufferCollectionCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(
                  MatchRequestField(SetBufferCollectionConstraints, buffer_collection_id,
                                    Eq(kDisplayBufferCollectionId)),
                  _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::SetBufferCollectionConstraintsRequest&,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId,
                             fuchsia::sysmem2::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token,
                             BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        token_ref = std::move(token);
        return true;
      });
  display_compositor_->ImportBufferCollection(kGlobalBufferCollectionId, sysmem_allocator_.get(),
                                              CreateToken(), BufferCollectionUsage::kClientImage,
                                              std::nullopt);
  SetDisplaySupported(kGlobalBufferCollectionId, true);

  const fuchsia_hardware_display::ImageId fidl_parent_image_id =
      scenic_impl::ToDisplayFidlImageId(parent_image_metadata.identifier);
  EXPECT_CALL(
      *mock_display_coordinator_,
      ImportImage(
          testing::AllOf(MatchRequestField(ImportImage, buffer_id,
                                           Eq(fuchsia_hardware_display::BufferId{{
                                               .buffer_collection_id = kDisplayBufferCollectionId,
                                               .buffer_index = 0,
                                           }})),
                         MatchRequestField(ImportImage, image_id, Eq(fidl_parent_image_id))),
          _))
      .Times(1)
      .WillOnce(testing::Invoke([](MockDisplayCoordinator::ImportImageRequest&,
                                   MockDisplayCoordinator::ImportImageCompleter::Sync& completer) {
        completer.Reply(fit::ok());
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
      .WillRepeatedly(
          testing::Invoke([&](MockDisplayCoordinator::CreateLayerCompleter::Sync& completer) {
            fuchsia_hardware_display::CoordinatorCreateLayerResponse response(
                fuchsia_hardware_display::LayerId{{.value = layer_id_value++}});
            completer.Reply(fit::ok(std::move(response)));
          }));

  // However, we only set one display layer for the image.
  const std::vector<fuchsia_hardware_display::LayerId> layers = {
      {{.value = 1}},
  };
  EXPECT_CALL(*mock_display_coordinator_,
              SetDisplayLayers(
                  testing::AllOf(MatchRequestField(SetDisplayLayers, display_id, Eq(kDisplayId)),
                                 MatchRequestField(SetDisplayLayers, layer_ids,
                                                   testing::ElementsAreArray(layers))),
                  _))
      .Times(1)
      .WillOnce(Return());

  const fuchsia_hardware_display::ImageId fidl_collection_image_id =
      scenic_impl::ToDisplayFidlImageId(parent_image_metadata.identifier);
  EXPECT_CALL(
      *mock_display_coordinator_,
      SetLayerPrimaryConfig(MatchRequestField(SetLayerPrimaryConfig, layer_id, Eq(layers[0])), _))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(
      *mock_display_coordinator_,
      SetLayerPrimaryPosition(
          testing::AllOf(MatchRequestField(SetLayerPrimaryPosition, layer_id, Eq(layers[0])),
                         MatchRequestField(SetLayerPrimaryPosition, image_source_transformation,
                                           Eq(expected_transform))),
          _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [expected_source, expected_dst](
              MockDisplayCoordinator::SetLayerPrimaryPositionRequest& request,
              MockDisplayCoordinator::SetLayerPrimaryPositionCompleter::Sync& completer) {
            EXPECT_EQ(request.image_source(), expected_source);
            EXPECT_EQ(request.display_destination(), expected_dst);
          }));
  EXPECT_CALL(
      *mock_display_coordinator_,
      SetLayerPrimaryAlpha(MatchRequestField(SetLayerPrimaryAlpha, layer_id, Eq(layers[0])), _))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(
      *mock_display_coordinator_,
      SetLayerImage(
          testing::AllOf(MatchRequestField(SetLayerImage, layer_id, Eq(layers[0])),
                         MatchRequestField(SetLayerImage, image_id, Eq(fidl_collection_image_id))),
          _))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_, ImportEvent(_, _)).Times(1);

  EXPECT_CALL(*mock_display_coordinator_, SetDisplayColorConversion(_, _)).Times(1);

  EXPECT_CALL(*mock_display_coordinator_,
              CheckConfig(MatchRequestField(CheckConfig, discard, false), _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
      }));

  EXPECT_CALL(*renderer_, ChoosePreferredRenderTargetFormat(_));

  DisplayInfo display_info = {resolution, {kPixelFormat}};
  scenic_impl::display::Display display(kDisplayId, resolution.x, resolution.y);
  display_compositor_->AddDisplay(&display, display_info, /*num_vmos*/ 0,
                                  /*out_buffer_collection*/ nullptr);

  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig(_)).Times(1).WillOnce(Return());

  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCompleter::Sync& completer) {
            const fuchsia_hardware_display_types::ConfigStamp stamp = {1};
            completer.Reply({{.stamp = stamp}});
          }));

  display_compositor_->RenderFrame(
      1, zx::time(1),
      GenerateDisplayListForTest({{kDisplayId.value(), {display_info, parent_root_handle}}}), {},
      [](const scheduling::Timestamps&) {});

  for (uint64_t i = 1; i < layer_id_value; ++i) {
    EXPECT_CALL(*mock_display_coordinator_,
                DestroyLayer(MatchRequestField(DestroyLayer, layer_id,
                                               Eq(fuchsia_hardware_display::LayerId{{.value = i}})),
                             _))
        .Times(1)
        .WillOnce(Return());
  }

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
      }));

  display_compositor_.reset();
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWith90DegreeRotationTest) {
  // After scale and 90 CCW rotation, the new top-left corner would be (0, -10). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(0, 10));
  matrix = glm::rotate(matrix, -glm::half_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  const fuchsia_math::RectU expected_dst = {{.x = 0u, .y = 0u, .width = 20u, .height = 10u}};

  HardwareFrameCorrectnessWithRotationTester(
      matrix, ImageFlip::kNone, expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw90);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWith180DegreeRotationTest) {
  // After scale and 180 CCW rotation, the new top-left corner would be (-10, -20).
  // Translate back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(10, 20));
  matrix = glm::rotate(matrix, -glm::pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  const fuchsia_math::RectU expected_dst = {{.x = 0u, .y = 0u, .width = 10u, .height = 20u}};

  HardwareFrameCorrectnessWithRotationTester(
      matrix, ImageFlip::kNone, expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw180);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWith270DegreeRotationTest) {
  // After scale and 270 CCW rotation, the new top-left corner would be (-20, 0). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(20, 0));
  matrix = glm::rotate(matrix, -glm::three_over_two_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  const fuchsia_math::RectU expected_dst = {{.x = 0u, .y = 0u, .width = 20u, .height = 10u}};

  HardwareFrameCorrectnessWithRotationTester(
      matrix, ImageFlip::kNone, expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw270);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlipTest) {
  glm::mat3 matrix = glm::scale(glm::mat3(), glm::vec2(10, 20));

  const fuchsia_math::RectU expected_dst = {{.x = 0u, .y = 0u, .width = 10u, .height = 20u}};

  HardwareFrameCorrectnessWithRotationTester(
      matrix, ImageFlip::kLeftRight, expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation::kReflectY);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlipTest) {
  glm::mat3 matrix = glm::scale(glm::mat3(), glm::vec2(10, 20));

  const fuchsia_math::RectU expected_dst = {{.x = 0u, .y = 0u, .width = 10u, .height = 20u}};

  HardwareFrameCorrectnessWithRotationTester(
      matrix, ImageFlip::kUpDown, expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation::kReflectX);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlip90DegreeRotationTest) {
  // After scale and 90 CCW rotation, the new top-left corner would be (0, -10). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(0, 10));
  matrix = glm::rotate(matrix, -glm::half_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  const fuchsia_math::RectU expected_dst = {{.x = 0u, .y = 0u, .width = 20u, .height = 10u}};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(
      matrix, ImageFlip::kLeftRight, expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw90ReflectX);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlip90DegreeRotationTest) {
  // After scale and 90 CCW rotation, the new top-left corner would be (0, -10). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(0, 10));
  matrix = glm::rotate(matrix, -glm::half_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  const fuchsia_math::RectU expected_dst = {{.x = 0u, .y = 0u, .width = 20u, .height = 10u}};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(
      matrix, ImageFlip::kUpDown, expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw90ReflectY);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlip180DegreeRotationTest) {
  // After scale and 180 CCW rotation, the new top-left corner would be (-10, -20).
  // Translate back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(10, 20));
  matrix = glm::rotate(matrix, -glm::pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  const fuchsia_math::RectU expected_dst = {{.x = 0u, .y = 0u, .width = 10u, .height = 20u}};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(
      matrix, ImageFlip::kLeftRight, expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation::kReflectX);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlip180DegreeRotationTest) {
  // After scale and 180 CCW rotation, the new top-left corner would be (-10, -20).
  // Translate back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(10, 20));
  matrix = glm::rotate(matrix, -glm::pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  const fuchsia_math::RectU expected_dst = {{.x = 0u, .y = 0u, .width = 10u, .height = 20u}};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(
      matrix, ImageFlip::kUpDown, expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation::kReflectY);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithLeftRightFlip270DegreeRotationTest) {
  // After scale and 270 CCW rotation, the new top-left corner would be (-20, 0). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(20, 0));
  matrix = glm::rotate(matrix, -glm::three_over_two_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  const fuchsia_math::RectU expected_dst = {{.x = 0u, .y = 0u, .width = 20u, .height = 10u}};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(
      matrix, ImageFlip::kLeftRight, expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw90ReflectY);
}

TEST_F(DisplayCompositorTest, HardwareFrameCorrectnessWithUpDownFlip270DegreeRotationTest) {
  // After scale and 270 CCW rotation, the new top-left corner would be (-20, 0). Translate
  // back to position.
  glm::mat3 matrix = glm::translate(glm::mat3(1.0), glm::vec2(20, 0));
  matrix = glm::rotate(matrix, -glm::three_over_two_pi<float>());
  matrix = glm::scale(matrix, glm::vec2(10, 20));

  const fuchsia_math::RectU expected_dst = {{.x = 0u, .y = 0u, .width = 20u, .height = 10u}};

  // The expected display coordinator transform performs rotation before reflection.
  HardwareFrameCorrectnessWithRotationTester(
      matrix, ImageFlip::kUpDown, expected_dst,
      fuchsia_hardware_display_types::CoordinateTransformation::kRotateCcw90ReflectX);
}

TEST_F(DisplayCompositorTest, ChecksDisplayImageSignalFences) {
  const uint64_t kGlobalBufferCollectionId = 1;
  const fuchsia_hardware_display::BufferCollectionId kDisplayBufferCollectionId =
      scenic_impl::ToDisplayFidlBufferCollectionId(kGlobalBufferCollectionId);

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
      .blend_mode = BlendMode::kSrc,
  };
  uber_struct->images[image_handle] = image_metadata;
  uber_struct->local_matrices[image_handle] =
      glm::scale(glm::translate(glm::mat3(1.0), glm::vec2(9, 13)), glm::vec2(10, 20));
  uber_struct->local_image_sample_regions[image_handle] = {0, 0, 128, 256};

  // Submit the UberStruct.
  session.PushUberStruct(std::move(uber_struct));

  // Import buffer collection.
  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(MatchRequestField(ImportBufferCollection, buffer_collection_id,
                                                       Eq(kDisplayBufferCollectionId)),
                                     _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::ImportBufferCollectionRequest& request,
             MockDisplayCoordinator::ImportBufferCollectionCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetBufferCollectionConstraints(
                  MatchRequestField(SetBufferCollectionConstraints, buffer_collection_id,
                                    Eq(kDisplayBufferCollectionId)),
                  _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [](MockDisplayCoordinator::SetBufferCollectionConstraintsRequest&,
             MockDisplayCoordinator::SetBufferCollectionConstraintsCompleter::Sync& completer) {
            completer.Reply(fit::ok());
          }));
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId,
                             fuchsia::sysmem2::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token,
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
              ImportImage(MatchRequestField(ImportImage, buffer_id,
                                            Eq(fuchsia_hardware_display::BufferId{{
                                                .buffer_collection_id = kDisplayBufferCollectionId,
                                                .buffer_index = 0,
                                            }})),
                          _))
      .Times(1)
      .WillOnce(testing::Invoke([](MockDisplayCoordinator::ImportImageRequest&,
                                   MockDisplayCoordinator::ImportImageCompleter::Sync& completer) {
        completer.Reply(fit::ok());
      }));

  EXPECT_CALL(*renderer_, ImportBufferImage(image_metadata, _)).WillOnce(Return(true));
  display_compositor_->ImportBufferImage(image_metadata, BufferCollectionUsage::kClientImage);

  // We start the frame by clearing the config.
  // In the end when the DisplayCompositor is destroyed, the config will also
  // be discarded.
  EXPECT_CALL(*mock_display_coordinator_,
              CheckConfig(MatchRequestField(CheckConfig, discard, true), _))
      .Times(2)
      .WillRepeatedly(
          testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                              MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
            completer.Reply(
                {{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
          }));

  // Set expectation for CreateLayer calls.
  uint64_t layer_id_value = 1;
  std::vector<fuchsia_hardware_display::LayerId> layers = {{{.value = 1}}, {{.value = 2}}};
  EXPECT_CALL(*mock_display_coordinator_, CreateLayer(_))
      .Times(2)
      .WillRepeatedly(
          testing::Invoke([&](MockDisplayCoordinator::CreateLayerCompleter::Sync& completer) {
            fuchsia_hardware_display::CoordinatorCreateLayerResponse response(
                fuchsia_hardware_display::LayerId{{.value = layer_id_value++}});
            completer.Reply(fit::ok(std::move(response)));
          }));
  EXPECT_CALL(*renderer_, ChoosePreferredRenderTargetFormat(_));

  // Add display.
  const fuchsia_hardware_display_types::DisplayId kDisplayId = {{.value = 1}};
  glm::uvec2 kResolution(1024, 768);
  DisplayInfo display_info = {kResolution, {kPixelFormat}};
  scenic_impl::display::Display display(kDisplayId, kResolution.x, kResolution.y);
  display_compositor_->AddDisplay(&display, display_info, /*num_vmos*/ 0,
                                  /*out_buffer_collection*/ nullptr);

  // Set expectation for rendering image on layer.
  std::vector<fuchsia_hardware_display::LayerId> active_layers = {
      {{.value = 1}},
  };
  zx::event imported_event;
  EXPECT_CALL(*mock_display_coordinator_, ImportEvent(_, _))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&imported_event](MockDisplayCoordinator::ImportEventRequest& request,
                            MockDisplayCoordinator::ImportEventCompleter::Sync& completer) {
            imported_event = std::move(request.event());
          }));
  EXPECT_CALL(*mock_display_coordinator_,
              SetDisplayLayers(
                  testing::AllOf(MatchRequestField(SetDisplayLayers, display_id, Eq(kDisplayId)),
                                 MatchRequestField(SetDisplayLayers, layer_ids,
                                                   testing::ElementsAreArray(active_layers))),
                  _))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_,
              SetLayerPrimaryConfig(
                  MatchRequestField(SetLayerPrimaryConfig, layer_id, Eq(active_layers[0])), _))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_,
              SetLayerPrimaryPosition(
                  MatchRequestField(SetLayerPrimaryPosition, layer_id, Eq(active_layers[0])), _))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_,
              SetLayerPrimaryAlpha(
                  MatchRequestField(SetLayerPrimaryAlpha, layer_id, Eq(active_layers[0])), _))
      .Times(1)
      .WillOnce(Return());

  EXPECT_CALL(*mock_display_coordinator_,
              SetLayerImage(MatchRequestField(SetLayerImage, layer_id, Eq(active_layers[0])), _))
      .Times(1)
      .WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_,
              CheckConfig(MatchRequestField(CheckConfig, discard, false), _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
      }));
  EXPECT_CALL(*mock_display_coordinator_, ApplyConfig(_)).Times(1).WillOnce(Return());
  EXPECT_CALL(*mock_display_coordinator_, GetLatestAppliedConfigStamp(_))
      .Times(1)
      .WillOnce(testing::Invoke(
          [&](MockDisplayCoordinator::GetLatestAppliedConfigStampCompleter::Sync& completer) {
            const fuchsia_hardware_display_types::ConfigStamp stamp = {1};
            completer.Reply({{.stamp = stamp}});
          }));

  // Render image. This should end up in display.
  const auto& display_list =
      GenerateDisplayListForTest({{kDisplayId.value(), {display_info, root_handle}}});
  display_compositor_->RenderFrame(1, zx::time(1), display_list, {},
                                   [](const scheduling::Timestamps&) {});

  // Try rendering again. Because |imported_event| isn't signaled and no render targets
  // were created when adding display, we should fail.
  auto status = imported_event.wait_one(ZX_EVENT_SIGNALED, zx::time(), nullptr);
  EXPECT_NE(status, ZX_OK);
  display_compositor_->RenderFrame(1, zx::time(1), display_list, {},
                                   [](const scheduling::Timestamps&) {});

  for (uint32_t i = 0; i < 2; i++) {
    EXPECT_CALL(*mock_display_coordinator_,
                DestroyLayer(MatchRequestField(DestroyLayer, layer_id, Eq(layers[i])), _))
        .Times(1)
        .WillOnce(Return());
  }
  display_compositor_.reset();
}

// Tests that RenderOnly mode does not attempt to ImportBufferCollection() to display.
TEST_F(DisplayCompositorTest, RendererOnly_ImportAndReleaseBufferCollectionTest) {
  ForceRendererOnlyMode(true);

  const allocation::GlobalBufferCollectionId kGlobalBufferCollectionId = 15;
  const fuchsia_hardware_display::BufferCollectionId kDisplayBufferCollectionId =
      scenic_impl::ToDisplayFidlBufferCollectionId(kGlobalBufferCollectionId);

  EXPECT_CALL(*mock_display_coordinator_,
              ImportBufferCollection(MatchRequestField(ImportBufferCollection, buffer_collection_id,
                                                       Eq(kDisplayBufferCollectionId)),
                                     _))
      .Times(0);
  // Save token to avoid early token failure.
  fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token_ref;
  EXPECT_CALL(*renderer_, ImportBufferCollection(kGlobalBufferCollectionId, _, _, _, _))
      .WillOnce([&token_ref](allocation::GlobalBufferCollectionId,
                             fuchsia::sysmem2::Allocator_Sync*,
                             fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token,
                             BufferCollectionUsage, std::optional<fuchsia::math::SizeU>) {
        token_ref = std::move(token);
        return true;
      });
  display_compositor_->ImportBufferCollection(kGlobalBufferCollectionId, sysmem_allocator_.get(),
                                              CreateToken(), BufferCollectionUsage::kClientImage,
                                              std::nullopt);

  EXPECT_CALL(
      *mock_display_coordinator_,
      ReleaseBufferCollection(MatchRequestField(ReleaseBufferCollection, buffer_collection_id,
                                                Eq(kDisplayBufferCollectionId)),
                              _))
      .Times(1)
      .WillOnce(Return());

  EXPECT_CALL(*renderer_, ReleaseBufferCollection(kGlobalBufferCollectionId, _)).WillOnce(Return());
  display_compositor_->ReleaseBufferCollection(kGlobalBufferCollectionId,
                                               BufferCollectionUsage::kClientImage);

  EXPECT_CALL(*mock_display_coordinator_, CheckConfig(_, _))
      .Times(1)
      .WillOnce(testing::Invoke([&](MockDisplayCoordinator::CheckConfigRequest&,
                                    MockDisplayCoordinator::CheckConfigCompleter::Sync& completer) {
        completer.Reply({{.res = fuchsia_hardware_display_types::ConfigResult::kOk, .ops = {}}});
      }));
  display_compositor_.reset();
}

}  // namespace flatland::test
