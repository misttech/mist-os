// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.ui.composition/cpp/fidl.h>
#include <lib/fit/defer.h>
#include <lib/zircon-internal/align.h>

#include "src/graphics/display/lib/coordinator-getter/client.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/ui/lib/escher/flatland/rectangle_compositor.h"
#include "src/ui/lib/escher/impl/vulkan_utils.h"
#include "src/ui/lib/escher/renderer/batch_gpu_downloader.h"
#include "src/ui/lib/escher/renderer/batch_gpu_uploader.h"
#include "src/ui/lib/escher/test/common/gtest_escher.h"
#include "src/ui/lib/escher/util/image_utils.h"
#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"
#include "src/ui/scenic/lib/display/display_manager.h"
#include "src/ui/scenic/lib/display/util.h"
#include "src/ui/scenic/lib/flatland/buffers/util.h"
#include "src/ui/scenic/lib/flatland/engine/tests/common.h"
#include "src/ui/scenic/lib/flatland/renderer/vk_renderer.h"
#include "src/ui/scenic/lib/utils/helpers.h"

using ::testing::_;
using ::testing::Return;

using allocation::BufferCollectionUsage;
using allocation::ImageMetadata;
using flatland::LinkSystem;
using flatland::Renderer;
using flatland::TransformGraph;
using flatland::TransformHandle;
using flatland::UberStruct;
using flatland::UberStructSystem;
using fuchsia::ui::composition::ChildViewStatus;
using fuchsia::ui::composition::ChildViewWatcher;
using fuchsia::ui::composition::LayoutInfo;
using fuchsia::ui::composition::ParentViewportWatcher;
using fuchsia::ui::composition::ViewportProperties;
using fuchsia::ui::views::ViewCreationToken;
using fuchsia::ui::views::ViewportCreationToken;

namespace flatland {
namespace test {

// The smoke tests are used to ensure that we can get testing of the Flatland
// Display Compositor across a variety of test hardware configurations, including
// those that do not have a real display, and those where making sysmem buffer
// collection vmos host-accessible (i.e. cpu accessible) is not allowed, precluding
// the possibility of doing a pixel readback on the framebuffers.
class DisplayCompositorSmokeTest : public DisplayCompositorTestBase {
 public:
  void SetUp() override {
    DisplayCompositorTestBase::SetUp();

    // Create the SysmemAllocator.
    zx_status_t status = fdio_service_connect(
        "/svc/fuchsia.sysmem2.Allocator", sysmem_allocator_.NewRequest().TakeChannel().release());
    EXPECT_EQ(status, ZX_OK);
    fuchsia::sysmem2::AllocatorSetDebugClientInfoRequest set_debug_request;
    set_debug_request.set_name(fsl::GetCurrentProcessName() + " DisplayCompositorSmokeTest");
    set_debug_request.set_id(fsl::GetCurrentProcessKoid());
    sysmem_allocator_->SetDebugClientInfo(std::move(set_debug_request));

    executor_ = std::make_unique<async::Executor>(dispatcher());

    display_manager_ = std::make_unique<scenic_impl::display::DisplayManager>([]() {});

    // TODO(https://fxbug.dev/42073120): This reuses the display coordinator from previous
    // test cases in the same test component, so the display coordinator may be
    // in a dirty state. Tests should request a reset of display coordinator
    // here.
    auto hdc_promise = display::GetCoordinator();
    executor_->schedule_task(hdc_promise.then(
        [this](fpromise::result<display::CoordinatorClientChannels, zx_status_t>& client_channels) {
          ASSERT_TRUE(client_channels.is_ok()) << "Failed to get display coordinator:"
                                               << zx_status_get_string(client_channels.error());
          auto [coordinator_client, listener_server] = std::move(client_channels.value());
          display_manager_->BindDefaultDisplayCoordinator(
              dispatcher(), std::move(coordinator_client), std::move(listener_server));
        }));

    RunLoopUntil([this] { return display_manager_->default_display() != nullptr; });
  }

  void TearDown() override {
    RunLoopUntilIdle();
    executor_.reset();
    display_manager_.reset();
    DisplayCompositorTestBase::TearDown();
  }

  bool IsDisplaySupported(DisplayCompositor* display_compositor,
                          allocation::GlobalBufferCollectionId id) {
    std::scoped_lock lock(display_compositor->lock_);
    return display_compositor->buffer_collection_supports_display_[id];
  }

 protected:
  static constexpr fuchsia_images2::PixelFormat kPixelFormat =
      fuchsia_images2::PixelFormat::kB8G8R8A8;

  fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator_;
  std::unique_ptr<async::Executor> executor_;
  std::unique_ptr<scenic_impl::display::DisplayManager> display_manager_;

  static std::pair<std::unique_ptr<escher::Escher>, std::shared_ptr<flatland::VkRenderer>>
  NewVkRenderer() {
    auto env = escher::test::EscherEnvironment::GetGlobalTestEnvironment();
    auto unique_escher = std::make_unique<escher::Escher>(
        env->GetVulkanDevice(), env->GetFilesystem(), /*gpu_allocator*/ nullptr);
    return {std::move(unique_escher),
            std::make_shared<flatland::VkRenderer>(unique_escher->GetWeakPtr())};
  }

  static std::shared_ptr<flatland::NullRenderer> NewNullRenderer() {
    return std::make_shared<flatland::NullRenderer>();
  }

  // Sets up the buffer collection information for collections that will be imported
  // into the engine.
  fuchsia::sysmem2::BufferCollectionSyncPtr SetupClientTextures(
      DisplayCompositor* display_compositor, allocation::GlobalBufferCollectionId collection_id,
      fuchsia::images2::PixelFormat pixel_type, uint32_t width, uint32_t height, uint32_t num_vmos,
      fuchsia::sysmem2::BufferCollectionInfo* collection_info) {
    // Setup the buffer collection that will be used for the flatland rectangle's texture.
    auto texture_tokens = SysmemTokens::Create(sysmem_allocator_.get());

    auto result = display_compositor->ImportBufferCollection(
        collection_id, sysmem_allocator_.get(), std::move(texture_tokens.dup_token),
        BufferCollectionUsage::kClientImage, std::nullopt);
    EXPECT_TRUE(result);

    auto [buffer_usage, memory_constraints] = GetUsageAndMemoryConstraintsForCpuWriteOften();
    fuchsia::sysmem2::BufferCollectionSyncPtr texture_collection =
        CreateBufferCollectionSyncPtrAndSetConstraints(
            sysmem_allocator_.get(), std::move(texture_tokens.local_token), num_vmos, width, height,
            fidl::Clone(buffer_usage), pixel_type, fidl::Clone(memory_constraints),
            std::make_optional(fuchsia::images2::PixelFormatModifier::LINEAR));

    // Have the client wait for buffers allocated so it can populate its information
    // struct with the vmo data.
    fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
    auto status = texture_collection->WaitForAllBuffersAllocated(&wait_result);
    EXPECT_EQ(status, ZX_OK);
    EXPECT_TRUE(!wait_result.is_framework_err());
    EXPECT_TRUE(!wait_result.is_err());
    EXPECT_TRUE(wait_result.is_response());
    *collection_info = std::move(*wait_result.response().mutable_buffer_collection_info());

    return texture_collection;
  }
};

class DisplayCompositorParameterizedSmokeTest
    : public DisplayCompositorSmokeTest,
      public ::testing::WithParamInterface<fuchsia::images2::PixelFormat> {};

namespace {

// Renders a fullscreen rectangle to the provided display. This tests the engine's ability to
// properly read in flatland uberstruct data and then pass the data along to the display-coordinator
// interface to be composited directly in hardware. The Astro display coordinator only handles full
// screen rects.
VK_TEST_P(DisplayCompositorParameterizedSmokeTest, FullscreenRectangleTest) {
  // Even though we are rendering directly with the display coordinator in this test,
  // we still use the VkRenderer so that all of the same constraints we'd expect to
  // see set in a real production setting are reproduced here.
  auto [escher, renderer] = NewVkRenderer();
  auto display_compositor = std::make_shared<flatland::DisplayCompositor>(
      dispatcher(), display_manager_->default_display_coordinator(), renderer,
      utils::CreateSysmemAllocatorSyncPtr("display_compositor_pixeltest"),
      /*enable_display_composition*/ true, /*max_display_layers=*/1, /*visual_debug_level=*/0);

  auto display = display_manager_->default_display();
  auto display_coordinator = display_manager_->default_display_coordinator();

  const uint64_t kTextureCollectionId = allocation::GenerateUniqueBufferCollectionId();

  // Setup the collection for the texture. Due to display coordinator limitations, the size of
  // the texture needs to match the size of the rect. So since we have a fullscreen rect, we
  // must also have a fullscreen texture to match.
  const uint32_t kRectWidth = display->width_in_px(), kTextureWidth = display->width_in_px();
  const uint32_t kRectHeight = display->height_in_px(), kTextureHeight = display->height_in_px();
  fuchsia::sysmem2::BufferCollectionInfo texture_collection_info;
  auto texture_collection =
      SetupClientTextures(display_compositor.get(), kTextureCollectionId, GetParam(), kTextureWidth,
                          kTextureHeight, 1, &texture_collection_info);
  EXPECT_TRUE(texture_collection);
  auto release_texture_collection = fit::defer([display_compositor, kTextureCollectionId] {
    display_compositor->ReleaseBufferCollection(kTextureCollectionId,
                                                BufferCollectionUsage::kClientImage);
  });

  // Import the texture to the engine.
  auto image_metadata = ImageMetadata{.collection_id = kTextureCollectionId,
                                      .identifier = allocation::GenerateUniqueImageId(),
                                      .vmo_index = 0,
                                      .width = kTextureWidth,
                                      .height = kTextureHeight,
                                      .blend_mode = fuchsia_ui_composition::BlendMode::kSrc};
  auto result =
      display_compositor->ImportBufferImage(image_metadata, BufferCollectionUsage::kClientImage);
  EXPECT_TRUE(result);

  // We cannot send to display because it is not supported in allocations.
  EXPECT_TRUE(IsDisplaySupported(display_compositor.get(), kTextureCollectionId));

  // Create a flatland session with a root and image handle. Import to the engine as display root.
  auto session = CreateSession();
  const TransformHandle root_handle = session.graph().CreateTransform();
  const TransformHandle image_handle = session.graph().CreateTransform();
  session.graph().AddChild(root_handle, image_handle);
  DisplayInfo display_info{
      .dimensions = glm::uvec2(display->width_in_px(), display->height_in_px()),
      .formats = {kPixelFormat}};
  display_compositor->AddDisplay(display, display_info, /*num_vmos*/ 0,
                                 /*out_collection_info*/ nullptr);

  // Setup the uberstruct data.
  auto uberstruct = session.CreateUberStructWithCurrentTopology(root_handle);
  uberstruct->images[image_handle] = image_metadata;
  uberstruct->local_matrices[image_handle] = glm::scale(
      glm::translate(glm::mat3(1.0), glm::vec2(0, 0)), glm::vec2(kRectWidth, kRectHeight));
  uberstruct->local_image_sample_regions[image_handle] = {0, 0, static_cast<float>(kTextureWidth),
                                                          static_cast<float>(kTextureHeight)};
  session.PushUberStruct(std::move(uberstruct));

  // Now we can finally render.
  display_compositor->RenderFrame(
      1, zx::time(1),
      GenerateDisplayListForTest(
          {{display->display_id().value, std::make_pair(display_info, root_handle)}}),
      {}, [](const scheduling::Timestamps&) {});
}

// TODO(https://fxbug.dev/42154038): Add YUV formats when they are supported by fake or real
// display.
INSTANTIATE_TEST_SUITE_P(PixelFormats, DisplayCompositorParameterizedSmokeTest,
                         ::testing::Values(fuchsia::images2::PixelFormat::B8G8R8A8,
                                           fuchsia::images2::PixelFormat::R8G8B8A8));

}  // namespace

}  // namespace test
}  // namespace flatland
