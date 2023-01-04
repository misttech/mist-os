// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/screenshot/flatland_screenshot.h"

#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/sys/cpp/testing/component_context_provider.h>

#include <utility>

#include <gtest/gtest.h>

#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/ui/scenic/lib/allocation/allocator.h"
#include "src/ui/scenic/lib/flatland/renderer/null_renderer.h"
#include "src/ui/scenic/lib/screen_capture/screen_capture_buffer_collection_importer.h"

using allocation::BufferCollectionImporter;
using fuchsia::ui::composition::ScreenshotFormat;
using fuchsia::ui::composition::ScreenshotTakeFileResponse;
using fuchsia::ui::composition::ScreenshotTakeResponse;
using screen_capture::ScreenCaptureBufferCollectionImporter;

namespace screenshot {
namespace test {

constexpr auto kDisplayWidth = 100u;
constexpr auto kDisplayHeight = 200u;

class FlatlandScreenshotTest : public gtest::RealLoopFixture,
                               public ::testing::WithParamInterface<int> {
 public:
  FlatlandScreenshotTest() = default;
  void SetUp() override {
    renderer_ = std::make_shared<flatland::NullRenderer>();
    importer_ = std::make_shared<ScreenCaptureBufferCollectionImporter>(
        utils::CreateSysmemAllocatorSyncPtr("ScreenshotTest"), renderer_);

    std::vector<std::shared_ptr<BufferCollectionImporter>> screenshot_importers;
    screenshot_importers.push_back(importer_);

    screen_capturer_ = std::make_unique<screen_capture::ScreenCapture>(
        screenshot_importers, renderer_,
        /*get_renderables=*/[](auto...) {
          return std::make_pair<std::vector<flatland::ImageRect>,
                                std::vector<allocation::ImageMetadata>>({}, {});
        });

    // Create flatland allocator.
    {
      std::vector<std::shared_ptr<BufferCollectionImporter>> extra_importers;
      std::vector<std::shared_ptr<BufferCollectionImporter>> screenshot_importers;
      screenshot_importers.push_back(importer_);
      flatland_allocator_ = std::make_shared<allocation::Allocator>(
          context_provider_.context(), extra_importers, screenshot_importers,
          utils::CreateSysmemAllocatorSyncPtr("-allocator"));
    }

    // We have what we need to make the flatland screenshot client.

    fuchsia::math::SizeU display_size = {.width = kDisplayWidth, .height = kDisplayHeight};

    flatland_screenshotter_ = std::make_unique<screenshot::FlatlandScreenshot>(
        std::move(screen_capturer_), flatland_allocator_, display_size, GetParam(), [](auto...) {});
    RunLoopUntilIdle();
  }

  void TearDown() override {}

  std::unique_ptr<screenshot::FlatlandScreenshot> flatland_screenshotter_;

 protected:
  size_t NumCurrentServedScreenshots() {
    return flatland_screenshotter_->NumCurrentServedScreenshots();
  }

  ScreenshotTakeFileResponse TakeFile(ScreenshotFormat format = ScreenshotFormat::BGRA_RAW) {
    fuchsia::ui::composition::ScreenshotTakeFileRequest request;
    request.set_format(format);

    ScreenshotTakeFileResponse take_file_response;
    bool done = false;

    flatland_screenshotter_->TakeFile(
        std::move(request), [&take_file_response, &done](ScreenshotTakeFileResponse response) {
          take_file_response = std::move(response);
          done = true;
        });
    RunLoopUntil([&done] { return done; });

    return take_file_response;
  }

 private:
  sys::testing::ComponentContextProvider context_provider_;

  std::shared_ptr<flatland::NullRenderer> renderer_;
  std::shared_ptr<ScreenCaptureBufferCollectionImporter> importer_;

  std::shared_ptr<allocation::Allocator> flatland_allocator_;

  std::unique_ptr<screen_capture::ScreenCapture> screen_capturer_;
};

INSTANTIATE_TEST_SUITE_P(ParameterizedFlatlandScreenshotTest, FlatlandScreenshotTest,
                         testing::Values(0, 90, 180, 270));

TEST_P(FlatlandScreenshotTest, SimpleTest) {
  fuchsia::ui::composition::ScreenshotTakeRequest request;
  request.set_format(fuchsia::ui::composition::ScreenshotFormat::BGRA_RAW);

  fuchsia::ui::composition::ScreenshotTakeResponse take_response;
  bool done = false;

  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);
  flatland_screenshotter_->Take(
      std::move(request),
      [&take_response, &done](fuchsia::ui::composition::ScreenshotTakeResponse response) {
        take_response = std::move(response);
        done = true;
      });

  RunLoopUntil([&done] { return done; });

  EXPECT_TRUE(take_response.has_vmo());
  EXPECT_TRUE(take_response.has_size());
  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);

  // Width and height are flipped when the display is rotated by 90 or 270 degrees.
  if (GetParam() == 90 || GetParam() == 270) {
    EXPECT_EQ(take_response.size().width, kDisplayHeight);
    EXPECT_EQ(take_response.size().height, kDisplayWidth);

  } else {
    EXPECT_EQ(take_response.size().width, kDisplayWidth);
    EXPECT_EQ(take_response.size().height, kDisplayHeight);
  }
  EXPECT_NE(take_response.vmo(), 0u);
}

TEST_P(FlatlandScreenshotTest, SimpleTakeFileTest) {
  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);

  ScreenshotTakeFileResponse takefile_response = TakeFile();

  EXPECT_EQ(NumCurrentServedScreenshots(), 1u);

  EXPECT_TRUE(takefile_response.has_size());

  fidl::InterfaceHandle<::fuchsia::io::File>* file = takefile_response.mutable_file();
  EXPECT_TRUE(file->is_valid());
  {
    fuchsia::io::FilePtr screenshot = file->Bind();
    // Get screenshot attributes.
    uint64_t screenshot_size;
    screenshot->GetAttr(
        [&screenshot_size](zx_status_t status, fuchsia::io::NodeAttributes attributes) {
          EXPECT_EQ(ZX_OK, status);
          screenshot_size = attributes.content_size;
        });

    uint64_t read_count = 0;
    uint64_t increment = 0;
    do {
      screenshot->Read(fuchsia::io::MAX_BUF,
                       [&increment](fuchsia::io::Readable_Read_Result result) {
                         EXPECT_TRUE(result.is_response()) << zx_status_get_string(result.err());
                         increment = result.response().data.size();
                       });
      RunLoopUntilIdle();
      read_count += increment;
    } while (increment);
    EXPECT_EQ(screenshot_size, read_count);
  }

  RunLoopUntilIdle();
  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);
}

TEST_P(FlatlandScreenshotTest, GetMultipleScreenshotsViaChannel) {
  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);

  // Serve clients.

  auto response1 = TakeFile();
  RunLoopUntilIdle();
  fidl::InterfaceHandle<::fuchsia::io::File>* file1 = response1.mutable_file();
  EXPECT_EQ(NumCurrentServedScreenshots(), 1u);

  auto response2 = TakeFile();
  RunLoopUntilIdle();
  fidl::InterfaceHandle<::fuchsia::io::File>* file2 = response2.mutable_file();
  EXPECT_EQ(NumCurrentServedScreenshots(), 2u);

  auto response3 = TakeFile();
  RunLoopUntilIdle();
  fidl::InterfaceHandle<::fuchsia::io::File>* file3 = response3.mutable_file();
  EXPECT_EQ(NumCurrentServedScreenshots(), 3u);

  // Close clients.
  file3->TakeChannel().reset();
  RunLoopUntilIdle();
  EXPECT_EQ(NumCurrentServedScreenshots(), 2u);

  file2->TakeChannel().reset();
  RunLoopUntilIdle();
  EXPECT_EQ(NumCurrentServedScreenshots(), 1u);

  file1->TakeChannel().reset();
  RunLoopUntilIdle();
  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);
}

}  // namespace test
}  // namespace screenshot
