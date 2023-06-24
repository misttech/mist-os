// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-testing/test_loop.h>
#include <lib/async/cpp/executor.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/default.h>
#include <lib/fdio/directory.h>
#include <lib/fit/defer.h>

#include <thread>

#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/ui/lib/display/get_hardware_display_controller.h"
#include "src/ui/lib/escher/vk/vulkan_device_queues.h"
#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"
#include "src/ui/scenic/lib/display/display_manager.h"
#include "src/ui/scenic/lib/display/util.h"
#include "src/ui/scenic/lib/flatland/renderer/null_renderer.h"
#include "src/ui/scenic/lib/flatland/renderer/tests/common.h"
#include "src/ui/scenic/lib/flatland/renderer/vk_renderer.h"
#include "src/ui/scenic/lib/utils/helpers.h"

#include <glm/gtc/constants.hpp>
#include <glm/gtx/matrix_transform_2d.hpp>

namespace {

class DisplayTest : public gtest::RealLoopFixture {
 protected:
  void SetUp() override {
    if (VK_TESTS_SUPPRESSED()) {
      return;
    }
    gtest::RealLoopFixture::SetUp();

    sysmem_allocator_ = utils::CreateSysmemAllocatorSyncPtr("display_unittest::Setup");

    async_set_default_dispatcher(dispatcher());
    executor_ = std::make_unique<async::Executor>(dispatcher());

    display_manager_ = std::make_unique<scenic_impl::display::DisplayManager>([]() {});

    // TODO(fxbug.dev/122131): This reuses the display coordinator from previous
    // test cases in the same test component, so the display coordinator may be
    // in a dirty state. Tests should request a reset of display coordinator
    // here.
    auto hdc_promise = ui_display::GetHardwareDisplayCoordinator();
    executor_->schedule_task(hdc_promise.then(
        [this](fpromise::result<ui_display::DisplayCoordinatorHandles, zx_status_t>& handles) {
          display_manager_->BindDefaultDisplayCoordinator(std::move(handles.value().coordinator));
        }));

    RunLoopUntil([this] { return display_manager_->default_display() != nullptr; });
  }

  void TearDown() override {
    if (VK_TESTS_SUPPRESSED()) {
      return;
    }
    executor_.reset();
    display_manager_.reset();
    sysmem_allocator_ = nullptr;
    gtest::RealLoopFixture::TearDown();
  }

  uint64_t InitializeDisplayLayer(
      fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator,
      scenic_impl::display::Display* display) {
    uint64_t layer_id;
    zx_status_t create_layer_status;
    zx_status_t transport_status =
        display_coordinator->CreateLayer(&create_layer_status, &layer_id);
    if (create_layer_status != ZX_OK || transport_status != ZX_OK) {
      FX_LOGS(ERROR) << "Failed to create layer, " << create_layer_status;
      return 0;
    }

    zx_status_t status = display_coordinator->SetDisplayLayers(display->display_id(), {layer_id});
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "Failed to configure display layers. Error code: " << status;
      return 0;
    }

    return layer_id;
  }

  std::unique_ptr<async::Executor> executor_;
  std::unique_ptr<scenic_impl::display::DisplayManager> display_manager_;
  fuchsia::sysmem::AllocatorSyncPtr sysmem_allocator_;
};

// Create a buffer collection and set constraints on the display, the vulkan renderer
// and the client, and make sure that the collection is still properly allocated.
VK_TEST_F(DisplayTest, SetAllConstraintsTest) {
  const uint64_t kWidth = 8;
  const uint64_t kHeight = 16;

  // Grab the display coordinator.
  auto display_coordinator = display_manager_->default_display_coordinator();
  EXPECT_TRUE(display_coordinator);

  // Create the VK renderer.
  auto env = escher::test::EscherEnvironment::GetGlobalTestEnvironment();
  auto unique_escher = std::make_unique<escher::Escher>(
      env->GetVulkanDevice(), env->GetFilesystem(), /*gpu_allocator*/ nullptr);
  flatland::VkRenderer renderer(unique_escher->GetWeakPtr());

  // First create the pair of sysmem tokens, one for the client, one for the renderer.
  auto tokens = flatland::SysmemTokens::Create(sysmem_allocator_.get());

  fuchsia::sysmem::BufferCollectionTokenSyncPtr display_token;
  zx_status_t status = tokens.local_token->Duplicate(std::numeric_limits<uint32_t>::max(),
                                                     display_token.NewRequest());
  FX_DCHECK(status == ZX_OK);

  // Register the collection with the renderer, which sets the vk constraints.
  auto collection_id = allocation::GenerateUniqueBufferCollectionId();
  auto image_id = allocation::GenerateUniqueImageId();
  auto result = renderer.ImportBufferCollection(
      collection_id, sysmem_allocator_.get(), std::move(tokens.dup_token),
      allocation::BufferCollectionUsage::kClientImage, std::nullopt);
  EXPECT_TRUE(result);

  allocation::ImageMetadata metadata = {.collection_id = collection_id,
                                        .identifier = image_id,
                                        .vmo_index = 0,
                                        .width = kWidth,
                                        .height = kHeight};

  // Importing an image should fail at this point because we've only set the renderer constraints.
  auto import_result =
      renderer.ImportBufferImage(metadata, allocation::BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(import_result);

  // Set the display constraints on the display coordinator.
  fuchsia::hardware::display::ImageConfig display_constraints;
  bool res = scenic_impl::ImportBufferCollection(collection_id, *display_coordinator.get(),
                                                 std::move(display_token), display_constraints);
  ASSERT_TRUE(res);
  auto release_buffer_collection =
      fit::defer([display_coordinator = display_coordinator.get(), collection_id] {
        // Release the buffer collection.
        zx_status_t release_buffer_collection_status =
            (*display_coordinator)->ReleaseBufferCollection(collection_id);
        EXPECT_EQ(release_buffer_collection_status, ZX_OK);
      });

  // Importing should fail again, because we've only set 2 of the 3 constraints.
  import_result =
      renderer.ImportBufferImage(metadata, allocation::BufferCollectionUsage::kClientImage);
  EXPECT_FALSE(import_result);

  // Create a client-side handle to the buffer collection and set the client constraints.
  auto client_collection = flatland::CreateBufferCollectionSyncPtrAndSetConstraints(
      sysmem_allocator_.get(), std::move(tokens.local_token),
      /*image_count*/ 1,
      /*width*/ kWidth,
      /*height*/ kHeight,
      /*usage*/ flatland::kNoneUsage, fuchsia::sysmem::PixelFormatType::BGRA32,
      /*memory_constraints*/ std::nullopt,
      std::make_optional(fuchsia::sysmem::FORMAT_MODIFIER_LINEAR));

  // Have the client wait for buffers allocated so it can populate its information
  // struct with the vmo data.
  fuchsia::sysmem::BufferCollectionInfo_2 client_collection_info = {};
  {
    zx_status_t allocation_status = ZX_OK;
    auto status =
        client_collection->WaitForBuffersAllocated(&allocation_status, &client_collection_info);
    EXPECT_EQ(status, ZX_OK);
    EXPECT_EQ(allocation_status, ZX_OK);
  }

  // Now that the renderer, client, and the display have set their constraints, we import one last
  // time and this time it should return true.
  import_result =
      renderer.ImportBufferImage(metadata, allocation::BufferCollectionUsage::kClientImage);
  EXPECT_TRUE(import_result);

  // We should now be able to also import an image to the display coordinator, using the
  // display-specific buffer collection id. If it returns OK, then we know that the renderer
  // did fully set the DC constraints.
  fuchsia::hardware::display::ImageConfig image_config{};

  // Try to import the image into the display coordinator API and make sure it succeeds.
  uint64_t display_image_id = allocation::GenerateUniqueImageId();
  zx_status_t import_image_status = ZX_OK;
  (*display_coordinator.get())
      ->ImportImage(image_config, collection_id, display_image_id, /*vmo_index*/ 0,
                    &import_image_status);
  EXPECT_EQ(import_image_status, ZX_OK);
}

// Test out event signaling on the Display Coordinator by importing a buffer collection and its 2
// images, setting the first image to a display layer with a signal event, and
// then setting the second image on the layer which has a wait event. When the wait event is
// signaled, this will cause the second layer image to go up, which in turn will cause the first
// layer image's event to be signaled.
// TODO(fxbug.dev/55167): Check to see if there is a more appropriate place to test display
// coordinator events and/or if there already exist adequate tests that cover all of the use cases
// being covered by this test.
VK_TEST_F(DisplayTest, SetDisplayImageTest) {
  // Grab the display coordinator.
  auto display_coordinator = display_manager_->default_display_coordinator();
  ASSERT_TRUE(display_coordinator);

  auto display = display_manager_->default_display();
  ASSERT_TRUE(display);

  auto layer_id = InitializeDisplayLayer(*display_coordinator.get(), display);
  ASSERT_NE(layer_id, 0U);

  const uint32_t kWidth = display->width_in_px();
  const uint32_t kHeight = display->height_in_px();
  const uint32_t kNumVmos = 2;

  // First create the pair of sysmem tokens, one for the client, one for the display.
  auto tokens = flatland::SysmemTokens::Create(sysmem_allocator_.get());

  // Set the display constraints on the display coordinator.
  fuchsia::hardware::display::ImageConfig image_config = {
      .width = kWidth,
      .height = kHeight,
  };
  auto display_collection_id = allocation::GenerateUniqueBufferCollectionId();
  ASSERT_NE(display_collection_id, 0U);

  bool res = scenic_impl::ImportBufferCollection(display_collection_id, *display_coordinator.get(),
                                                 std::move(tokens.dup_token), image_config);
  ASSERT_TRUE(res);

  flatland::SetClientConstraintsAndWaitForAllocated(
      sysmem_allocator_.get(), std::move(tokens.local_token), kNumVmos, kWidth, kHeight);

  // Import the images to the display.
  uint64_t image_ids[kNumVmos];
  for (uint32_t i = 0; i < kNumVmos; i++) {
    image_ids[i] = allocation::GenerateUniqueImageId();
    zx_status_t import_image_status = ZX_OK;
    auto transport_status = (*display_coordinator.get())
                                ->ImportImage(image_config, display_collection_id, image_ids[i], i,
                                              &import_image_status);
    ASSERT_EQ(transport_status, ZX_OK);
    ASSERT_EQ(import_image_status, ZX_OK);
    ASSERT_NE(image_ids[i], fuchsia::hardware::display::INVALID_DISP_ID);
  }

  // It is safe to release buffer collection because we are not going to import any more images.
  (*display_coordinator.get())->ReleaseBufferCollection(display_collection_id);

  // Create the events used by the display.
  zx::event display_wait_fence, display_signal_fence;
  auto status = zx::event::create(0, &display_wait_fence);
  status |= zx::event::create(0, &display_signal_fence);
  EXPECT_EQ(status, ZX_OK);

  // Import the above events to the display.
  auto display_wait_event_id =
      scenic_impl::ImportEvent(*display_coordinator.get(), display_wait_fence);
  auto display_signal_event_id =
      scenic_impl::ImportEvent(*display_coordinator.get(), display_signal_fence);
  EXPECT_NE(display_wait_event_id, fuchsia::hardware::display::INVALID_DISP_ID);
  EXPECT_NE(display_signal_event_id, fuchsia::hardware::display::INVALID_DISP_ID);
  EXPECT_NE(display_wait_event_id, display_signal_event_id);

  // Set the layer image and apply the config.
  (*display_coordinator.get())->SetLayerPrimaryConfig(layer_id, image_config);

  status = (*display_coordinator.get())
               ->SetLayerImage(layer_id, image_ids[0], 0, display_signal_event_id);
  EXPECT_EQ(status, ZX_OK);

  // Apply the config.
  fuchsia::hardware::display::ConfigResult result;
  std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
  (*display_coordinator.get())->CheckConfig(/*discard=*/false, &result, &ops);
  EXPECT_EQ(result, fuchsia::hardware::display::ConfigResult::OK);
  status = (*display_coordinator.get())->ApplyConfig();
  EXPECT_EQ(status, ZX_OK);

  // Attempt to wait here...this should time out because the event has not yet been signaled.
  status =
      display_signal_fence.wait_one(ZX_EVENT_SIGNALED, zx::deadline_after(zx::msec(3000)), nullptr);
  EXPECT_EQ(status, ZX_ERR_TIMED_OUT);

  // Set the layer image again, to the second image, so that our first call to SetLayerImage()
  // above will signal.
  status =
      (*display_coordinator.get())->SetLayerImage(layer_id, image_ids[1], display_wait_event_id, 0);
  EXPECT_EQ(status, ZX_OK);

  // Apply the config to display the second image.
  (*display_coordinator.get())->CheckConfig(/*discard=*/false, &result, &ops);
  EXPECT_EQ(result, fuchsia::hardware::display::ConfigResult::OK);
  status = (*display_coordinator.get())->ApplyConfig();
  EXPECT_EQ(status, ZX_OK);

  // Attempt to wait again, this should also time out because we haven't signaled our wait fence.
  status =
      display_signal_fence.wait_one(ZX_EVENT_SIGNALED, zx::deadline_after(zx::msec(3000)), nullptr);
  EXPECT_EQ(status, ZX_ERR_TIMED_OUT);

  // Now we signal wait on the second layer.
  display_wait_fence.signal(0, ZX_EVENT_SIGNALED);

  // Now we wait for the display to signal again, and this time it should go through.
  status =
      display_signal_fence.wait_one(ZX_EVENT_SIGNALED, zx::deadline_after(zx::msec(3000)), nullptr);
  EXPECT_EQ(status, ZX_OK);
}

}  // namespace
