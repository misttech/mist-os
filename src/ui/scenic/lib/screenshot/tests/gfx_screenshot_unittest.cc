// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/screenshot/gfx_screenshot.h"

#include <fuchsia/ui/composition/cpp/fidl.h>

#include <utility>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

namespace screenshot {
namespace test {

using fuchsia::ui::composition::ScreenshotFormat;
using fuchsia::ui::composition::ScreenshotTakeFileResponse;
using fuchsia::ui::composition::ScreenshotTakeResponse;

class GfxScreenshotTest : public gtest::RealLoopFixture {
 public:
  GfxScreenshotTest() = default;
  void SetUp() override {
    gfx_screenshotter_ = std::make_unique<screenshot::GfxScreenshot>(
        [](fuchsia::ui::scenic::Scenic::TakeScreenshotCallback callback) {
          fuchsia::ui::scenic::ScreenshotData screenshot_data;
          fuchsia::images::ImageInfo image_info;
          fuchsia::mem::Buffer data_buffer;

          // Create |info|.
          image_info.width = 100u;
          image_info.height = 100u;

          // Create |data|.
          zx::vmo vmo;
          zx_status_t status = zx::vmo::create(4096, 0, &vmo);
          EXPECT_EQ(status, ZX_OK);
          data_buffer.vmo = std::move(vmo);

          screenshot_data.info = image_info;
          screenshot_data.data = std::move(data_buffer);

          callback(std::move(screenshot_data), /*success=*/true);
        },
        [](screenshot::GfxScreenshot* screenshotter) {});
  }

  void TearDown() override {}

  std::unique_ptr<screenshot::GfxScreenshot> gfx_screenshotter_;

 protected:
  size_t NumCurrentServedScreenshots() { return gfx_screenshotter_->NumCurrentServedScreenshots(); }

  ScreenshotTakeFileResponse TakeFile(ScreenshotFormat format = ScreenshotFormat::BGRA_RAW) {
    fuchsia::ui::composition::ScreenshotTakeFileRequest request;
    request.set_format(format);

    ScreenshotTakeFileResponse take_file_response;
    bool done = false;

    gfx_screenshotter_->TakeFile(std::move(request),
                                 [&take_file_response, &done](ScreenshotTakeFileResponse response) {
                                   take_file_response = std::move(response);
                                   done = true;
                                 });
    RunLoopUntil([&done] { return done; });

    return take_file_response;
  }
};

TEST_F(GfxScreenshotTest, SimpleTest) {
  fuchsia::ui::composition::ScreenshotTakeRequest request;
  request.set_format(ScreenshotFormat::BGRA_RAW);

  ScreenshotTakeResponse take_response;
  bool done = false;

  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);
  gfx_screenshotter_->Take(std::move(request),
                           [&take_response, &done](ScreenshotTakeResponse response) {
                             take_response = std::move(response);
                             done = true;
                           });

  RunLoopUntil([&done] { return done; });
  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);

  EXPECT_TRUE(take_response.has_vmo());
  EXPECT_TRUE(take_response.has_size());

  EXPECT_GT(take_response.size().width, 0u);
  EXPECT_GT(take_response.size().height, 0u);
  EXPECT_NE(take_response.vmo(), 0u);
}

TEST_F(GfxScreenshotTest, SimpleTakeFileTest) {
  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);

  ScreenshotTakeFileResponse takefile_response = TakeFile();

  EXPECT_EQ(NumCurrentServedScreenshots(), 1u);

  EXPECT_TRUE(takefile_response.has_size());
  EXPECT_GT(takefile_response.size().width, 0u);
  EXPECT_GT(takefile_response.size().height, 0u);

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

TEST_F(GfxScreenshotTest, GetMultipleScreenshotsViaChannel) {
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
  file2->TakeChannel().reset();
  RunLoopUntilIdle();
  EXPECT_EQ(NumCurrentServedScreenshots(), 2u);

  file1->TakeChannel().reset();
  RunLoopUntilIdle();
  EXPECT_EQ(NumCurrentServedScreenshots(), 1u);

  file3->TakeChannel().reset();
  RunLoopUntilIdle();
  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);
}

TEST_F(GfxScreenshotTest, SimpleUnreadableVmoTakeFileTest) {
  gfx_screenshotter_ = std::make_unique<screenshot::GfxScreenshot>(
      [](fuchsia::ui::scenic::Scenic::TakeScreenshotCallback callback) {
        fuchsia::ui::scenic::ScreenshotData screenshot_data;
        fuchsia::images::ImageInfo image_info;
        fuchsia::mem::Buffer data_buffer;

        // Create |info|.
        image_info.width = 100u;
        image_info.height = 100u;

        // Create |data|.
        zx::vmo vmo;
        zx_status_t status = zx::vmo::create(4096, 0, &vmo);
        EXPECT_EQ(status, ZX_OK);
        EXPECT_EQ(vmo.set_cache_policy(ZX_CACHE_POLICY_UNCACHED_DEVICE), ZX_OK);
        data_buffer.vmo = std::move(vmo);

        screenshot_data.info = image_info;
        screenshot_data.data = std::move(data_buffer);

        callback(std::move(screenshot_data), /*success=*/true);
      },
      [](screenshot::GfxScreenshot* screenshotter) {});

  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);

  ScreenshotTakeFileResponse takefile_response = TakeFile();

  EXPECT_EQ(NumCurrentServedScreenshots(), 1u);

  EXPECT_TRUE(takefile_response.has_size());
  EXPECT_GT(takefile_response.size().width, 0u);
  EXPECT_GT(takefile_response.size().height, 0u);

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

}  // namespace test
}  // namespace screenshot
