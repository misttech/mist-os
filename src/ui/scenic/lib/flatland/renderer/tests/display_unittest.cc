// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.math/cpp/fidl.h>
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
        [this](fpromise::result<display::CoordinatorClientChannels, zx_status_t>& client_channels) {
          ASSERT_TRUE(client_channels.is_ok()) << "Failed to get display coordinator:"
                                               << zx_status_get_string(client_channels.error());
          auto [coordinator_client, listener_server] = std::move(client_channels.value());
          display_manager_->BindDefaultDisplayCoordinator(std::move(coordinator_client),
                                                          std::move(listener_server));
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

  fuchsia_hardware_display::LayerId InitializeDisplayLayer(
      const fidl::SyncClient<fuchsia_hardware_display::Coordinator>& display_coordinator,
      scenic_impl::display::Display* display) {
    const fidl::Result create_layer_result = display_coordinator->CreateLayer();
    if (!create_layer_result.is_ok()) {
      FX_LOGS(ERROR) << "Failed to call FIDL CreateLayer: " << create_layer_result.error_value();
      return {{.value = fuchsia_hardware_display_types::kInvalidDispId}};
    }
    const fuchsia_hardware_display::LayerId layer_id = create_layer_result.value().layer_id();

    const fit::result<fidl::OneWayStatus> set_display_layers_result =
        display_coordinator->SetDisplayLayers({{
            .display_id = display->display_id(),
            .layer_ids = {layer_id},
        }});
    if (!set_display_layers_result.is_ok()) {
      FX_LOGS(ERROR) << "Failed to call FIDL SetDisplayLayers: "
                     << set_display_layers_result.error_value();
      return {{.value = fuchsia_hardware_display_types::kInvalidDispId}};
    }
    return layer_id;
  }

  // Wait until a vsync is received with a stamp that is >= `target_stamp`.  Return ZX_ERR_TIMED_OUT
  // if no such vsync is received before `timeout` elapses.
  zx::result<> WaitForVsync(fuchsia_hardware_display::ConfigStamp target_stamp,
                            zx::duration timeout) {
    std::optional<fuchsia_hardware_display::ConfigStamp> received_stamp;
    display_manager_->default_display()->SetVsyncCallback(
        [&](zx::time, fuchsia_hardware_display::ConfigStamp applied_config_stamp) {
          received_stamp = applied_config_stamp;
        });

    bool success = RunLoopWithTimeoutOrUntil(
        [&]() { return received_stamp && received_stamp.value().value() >= target_stamp.value(); },
        timeout);

    display_manager_->default_display()->SetVsyncCallback(nullptr);

    if (success) {
      return zx::ok();
    }
    return zx::error(ZX_ERR_TIMED_OUT);
  }

  std::unique_ptr<async::Executor> executor_;
  std::unique_ptr<scenic_impl::display::DisplayManager> display_manager_;
  fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator_;
};

// Create a buffer collection and set constraints on the display, the vulkan renderer
// and the client, and make sure that the collection is still properly allocated.
VK_TEST_F(DisplayTest, SetAllConstraintsTest) {
  const uint64_t kWidth = 8;
  const uint64_t kHeight = 16;

  // Grab the display coordinator.
  std::shared_ptr<fidl::SyncClient<fuchsia_hardware_display::Coordinator>> display_coordinator =
      display_manager_->default_display_coordinator();
  EXPECT_TRUE(display_coordinator);

  // Create the VK renderer.
  auto env = escher::test::EscherEnvironment::GetGlobalTestEnvironment();
  auto unique_escher = std::make_unique<escher::Escher>(
      env->GetVulkanDevice(), env->GetFilesystem(), /*gpu_allocator*/ nullptr);
  flatland::VkRenderer renderer(unique_escher->GetWeakPtr());

  // First create the pair of sysmem tokens, one for the client, one for the renderer.
  auto tokens = flatland::SysmemTokens::Create(sysmem_allocator_.get());

  fuchsia::sysmem2::BufferCollectionTokenSyncPtr display_token;
  fuchsia::sysmem2::BufferCollectionTokenDuplicateRequest dup_request;
  dup_request.set_rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS);
  dup_request.set_token_request(display_token.NewRequest());
  zx_status_t status = tokens.local_token->Duplicate(std::move(dup_request));
  FX_DCHECK(status == ZX_OK);

  // Register the collection with the renderer, which sets the vk constraints.
  const auto collection_id = allocation::GenerateUniqueBufferCollectionId();
  const fuchsia_hardware_display::BufferCollectionId display_collection_id =
      scenic_impl::ToDisplayFidlBufferCollectionId(collection_id);
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
  fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> natural_token(
      std::move(display_token).Unbind().TakeChannel());
  fuchsia_hardware_display_types::ImageBufferUsage image_buffer_usage = {{
      .tiling_type = fuchsia_hardware_display_types::kImageTilingTypeLinear,
  }};
  bool res = scenic_impl::ImportBufferCollection(collection_id, *display_coordinator,
                                                 std::move(natural_token), image_buffer_usage);
  ASSERT_TRUE(res);
  auto release_buffer_collection = fit::defer([display_coordinator, display_collection_id] {
    // Release the buffer collection.
    const fit::result<fidl::OneWayStatus> release_buffer_collection_result =
        (*display_coordinator)
            ->ReleaseBufferCollection({{.buffer_collection_id = display_collection_id}});
    EXPECT_TRUE(release_buffer_collection_result.is_ok())
        << "Failed to call FIDL ReleaseBufferCollection: "
        << release_buffer_collection_result.error_value();
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
      /*usage*/ fidl::Clone(flatland::get_none_usage()), fuchsia::images2::PixelFormat::B8G8R8A8,
      /*memory_constraints*/ std::nullopt,
      std::make_optional(fuchsia::images2::PixelFormatModifier::LINEAR));

  // Have the client wait for buffers allocated so it can populate its information
  // struct with the vmo data.
  fuchsia::sysmem2::BufferCollectionInfo client_collection_info;
  {
    fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
    auto status = client_collection->WaitForAllBuffersAllocated(&wait_result);
    EXPECT_EQ(status, ZX_OK);
    EXPECT_TRUE(!wait_result.is_framework_err());
    EXPECT_TRUE(!wait_result.is_err());
    EXPECT_TRUE(wait_result.is_response());
    client_collection_info = std::move(*wait_result.response().mutable_buffer_collection_info());
  }

  // Now that the renderer, client, and the display have set their constraints, we import one last
  // time and this time it should return true.
  import_result =
      renderer.ImportBufferImage(metadata, allocation::BufferCollectionUsage::kClientImage);
  EXPECT_TRUE(import_result);

  // We should now be able to also import an image to the display coordinator, using the
  // display-specific buffer collection id. If it returns OK, then we know that the renderer
  // did fully set the DC constraints.
  fuchsia_hardware_display_types::ImageMetadata image_metadata({
      .dimensions = fuchsia_math::SizeU({.width = kWidth, .height = kHeight}),
      .tiling_type = fuchsia_hardware_display_types::kImageTilingTypeLinear,
  });

  // Try to import the image into the display coordinator API and make sure it succeeds.
  allocation::GlobalImageId display_image_id = allocation::GenerateUniqueImageId();

  const fidl::Result import_image_result =
      (*display_coordinator)
          ->ImportImage({{
              .image_metadata = image_metadata,
              .buffer_id = {{
                  .buffer_collection_id = display_collection_id,
                  .buffer_index = 0,
              }},
              .image_id = scenic_impl::ToDisplayFidlImageId(display_image_id),
          }});
  EXPECT_TRUE(import_image_result.is_ok())
      << "Failed to call FIDL ImportImage: " << import_image_result.error_value();
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
  std::shared_ptr<fidl::SyncClient<fuchsia_hardware_display::Coordinator>> display_coordinator =
      display_manager_->default_display_coordinator();
  ASSERT_TRUE(display_coordinator);

  auto display = display_manager_->default_display();
  ASSERT_TRUE(display);

  fuchsia_hardware_display::LayerId layer_id =
      InitializeDisplayLayer(*display_coordinator, display);
  ASSERT_NE(layer_id.value(), fuchsia_hardware_display_types::kInvalidDispId);

  const uint32_t kWidth = display->width_in_px();
  const uint32_t kHeight = display->height_in_px();
  const uint32_t kNumVmos = 2;

  // First create the pair of sysmem tokens, one for the client, one for the display.
  auto tokens = flatland::SysmemTokens::Create(sysmem_allocator_.get());

  // Set the display constraints on the display coordinator.
  fuchsia_hardware_display_types::ImageBufferUsage image_buffer_usage = {{
      .tiling_type = fuchsia_hardware_display_types::kImageTilingTypeLinear,
  }};
  auto global_collection_id = allocation::GenerateUniqueBufferCollectionId();
  ASSERT_NE(global_collection_id, ZX_KOID_INVALID);
  const fuchsia_hardware_display::BufferCollectionId display_collection_id =
      scenic_impl::ToDisplayFidlBufferCollectionId(global_collection_id);

  fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> dup_token(
      std::move(tokens.dup_token).Unbind().TakeChannel());
  bool res = scenic_impl::ImportBufferCollection(global_collection_id, *display_coordinator,
                                                 std::move(dup_token), image_buffer_usage);
  ASSERT_TRUE(res);

  flatland::SetClientConstraintsAndWaitForAllocated(
      sysmem_allocator_.get(), std::move(tokens.local_token), kNumVmos, kWidth, kHeight);

  // Import the images to the display.
  fuchsia_hardware_display_types::ImageMetadata image_metadata({
      .dimensions = fuchsia_math::SizeU({.width = kWidth, .height = kHeight}),
      .tiling_type = fuchsia_hardware_display_types::kImageTilingTypeLinear,
  });
  allocation::GlobalImageId image_ids[kNumVmos];
  for (uint32_t i = 0; i < kNumVmos; i++) {
    image_ids[i] = allocation::GenerateUniqueImageId();
    const fidl::Result import_image_result =
        (*display_coordinator)
            ->ImportImage({{
                .image_metadata = image_metadata,
                .buffer_id = {{
                    .buffer_collection_id = display_collection_id,
                    .buffer_index = i,
                }},
                .image_id = scenic_impl::ToDisplayFidlImageId(image_ids[i]),
            }});
    ASSERT_TRUE(import_image_result.is_ok())
        << "Failed to call FIDL ImportImage: " << import_image_result.error_value();
    ASSERT_NE(image_ids[i], fuchsia_hardware_display_types::kInvalidDispId);
  }

  // It is safe to release buffer collection because we are not going to import any more images.
  const fit::result<fidl::OneWayStatus> release_result =
      (*display_coordinator)
          ->ReleaseBufferCollection({{.buffer_collection_id = display_collection_id}});
  EXPECT_TRUE(release_result.is_ok())
      << "Failed to call FIDL ReleaseBufferCollection: " << release_result.error_value();

  // Create the events used by the display.
  zx::event display_wait_fence;
  auto status = zx::event::create(0, &display_wait_fence);
  EXPECT_EQ(status, ZX_OK);

  // Import the above events to the display.
  scenic_impl::DisplayEventId display_wait_event_id =
      scenic_impl::ImportEvent(*display_coordinator, display_wait_fence);
  EXPECT_NE(display_wait_event_id.value(), fuchsia_hardware_display_types::kInvalidDispId);

  // Set the layer image and apply the config.
  const fit::result<fidl::OneWayStatus> set_layer_primary_config_result =
      (*display_coordinator)
          ->SetLayerPrimaryConfig({{
              .layer_id = layer_id,
              .image_metadata = image_metadata,
          }});
  EXPECT_TRUE(set_layer_primary_config_result.is_ok())
      << "Failed to call FIDL SetLayerPrimaryConfig: "
      << set_layer_primary_config_result.error_value();

  static const scenic_impl::DisplayEventId kInvalidEventId = {
      {.value = fuchsia_hardware_display_types::kInvalidDispId}};
  const fit::result<fidl::OneWayStatus> set_layer_image_result =
      (*display_coordinator)
          ->SetLayerImage2({{
              .layer_id = layer_id,
              .image_id = scenic_impl::ToDisplayFidlImageId(image_ids[0]),
              .wait_event_id = scenic_impl::DisplayEventId(kInvalidEventId),
          }});
  EXPECT_TRUE(set_layer_image_result.is_ok())
      << "Failed to call FIDL SetLayerImage2: " << set_layer_image_result.error_value();

  // Apply the config.
  const fidl::Result check_config_result = (*display_coordinator)
                                               ->CheckConfig({{
                                                   .discard = false,
                                               }});
  EXPECT_TRUE(check_config_result.is_ok())
      << "Failed to call FIDL CheckConfig: " << check_config_result.error_value();

  const fuchsia_hardware_display::ConfigStamp kFirstConfigStamp(11);
  {
    fuchsia_hardware_display::CoordinatorApplyConfig3Request request;
    request.stamp(kFirstConfigStamp);

    const fit::result<fidl::OneWayStatus> result =
        (*display_coordinator)->ApplyConfig3(std::move(request));
    EXPECT_TRUE(result.is_ok()) << "Failed to call FIDL ApplyConfig3: " << result.error_value();
  }

  // Wait for the first Vsync.  This should arrive because there is no wait fence to block
  // application of the config.
  zx::result vsync_result = WaitForVsync(kFirstConfigStamp, zx::msec(3000));
  EXPECT_TRUE(vsync_result.is_ok())
      << "first WaitForVsync() failed with status: " << vsync_result.status_string();

  // Set the layer image again, to the second image, so that our first call to SetLayerImage2()
  // above will signal.
  const fit::result<fidl::OneWayStatus> set_layer_image_result2 =
      (*display_coordinator)
          ->SetLayerImage2({{
              .layer_id = layer_id,
              .image_id = scenic_impl::ToDisplayFidlImageId(image_ids[1]),
              .wait_event_id = display_wait_event_id,
          }});
  EXPECT_TRUE(set_layer_image_result2.is_ok())
      << "Failed to call FIDL SetLayerImage2: " << set_layer_image_result2.error_value();

  // Apply the config to display the second image.
  const fidl::Result check_config_result2 =
      (*display_coordinator)->CheckConfig({{.discard = false}});
  EXPECT_TRUE(check_config_result2.is_ok())
      << "Failed to call FIDL CheckConfig: " << check_config_result2.error_value();
  EXPECT_EQ(check_config_result2.value().res(), fuchsia_hardware_display_types::ConfigResult::kOk);

  const fuchsia_hardware_display::ConfigStamp kSecondConfigStamp(22);
  {
    fuchsia_hardware_display::CoordinatorApplyConfig3Request request;
    request.stamp(kSecondConfigStamp);

    const fit::result<fidl::OneWayStatus> result =
        (*display_coordinator)->ApplyConfig3(std::move(request));
    EXPECT_TRUE(result.is_ok()) << "Failed to call FIDL ApplyConfig3: " << result.error_value();
  }

  // Wait for the second Vsync.  This won't come, because the display coordinator will block on the
  // wait fence.
  vsync_result = WaitForVsync(kSecondConfigStamp, zx::msec(3000));
  EXPECT_TRUE(vsync_result.is_error() && vsync_result.status_value() == ZX_ERR_TIMED_OUT)
      << "second WaitForVsync() unexpected status: " << vsync_result.status_string();

  // Now we signal wait on the second layer.
  display_wait_fence.signal(0, ZX_EVENT_SIGNALED);

  // Now we that the wait fence has been signaled, we should receive a vsync corresponding to the
  // second config.
  vsync_result = WaitForVsync(kSecondConfigStamp, zx::msec(3000));
  EXPECT_TRUE(vsync_result.is_ok())
      << "final WaitForVsync() failed with status: " << vsync_result.status_string();
}

}  // namespace
