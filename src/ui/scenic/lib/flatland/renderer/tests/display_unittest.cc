// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fuchsia/hardware/display/cpp/fidl.h>
#include <lib/async-testing/test_loop.h>
#include <lib/async/cpp/executor.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/default.h>
#include <lib/fdio/directory.h>
#include <lib/fit/defer.h>

#include <thread>

#include "src/graphics/display/lib/coordinator-getter/client.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/ui/lib/escher/test/common/gtest_escher.h"
#include "src/ui/lib/escher/test/common/gtest_vulkan.h"
#include "src/ui/lib/escher/vk/vulkan_device_queues.h"
#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"
#include "src/ui/scenic/lib/allocation/id.h"
#include "src/ui/scenic/lib/display/display_manager.h"
#include "src/ui/scenic/lib/display/util.h"
#include "src/ui/scenic/lib/flatland/buffers/util.h"
#include "src/ui/scenic/lib/flatland/renderer/null_renderer.h"
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

    // TODO(https://fxbug.dev/42073120): This reuses the display coordinator from previous
    // test cases in the same test component, so the display coordinator may be
    // in a dirty state. Tests should request a reset of display coordinator
    // here.
    auto hdc_promise = display::GetCoordinator();
    executor_->schedule_task(hdc_promise.then(
        [this](fpromise::result<display::CoordinatorClientEnd, zx_status_t>& handles) {
          ASSERT_TRUE(handles.is_ok())
              << "Failed to get display coordinator:" << zx_status_get_string(handles.error());
          display_manager_->BindDefaultDisplayCoordinator(std::move(handles.value()));
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

  fuchsia::hardware::display::LayerId InitializeDisplayLayer(
      fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator,
      scenic_impl::display::Display* display) {
    fuchsia::hardware::display::Coordinator_CreateLayer_Result result;
    const zx_status_t transport_status = display_coordinator->CreateLayer(&result);
    if (transport_status != ZX_OK) {
      FX_LOGS(ERROR) << "Failed to call FIDL CreateLayer: "
                     << zx_status_get_string(transport_status);
      return {.value = fuchsia::hardware::display::types::INVALID_DISP_ID};
    }
    if (result.is_err()) {
      FX_LOGS(ERROR) << "Failed to create layer: " << zx_status_get_string(result.err());
      return {.value = fuchsia::hardware::display::types::INVALID_DISP_ID};
    }
    const fuchsia::hardware::display::LayerId layer_id = result.response().layer_id;

    const zx_status_t set_display_layers_status =
        display_coordinator->SetDisplayLayers(display->display_id(), {layer_id});
    if (set_display_layers_status != ZX_OK) {
      FX_LOGS(ERROR) << "Failed to configure display layers: "
                     << zx_status_get_string(set_display_layers_status);
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
  const auto collection_id = allocation::GenerateUniqueBufferCollectionId();
  const fuchsia::hardware::display::BufferCollectionId display_collection_id =
      allocation::ToDisplayBufferCollectionId(collection_id);
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
  fuchsia::hardware::display::types::ImageBufferUsage image_buffer_usage = {
      .tiling_type = fuchsia::hardware::display::types::IMAGE_TILING_TYPE_LINEAR,
  };
  bool res = scenic_impl::ImportBufferCollection(collection_id, *display_coordinator.get(),
                                                 std::move(display_token), image_buffer_usage);
  ASSERT_TRUE(res);
  auto release_buffer_collection =
      fit::defer([display_coordinator = display_coordinator.get(), display_collection_id] {
        // Release the buffer collection.
        zx_status_t release_buffer_collection_status =
            (*display_coordinator)->ReleaseBufferCollection(display_collection_id);
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
  fuchsia::hardware::display::types::ImageMetadata image_metadata = {
      .width = kWidth,
      .height = kHeight,
      .tiling_type = fuchsia::hardware::display::types::IMAGE_TILING_TYPE_LINEAR,
  };

  // Try to import the image into the display coordinator API and make sure it succeeds.
  allocation::GlobalImageId display_image_id = allocation::GenerateUniqueImageId();
  fuchsia::hardware::display::Coordinator_ImportImage_Result import_image_result;
  (*display_coordinator.get())
      ->ImportImage(image_metadata, /*buffer_id=*/
                    {
                        .buffer_collection_id = display_collection_id,
                        .buffer_index = 0,
                    },
                    allocation::ToFidlImageId(display_image_id), &import_image_result);
  EXPECT_TRUE(import_image_result.is_response());
}

// Test out event signaling on the Display Coordinator by importing a buffer collection and its 2
// images, setting the first image to a display layer with a signal event, and
// then setting the second image on the layer which has a wait event. When the wait event is
// signaled, this will cause the second layer image to go up, which in turn will cause the first
// layer image's event to be signaled.
// TODO(https://fxbug.dev/42132767): Check to see if there is a more appropriate place to test
// display coordinator events and/or if there already exist adequate tests that cover all of the use
// cases being covered by this test.
VK_TEST_F(DisplayTest, SetDisplayImageTest) {
  // Grab the display coordinator.
  auto display_coordinator = display_manager_->default_display_coordinator();
  ASSERT_TRUE(display_coordinator);

  auto display = display_manager_->default_display();
  ASSERT_TRUE(display);

  fuchsia::hardware::display::LayerId layer_id =
      InitializeDisplayLayer(*display_coordinator.get(), display);
  ASSERT_NE(layer_id.value, fuchsia::hardware::display::types::INVALID_DISP_ID);

  const uint32_t kWidth = display->width_in_px();
  const uint32_t kHeight = display->height_in_px();
  const uint32_t kNumVmos = 2;

  // First create the pair of sysmem tokens, one for the client, one for the display.
  auto tokens = flatland::SysmemTokens::Create(sysmem_allocator_.get());

  // Set the display constraints on the display coordinator.
  fuchsia::hardware::display::types::ImageBufferUsage image_buffer_usage = {
      .tiling_type = fuchsia::hardware::display::types::IMAGE_TILING_TYPE_LINEAR,
  };
  auto global_collection_id = allocation::GenerateUniqueBufferCollectionId();
  ASSERT_NE(global_collection_id, ZX_KOID_INVALID);
  const fuchsia::hardware::display::BufferCollectionId display_collection_id =
      allocation::ToDisplayBufferCollectionId(global_collection_id);

  bool res = scenic_impl::ImportBufferCollection(global_collection_id, *display_coordinator.get(),
                                                 std::move(tokens.dup_token), image_buffer_usage);
  ASSERT_TRUE(res);

  flatland::SetClientConstraintsAndWaitForAllocated(
      sysmem_allocator_.get(), std::move(tokens.local_token), kNumVmos, kWidth, kHeight);

  // Import the images to the display.
  fuchsia::hardware::display::types::ImageMetadata image_metadata = {
      .width = kWidth,
      .height = kHeight,
      .tiling_type = fuchsia::hardware::display::types::IMAGE_TILING_TYPE_LINEAR,
  };
  allocation::GlobalImageId image_ids[kNumVmos];
  for (uint32_t i = 0; i < kNumVmos; i++) {
    image_ids[i] = allocation::GenerateUniqueImageId();
    fuchsia::hardware::display::Coordinator_ImportImage_Result import_image_result;
    auto transport_status =
        (*display_coordinator.get())
            ->ImportImage(image_metadata, /*buffer_id=*/
                          {
                              .buffer_collection_id = display_collection_id,
                              .buffer_index = i,
                          },
                          allocation::ToFidlImageId(image_ids[i]), &import_image_result);
    ASSERT_EQ(transport_status, ZX_OK);
    ASSERT_TRUE(import_image_result.is_response());
    ASSERT_NE(image_ids[i], fuchsia::hardware::display::types::INVALID_DISP_ID);
  }

  // It is safe to release buffer collection because we are not going to import any more images.
  (*display_coordinator.get())->ReleaseBufferCollection(display_collection_id);

  // Create the events used by the display.
  zx::event display_wait_fence, display_signal_fence;
  auto status = zx::event::create(0, &display_wait_fence);
  status |= zx::event::create(0, &display_signal_fence);
  EXPECT_EQ(status, ZX_OK);

  // Import the above events to the display.
  scenic_impl::DisplayEventId display_wait_event_id =
      scenic_impl::ImportEvent(*display_coordinator.get(), display_wait_fence);
  scenic_impl::DisplayEventId display_signal_event_id =
      scenic_impl::ImportEvent(*display_coordinator.get(), display_signal_fence);
  EXPECT_NE(display_wait_event_id.value, fuchsia::hardware::display::types::INVALID_DISP_ID);
  EXPECT_NE(display_signal_event_id.value, fuchsia::hardware::display::types::INVALID_DISP_ID);
  EXPECT_NE(display_wait_event_id.value, display_signal_event_id.value);

  // Set the layer image and apply the config.
  (*display_coordinator.get())->SetLayerPrimaryConfig(layer_id, image_metadata);

  static constexpr scenic_impl::DisplayEventId kInvalidEventId = {
      .value = fuchsia::hardware::display::types::INVALID_DISP_ID};
  status = (*display_coordinator.get())
               ->SetLayerImage(layer_id, allocation::ToFidlImageId(image_ids[0]),
                               /*wait_event_id=*/kInvalidEventId, display_signal_event_id);
  EXPECT_EQ(status, ZX_OK);

  // Apply the config.
  fuchsia::hardware::display::types::ConfigResult result;
  std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
  (*display_coordinator.get())->CheckConfig(/*discard=*/false, &result, &ops);
  EXPECT_EQ(result, fuchsia::hardware::display::types::ConfigResult::OK);
  status = (*display_coordinator.get())->ApplyConfig();
  EXPECT_EQ(status, ZX_OK);

  // Attempt to wait here...this should time out because the event has not yet been signaled.
  status =
      display_signal_fence.wait_one(ZX_EVENT_SIGNALED, zx::deadline_after(zx::msec(3000)), nullptr);
  EXPECT_EQ(status, ZX_ERR_TIMED_OUT);

  // Set the layer image again, to the second image, so that our first call to SetLayerImage()
  // above will signal.
  status =
      (*display_coordinator.get())
          ->SetLayerImage(layer_id, allocation::ToFidlImageId(image_ids[1]), display_wait_event_id,
                          /*signal_event_id=*/kInvalidEventId);
  EXPECT_EQ(status, ZX_OK);

  // Apply the config to display the second image.
  (*display_coordinator.get())->CheckConfig(/*discard=*/false, &result, &ops);
  EXPECT_EQ(result, fuchsia::hardware::display::types::ConfigResult::OK);
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
