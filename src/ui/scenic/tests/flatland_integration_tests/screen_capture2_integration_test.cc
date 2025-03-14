// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/sysmem/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl.h>
#include <fuchsia/ui/composition/internal/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_creation_tokens.h>
#include <lib/ui/scenic/cpp/view_identity.h>
#include <sys/types.h>
#include <zircon/status.h>

#include <cstdint>
#include <iostream>
#include <utility>

#include <zxtest/zxtest.h>

#include "src/ui/scenic/lib/allocation/buffer_collection_import_export_tokens.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/tests/utils/blocking_present.h"
#include "src/ui/scenic/tests/utils/scenic_ctf_test_base.h"
#include "src/ui/scenic/tests/utils/screen_capture_utils.h"

namespace integration_tests {

using fuchsia::ui::composition::ChildViewWatcher;
using fuchsia::ui::composition::ContentId;
using fuchsia::ui::composition::Flatland;
using fuchsia::ui::composition::FlatlandDisplay;
using fuchsia::ui::composition::ParentViewportWatcher;
using fuchsia::ui::composition::RegisterBufferCollectionUsages;
using fuchsia::ui::composition::TransformId;
using fuchsia::ui::composition::ViewportProperties;
using fuchsia::ui::composition::internal::FrameInfo;
using fuchsia::ui::composition::internal::ScreenCapture;
using fuchsia::ui::composition::internal::ScreenCaptureConfig;
using fuchsia::ui::composition::internal::ScreenCaptureError;

class ScreenCapture2IntegrationTest : public ScenicCtfTest {
 public:
  void SetUp() override {
    ScenicCtfTest::SetUp();

    LocalServiceDirectory()->Connect(sysmem_allocator_.NewRequest());

    flatland_display_ = ConnectSyncIntoRealm<fuchsia::ui::composition::FlatlandDisplay>();
    flatland_allocator_ = ConnectSyncIntoRealm<fuchsia::ui::composition::Allocator>();
    root_session_ = ConnectAsyncIntoRealm<fuchsia::ui::composition::Flatland>();

    fidl::InterfacePtr<ChildViewWatcher> child_view_watcher;
    fidl::InterfacePtr<ParentViewportWatcher> parent_viewport_watcher;
    {
      auto [child_token, parent_token] = scenic::ViewCreationTokenPair::New();
      flatland_display_->SetContent(std::move(parent_token), child_view_watcher.NewRequest());

      auto identity = scenic::NewViewIdentityOnCreation();
      root_view_ref_ = fidl::Clone(identity.view_ref);
      root_session_->CreateView2(std::move(child_token), std::move(identity), {},
                                 parent_viewport_watcher.NewRequest());
      parent_viewport_watcher->GetLayout([this](auto layout_info) {
        ASSERT_TRUE(layout_info.has_logical_size());
        const auto [width, height] = layout_info.logical_size();
        display_width_ = width;
        display_height_ = height;
        num_pixels_ = display_width_ * display_height_;
      });
    }
    BlockingPresent(this, root_session_);

    // Wait until we get the display size.
    RunLoopUntil([this] { return display_width_ != 0 && display_height_ != 0; });

    // Set up the root graph.
    fidl::InterfacePtr<ChildViewWatcher> child_view_watcher2;
    auto [child_token, parent_token] = scenic::ViewCreationTokenPair::New();
    ViewportProperties properties;
    properties.set_logical_size({display_width_, display_height_});
    const TransformId kRootTransform{.value = 1};
    const ContentId kRootContent{.value = 1};
    root_session_->CreateTransform(kRootTransform);
    root_session_->CreateViewport(kRootContent, std::move(parent_token), std::move(properties),
                                  child_view_watcher2.NewRequest());
    root_session_->SetRootTransform(kRootTransform);
    root_session_->SetContent(kRootTransform, kRootContent);
    BlockingPresent(this, root_session_);

    // Set up the child view.
    child_session_ = ConnectAsyncIntoRealm<fuchsia::ui::composition::Flatland>();
    fidl::InterfacePtr<ParentViewportWatcher> parent_viewport_watcher2;
    auto identity = scenic::NewViewIdentityOnCreation();
    auto child_view_ref = fidl::Clone(identity.view_ref);
    fuchsia::ui::composition::ViewBoundProtocols protocols;
    child_session_->CreateView2(std::move(child_token), std::move(identity), std::move(protocols),
                                parent_viewport_watcher2.NewRequest());
    child_session_->CreateTransform(kChildRootTransform);
    child_session_->SetRootTransform(kChildRootTransform);
    BlockingPresent(this, child_session_);

    // Create ScreenCapture client.
    screen_capture_ = ConnectAsyncIntoRealm<fuchsia::ui::composition::internal::ScreenCapture>();

    // Set up error handling.
    root_session_.events().OnError = [](fuchsia::ui::composition::FlatlandError error) {
      FX_LOGS(ERROR) << "Root session error: " << static_cast<int>(error);
    };
    child_session_.events().OnError = [](fuchsia::ui::composition::FlatlandError error) {
      FX_LOGS(ERROR) << "Child session error: " << static_cast<int>(error);
    };
  }

  fuchsia::sysmem2::BufferCollectionInfo ConfigureScreenCapture(
      fuchsia::sysmem2::BufferCollectionConstraints constraints, const uint32_t render_target_width,
      const uint32_t render_target_height) {
    // Create buffer collection to render into for GetNextFrame().
    allocation::BufferCollectionImportExportTokens scr_ref_pair =
        allocation::BufferCollectionImportExportTokens::New();

    fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info =
        CreateBufferCollectionInfoWithConstraints(
            std::move(constraints), std::move(scr_ref_pair.export_token), flatland_allocator_.get(),
            sysmem_allocator_.get(), RegisterBufferCollectionUsages::SCREENSHOT);

    // Configure ScreenCapture client.
    ScreenCaptureConfig sc_args;
    sc_args.set_import_token(std::move(scr_ref_pair.import_token));
    sc_args.set_image_size({render_target_width, render_target_height});

    fpromise::result<void, ScreenCaptureError> configure_result;
    bool alloc_result = false;
    screen_capture_->Configure(
        std::move(sc_args),
        [&configure_result, &alloc_result](fpromise::result<void, ScreenCaptureError> result) {
          EXPECT_FALSE(result.is_error());
          configure_result = std::move(result);
          alloc_result = true;
        });
    RunLoopWithTimeoutOrUntil([&alloc_result] { return alloc_result; }, kEventDelay);
    EXPECT_TRUE(configure_result.is_ok());

    return sc_buffer_collection_info;
  }

  const TransformId kChildRootTransform{.value = 1};
  static constexpr zx::duration kEventDelay = zx::msec(1000);

  fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator_;
  fuchsia::ui::composition::AllocatorSyncPtr flatland_allocator_;
  fuchsia::ui::composition::FlatlandDisplaySyncPtr flatland_display_;
  fuchsia::ui::composition::FlatlandPtr root_session_;
  fuchsia::ui::composition::FlatlandPtr child_session_;
  fuchsia::ui::composition::internal::ScreenCapturePtr screen_capture_;
  fuchsia::ui::views::ViewRef root_view_ref_;

  uint32_t display_width_ = 0;
  uint32_t display_height_ = 0;
  uint32_t num_pixels_ = 0;
};

TEST_F(ScreenCapture2IntegrationTest, SingleColorCapture) {
  const uint32_t image_width = display_width_;
  const uint32_t image_height = display_height_;
  const uint32_t render_target_width = display_width_;
  const uint32_t render_target_height = display_height_;

  // Create buffer collection for image to add to scene graph.
  allocation::BufferCollectionImportExportTokens ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, image_width, image_height),
          std::move(ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::DEFAULT);

  std::vector<uint8_t> write_values;
  for (uint32_t i = 0; i < num_pixels_; ++i) {
    write_values.insert(write_values.end(), kRed, kRed + kBytesPerPixel);
  }
  WriteToSysmemBuffer(write_values, buffer_collection_info, 0, kBytesPerPixel, image_width,
                      image_height);
  GenerateImageForFlatlandInstance(0, child_session_, kChildRootTransform,
                                   std::move(ref_pair.import_token), {image_width, image_height},
                                   {0, 0}, 2, 2);
  BlockingPresent(this, child_session_);

  fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info = ConfigureScreenCapture(
      utils::CreateDefaultConstraints(/*buffer_count=*/1, image_width, image_height),
      render_target_width, render_target_height);

  fpromise::result<FrameInfo, ScreenCaptureError> gnf_result;
  bool callback_result = false;
  screen_capture_->GetNextFrame(
      [&gnf_result, &callback_result](fpromise::result<FrameInfo, ScreenCaptureError> result) {
        EXPECT_FALSE(result.is_error());
        gnf_result = std::move(result);
        callback_result = true;
      });
  RunLoopWithTimeoutOrUntil([&callback_result] { return callback_result; }, kEventDelay);
  EXPECT_TRUE(gnf_result.is_ok());
  FrameInfo info = std::move(gnf_result.value());

  const auto& read_values =
      ExtractScreenCapture(info.buffer_index(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);

  EXPECT_EQ(read_values.size(), write_values.size());

  uint32_t num_red = 0;

  for (size_t i = 0; i < read_values.size(); i += kBytesPerPixel) {
    if (PixelEquals(&read_values[i], kRed))
      num_red++;
  }

  EXPECT_EQ(num_red, num_pixels_);
}

TEST_F(ScreenCapture2IntegrationTest, FilledRectCapture) {
  const uint32_t image_width = display_width_;
  const uint32_t image_height = display_height_;
  const uint32_t render_target_width = display_width_;
  const uint32_t render_target_height = display_height_;

  const ContentId kFilledRectId = {1};
  const TransformId kTransformId = {2};

  // Create a red rectangle.
  child_session_->CreateFilledRect(kFilledRectId);
  child_session_->SetSolidFill(kFilledRectId, {1, 0, 0, 1}, {image_width, image_height});
  child_session_->CreateTransform(kTransformId);
  child_session_->SetContent(kTransformId, kFilledRectId);

  // Attach the transform to the scene
  child_session_->AddChild(kChildRootTransform, kTransformId);
  BlockingPresent(this, child_session_);

  fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info = ConfigureScreenCapture(
      utils::CreateDefaultConstraints(/*buffer_count=*/1, image_width, image_height),
      render_target_width, render_target_height);

  fpromise::result<FrameInfo, ScreenCaptureError> gnf_result;
  bool callback_result = false;
  screen_capture_->GetNextFrame(
      [&gnf_result, &callback_result](fpromise::result<FrameInfo, ScreenCaptureError> result) {
        EXPECT_FALSE(result.is_error());
        gnf_result = std::move(result);
        callback_result = true;
      });
  RunLoopWithTimeoutOrUntil([&callback_result] { return callback_result; }, kEventDelay);
  EXPECT_TRUE(gnf_result.is_ok());
  FrameInfo info = std::move(gnf_result.value());

  const auto& read_values =
      ExtractScreenCapture(info.buffer_index(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);

  EXPECT_EQ(read_values.size(), num_pixels_ * kBytesPerPixel);

  uint32_t num_red = 0;
  for (size_t i = 0; i < read_values.size(); i += kBytesPerPixel) {
    if (PixelEquals(&read_values[i], kRed))
      num_red++;
  }

  EXPECT_EQ(num_red, num_pixels_);
}

// If the client calls GetNextFrame() and they have recieved the last frame, the client should hang
// until OnCpuWorkDone() is fired.
TEST_F(ScreenCapture2IntegrationTest, OnCpuWorkDoneCapture) {
  const uint32_t image_width = display_width_;
  const uint32_t image_height = display_height_;
  const uint32_t render_target_width = display_width_;
  const uint32_t render_target_height = display_height_;

  const ContentId kFilledRectId = {1};
  const TransformId kTransformId = {2};

  // Create a red rectangle.
  child_session_->CreateFilledRect(kFilledRectId);
  child_session_->SetSolidFill(kFilledRectId, {1, 0, 0, 1}, {image_width, image_height});
  child_session_->CreateTransform(kTransformId);
  child_session_->SetContent(kTransformId, kFilledRectId);

  // Attach the transform to the scene
  child_session_->AddChild(kChildRootTransform, kTransformId);
  BlockingPresent(this, child_session_);

  fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info = ConfigureScreenCapture(
      utils::CreateDefaultConstraints(/*buffer_count=*/1, image_width, image_height),
      render_target_width, render_target_height);

  fpromise::result<FrameInfo, ScreenCaptureError> gnf_result;
  bool callback_result = false;
  screen_capture_->GetNextFrame(
      [&gnf_result, &callback_result](fpromise::result<FrameInfo, ScreenCaptureError> result) {
        EXPECT_FALSE(result.is_error());
        gnf_result = std::move(result);
        callback_result = true;
      });
  RunLoopWithTimeoutOrUntil([&callback_result] { return callback_result; }, kEventDelay);
  EXPECT_TRUE(gnf_result.is_ok());
  FrameInfo info = std::move(gnf_result.value());

  const auto& read_values =
      ExtractScreenCapture(info.buffer_index(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);
  EXPECT_EQ(read_values.size(), num_pixels_ * kBytesPerPixel);

  // Compare read and write values.
  uint32_t num_red_count = 0;

  for (size_t i = 0; i < read_values.size(); i += kBytesPerPixel) {
    if (PixelEquals(&read_values[i], kRed))
      num_red_count++;
  }

  EXPECT_EQ(num_red_count, num_pixels_);

  // Release buffer.
  zx::eventpair token = std::move(*info.mutable_buffer_release_token());
  EXPECT_EQ(token.signal_peer(0, ZX_EVENTPAIR_SIGNALED), ZX_OK);

  // Now change the color of the screen.
  const ContentId kFilledRectId2 = {2};
  const TransformId kTransformId2 = {3};

  // Create a blue rectangle.
  child_session_->CreateFilledRect(kFilledRectId2);
  child_session_->SetSolidFill(kFilledRectId2, {0, 0, 1, 1}, {image_width, image_height});
  child_session_->CreateTransform(kTransformId2);
  child_session_->SetContent(kTransformId2, kFilledRectId2);

  // Attach the transform to child but do not Present.
  child_session_->AddChild(kChildRootTransform, kTransformId2);

  fpromise::result<FrameInfo, ScreenCaptureError> gnf_result2;
  bool callback_result2 = false;
  screen_capture_->GetNextFrame(
      [&gnf_result2, &callback_result2](fpromise::result<FrameInfo, ScreenCaptureError> result) {
        EXPECT_FALSE(result.is_error());
        gnf_result2 = std::move(result);
        callback_result2 = true;
      });

  // Client has recieved last frame so will hang until OnCpuWorkDone fires MaybeRenderFrame().
  RunLoopWithTimeoutOrUntil([&callback_result2] { return callback_result2; }, kEventDelay);
  EXPECT_FALSE(callback_result2);
  child_session_->Present({});

  RunLoopWithTimeoutOrUntil([&callback_result2] { return callback_result2; }, kEventDelay);
  EXPECT_TRUE(gnf_result2.is_ok());
  FrameInfo info2 = std::move(gnf_result2.value());

  const auto& read_values2 =
      ExtractScreenCapture(info2.buffer_index(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);

  EXPECT_EQ(read_values2.size(), num_pixels_ * kBytesPerPixel);

  uint32_t num_blue_count = 0;

  for (size_t i = 0; i < read_values2.size(); i += kBytesPerPixel) {
    if (PixelEquals(&read_values2[i], kBlue))
      num_blue_count++;
  }

  EXPECT_EQ(num_blue_count, num_pixels_);
}

// If there are no available buffers for GetNextFrame() to render into, the client should hang until
// they release a buffer and then receive the frame immedietly.
TEST_F(ScreenCapture2IntegrationTest, ClientReleaseBufferCapture) {
  const uint32_t image_width = display_width_;
  const uint32_t image_height = display_height_;
  const uint32_t render_target_width = display_width_;
  const uint32_t render_target_height = display_height_;

  allocation::BufferCollectionImportExportTokens ref_pair =
      allocation::BufferCollectionImportExportTokens::New();

  fuchsia::sysmem2::BufferCollectionInfo buffer_collection_info =
      CreateBufferCollectionInfoWithConstraints(
          utils::CreateDefaultConstraints(/*buffer_count=*/1, image_width, image_height),
          std::move(ref_pair.export_token), flatland_allocator_.get(), sysmem_allocator_.get(),
          RegisterBufferCollectionUsages::DEFAULT);

  fuchsia::sysmem2::BufferCollectionInfo sc_buffer_collection_info = ConfigureScreenCapture(
      utils::CreateDefaultConstraints(/*buffer_count=*/1, image_width, image_height),
      render_target_width, render_target_height);

  fpromise::result<FrameInfo, ScreenCaptureError> gnf_result;
  bool callback_result = false;
  screen_capture_->GetNextFrame(
      [&gnf_result, &callback_result](fpromise::result<FrameInfo, ScreenCaptureError> result) {
        EXPECT_FALSE(result.is_error());
        gnf_result = std::move(result);
        callback_result = true;
      });
  RunLoopWithTimeoutOrUntil([&callback_result] { return callback_result; }, kEventDelay);
  EXPECT_TRUE(gnf_result.is_ok());
  FrameInfo info = std::move(gnf_result.value());

  std::vector<uint8_t> write_values;
  for (uint32_t i = 0; i < num_pixels_; ++i) {
    write_values.insert(write_values.end(), kRed, kRed + kBytesPerPixel);
  }

  WriteToSysmemBuffer(write_values, buffer_collection_info, 0, kBytesPerPixel, image_width,
                      image_height);
  GenerateImageForFlatlandInstance(0, child_session_, kChildRootTransform,
                                   std::move(ref_pair.import_token), {image_width, image_height},
                                   {0, 0}, 2, 2);

  fpromise::result<FrameInfo, ScreenCaptureError> gnf_result2;
  bool callback_result2 = false;
  screen_capture_->GetNextFrame(
      [&gnf_result2, &callback_result2](fpromise::result<FrameInfo, ScreenCaptureError> result) {
        EXPECT_FALSE(result.is_error());
        gnf_result2 = std::move(result);
        callback_result2 = true;
      });

  // Client has recieved last frame so GetNextFrame will hang.
  RunLoopWithTimeoutOrUntil([&callback_result2] { return callback_result2; }, kEventDelay);
  EXPECT_FALSE(callback_result2);

  // Client does not have any buffers available so OnCpuWorkDone will not render into buffer.
  child_session_->Present({});
  RunLoopWithTimeoutOrUntil([&callback_result2] { return callback_result2; }, kEventDelay);
  EXPECT_FALSE(callback_result2);

  // Client releases buffer.
  zx::eventpair token = std::move(*info.mutable_buffer_release_token());
  EXPECT_EQ(token.signal_peer(0, ZX_EVENTPAIR_SIGNALED), ZX_OK);

  RunLoopWithTimeoutOrUntil([&callback_result2] { return callback_result2; }, kEventDelay);
  EXPECT_TRUE(gnf_result2.is_ok());
  FrameInfo info2 = std::move(gnf_result2.value());

  const auto& read_values =
      ExtractScreenCapture(info2.buffer_index(), sc_buffer_collection_info, kBytesPerPixel,
                           render_target_width, render_target_height);
  EXPECT_EQ(read_values.size(), write_values.size());

  uint32_t num_red = 0;
  for (size_t i = 0; i < read_values.size(); i += kBytesPerPixel) {
    if (PixelEquals(&read_values[i], kRed))
      num_red++;
  }
  EXPECT_EQ(num_red, num_pixels_);
}

}  // namespace integration_tests
