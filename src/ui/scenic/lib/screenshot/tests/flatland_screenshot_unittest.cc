// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/screenshot/flatland_screenshot.h"

#include <fidl/fuchsia.ui.composition/cpp/fidl.h>
#include <fidl/fuchsia.ui.compression.internal/cpp/fidl.h>
#include <lib/sys/cpp/testing/component_context_provider.h>

#include <functional>
#include <iostream>
#include <utility>

#include <gtest/gtest.h>

#include "lib/fidl/cpp/hlcpp_conversion.h"
#include "src/lib/fsl/vmo/sized_vmo.h"
#include "src/lib/fsl/vmo/vector.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/ui/scenic/lib/allocation/allocator.h"
#include "src/ui/scenic/lib/flatland/renderer/null_renderer.h"
#include "src/ui/scenic/lib/image-compression/image_compression.h"
#include "src/ui/scenic/lib/screen_capture/screen_capture_buffer_collection_importer.h"
#include "src/ui/scenic/lib/screenshot/screenshot_manager.h"
#include "src/ui/scenic/lib/screenshot/tests/mock_image_compression.h"
#include "src/ui/scenic/lib/utils/helpers.h"

using allocation::BufferCollectionImporter;
using fuchsia::ui::compression::internal::ImageCompressorEncodePngRequest;
using fuchsia_ui_composition::ScreenshotFormat;
using fuchsia_ui_composition::ScreenshotTakeFileResponse;
using fuchsia_ui_composition::ScreenshotTakeResponse;
using screen_capture::ScreenCaptureBufferCollectionImporter;

namespace screenshot {
namespace test {

constexpr auto kDisplayWidth = 100u;
constexpr auto kDisplayHeight = 200u;

constexpr auto kResolveEncodePng = [](ImageCompressorEncodePngRequest request,
                                      MockImageCompression::EncodePngCallback callback) -> void {
  fuchsia::ui::compression::internal::ImageCompressor_EncodePng_Result result;
  if (!request.has_raw_vmo() || !request.has_png_vmo() || !request.has_image_dimensions()) {
    result.set_err(fuchsia::ui::compression::internal::ImageCompressionError::MISSING_ARGS);
  } else {
    uint64_t in_vmo_size;
    FX_CHECK(request.raw_vmo().get_size(&in_vmo_size) == ZX_OK);
    fsl::SizedVmo raw_image = fsl::SizedVmo(std::move(*request.mutable_raw_vmo()), in_vmo_size);
    std::vector<uint8_t> imgdata;
    fsl::VectorFromVmo(raw_image, &imgdata);

    // Just dump raw image data into png_vmo. Don't actually compress, just want to ensure
    // Take() with PNG format makes a call to EncodePng().
    FX_CHECK(request.png_vmo().write(imgdata.data(), 0, imgdata.size() * sizeof(uint8_t)) == ZX_OK);

    fuchsia::ui::compression::internal::ImageCompressor_EncodePng_Response value;
    result.set_response(value);
  }
  callback(std::move(result));
};

fidl::Endpoints<fuchsia_ui_compression_internal::ImageCompressor> CreateImageCompressorEndpoints() {
  zx::result<fidl::Endpoints<fuchsia_ui_compression_internal::ImageCompressor>> endpoints_result =
      fidl::CreateEndpoints<fuchsia_ui_compression_internal::ImageCompressor>();
  FX_CHECK(endpoints_result.is_ok())
      << "Failed to create endpoints: " << endpoints_result.status_string();
  return std::move(endpoints_result.value());
}

class FlatlandScreenshotTest : public gtest::RealLoopFixture,
                               public ::testing::WithParamInterface<
                                   std::tuple<fuchsia_ui_composition::ScreenshotFormat, int>> {
 public:
  FlatlandScreenshotTest() = default;
  void SetUp() override {
    renderer_ = std::make_shared<flatland::NullRenderer>();
    importer_ = std::make_shared<ScreenCaptureBufferCollectionImporter>(
        utils::CreateSysmemAllocatorSyncPtr("ScreenshotTest"), renderer_);

    auto compressor_channel = CreateImageCompressorEndpoints();
    mock_compressor_.Bind(compressor_channel.server.TakeChannel(), async_get_default_dispatcher());

    screenshot::CompressorEventHandler event_handler;
    fidl::Client client(std::move(compressor_channel.client), async_get_default_dispatcher(),
                        &event_handler);

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
        std::move(screen_capturer_), flatland_allocator_, display_size, std::get<1>(GetParam()),
        std::move(client), [](auto...) {});
    RunLoopUntilIdle();
  }

  void TearDown() override {}

  std::unique_ptr<screenshot::FlatlandScreenshot> flatland_screenshotter_;

 protected:
  size_t NumCurrentServedScreenshots() {
    return flatland_screenshotter_->NumCurrentServedScreenshots();
  }

  ScreenshotTakeFileResponse TakeFile(ScreenshotFormat format) {
    fuchsia_ui_composition::ScreenshotTakeFileRequest request;
    request.format(format);

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

  MockImageCompression mock_compressor_;

 private:
  sys::testing::ComponentContextProvider context_provider_;

  std::shared_ptr<flatland::NullRenderer> renderer_;
  std::shared_ptr<ScreenCaptureBufferCollectionImporter> importer_;

  std::shared_ptr<allocation::Allocator> flatland_allocator_;

  std::unique_ptr<screen_capture::ScreenCapture> screen_capturer_;
};

INSTANTIATE_TEST_SUITE_P(
    ParameterizedFlatlandScreenshotTest, FlatlandScreenshotTest,
    ::testing::Combine(::testing::Values(fuchsia_ui_composition::ScreenshotFormat::kBgraRaw,
                                         fuchsia_ui_composition::ScreenshotFormat::kRgbaRaw,
                                         fuchsia_ui_composition::ScreenshotFormat::kPng),
                       ::testing::Values(0, 90, 180, 270)));

TEST_P(FlatlandScreenshotTest, SimpleTest) {
  const auto& [format, rotation] = GetParam();
  if (format == fuchsia_ui_composition::ScreenshotFormat::kPng) {
    EXPECT_CALL(mock_compressor_, EncodePng(testing::_, testing::_))
        .Times(1)
        .WillOnce(kResolveEncodePng);
  }

  fuchsia_ui_composition::ScreenshotTakeRequest request;
  request.format() = format;

  fuchsia_ui_composition::ScreenshotTakeResponse take_response;
  bool done = false;

  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);
  flatland_screenshotter_->Take(
      std::move(request),
      [&take_response, &done](fuchsia_ui_composition::ScreenshotTakeResponse response) {
        take_response = std::move(response);
        done = true;
      });

  RunLoopUntil([&done] { return done; });

  EXPECT_TRUE(take_response.vmo().has_value());
  EXPECT_TRUE(take_response.size().has_value());
  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);

  // Width and height are flipped when the display is rotated by 90 or 270 degrees.
  if (rotation == 90 || rotation == 270) {
    EXPECT_EQ(take_response.size()->width(), kDisplayHeight);
    EXPECT_EQ(take_response.size()->height(), kDisplayWidth);

  } else {
    EXPECT_EQ(take_response.size()->width(), kDisplayWidth);
    EXPECT_EQ(take_response.size()->height(), kDisplayHeight);
  }
  EXPECT_NE(take_response.vmo(), 0u);
}

TEST_P(FlatlandScreenshotTest, SimpleTakeFileTest) {
  const auto& [format, _] = GetParam();
  if (format == fuchsia_ui_composition::ScreenshotFormat::kPng) {
    EXPECT_CALL(mock_compressor_, EncodePng(testing::_, testing::_))
        .Times(1)
        .WillOnce(kResolveEncodePng);
  }

  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);

  ScreenshotTakeFileResponse takefile_response = TakeFile(format);

  EXPECT_EQ(NumCurrentServedScreenshots(), 1u);

  EXPECT_TRUE(takefile_response.size().has_value());

  auto file = fidl::NaturalToHLCPP(takefile_response.file().value());
  EXPECT_TRUE(file.is_valid());
  {
    fuchsia::io::FilePtr screenshot = file.Bind();
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
  const auto& [format, _] = GetParam();
  if (format == fuchsia_ui_composition::ScreenshotFormat::kPng) {
    EXPECT_CALL(mock_compressor_, EncodePng(testing::_, testing::_))
        .Times(3)
        .WillRepeatedly(kResolveEncodePng);
  }

  EXPECT_EQ(NumCurrentServedScreenshots(), 0u);

  // Serve clients.

  auto response1 = TakeFile(format);
  RunLoopUntilIdle();
  auto& file1 = response1.file();
  EXPECT_EQ(NumCurrentServedScreenshots(), 1u);

  auto response2 = TakeFile(format);
  RunLoopUntilIdle();
  auto& file2 = response2.file();
  EXPECT_EQ(NumCurrentServedScreenshots(), 2u);

  auto response3 = TakeFile(format);
  RunLoopUntilIdle();
  auto& file3 = response3.file();
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
