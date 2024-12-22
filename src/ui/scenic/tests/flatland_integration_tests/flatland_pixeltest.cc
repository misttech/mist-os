// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/sysmem/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_creation_tokens.h>
#include <lib/ui/scenic/cpp/view_identity.h>

#include <cmath>
#include <cstdint>

#include <zxtest/zxtest.h>

#include "src/ui/scenic/lib/allocation/buffer_collection_import_export_tokens.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/tests/utils/blocking_present.h"
#include "src/ui/scenic/tests/utils/scenic_ctf_test_base.h"
#include "src/ui/scenic/tests/utils/utils.h"
#include "src/ui/testing/util/screenshot_helper.h"

namespace integration_tests {

namespace fuc = fuchsia::ui::composition;

#define EXPECT_NEAR(val1, val2, eps)                                         \
  EXPECT_LE(std::abs(static_cast<double>(val1) - static_cast<double>(val2)), \
            static_cast<double>(eps))

constexpr fuc::TransformId kRootTransform{.value = 1};
constexpr auto kEpsilon = 1;

fuc::ColorRgba GetColorInFloat(utils::Pixel color) {
  return {static_cast<float>(color.red) / 255.f, static_cast<float>(color.green) / 255.f,
          static_cast<float>(color.blue) / 255.f, static_cast<float>(color.alpha) / 255.f};
}

// Asserts whether the BGRA channel value difference between |actual| and |expected| is at most
// |kEpsilon|.
void CompareColor(utils::Pixel actual, utils::Pixel expected) {
  EXPECT_NEAR(actual.blue, expected.blue, kEpsilon);
  EXPECT_NEAR(actual.green, expected.green, kEpsilon);
  EXPECT_NEAR(actual.red, expected.red, kEpsilon);
  EXPECT_NEAR(actual.alpha, expected.alpha, kEpsilon);
}

// Test fixture that sets up an environment with a Scenic we can connect to.
class FlatlandPixelTestBase : public ScenicCtfTest {
 public:
  void SetUp() override {
    ScenicCtfTest::SetUp();

    LocalServiceDirectory()->Connect(sysmem_allocator_.NewRequest());

    flatland_display_ = ConnectAsyncIntoRealm<fuc::FlatlandDisplay>();
    flatland_display_.set_error_handler([](zx_status_t status) {
      FX_LOGS(ERROR) << "Lost connection to Scenic: " << zx_status_get_string(status);
      FAIL();
    });

    flatland_allocator_ = ConnectSyncIntoRealm<fuc::Allocator>();

    // Create a root view.
    root_flatland_ = ConnectAsyncIntoRealm<fuc::Flatland>();
    root_flatland_.set_error_handler([](zx_status_t status) {
      FX_LOGS(ERROR) << "Lost connection to Scenic: " << zx_status_get_string(status);
      FAIL();
    });

    // Attach |root_flatland_| as the only Flatland under |flatland_display_|.
    auto [child_token, parent_token] = scenic::ViewCreationTokenPair::New();
    fidl::InterfacePtr<fuc::ChildViewWatcher> child_view_watcher;
    flatland_display_->SetContent(std::move(parent_token), child_view_watcher.NewRequest());
    fidl::InterfacePtr<fuc::ParentViewportWatcher> parent_viewport_watcher;
    root_flatland_->CreateView2(std::move(child_token), scenic::NewViewIdentityOnCreation(), {},
                                parent_viewport_watcher.NewRequest());

    // Create the root transform.
    root_flatland_->CreateTransform(kRootTransform);
    root_flatland_->SetRootTransform(kRootTransform);

    // Get the display's width and height. Since there is no Present in FlatlandDisplay, receiving
    // this callback ensures that all |flatland_display_| calls are processed.
    std::optional<fuchsia::ui::composition::LayoutInfo> info;
    parent_viewport_watcher->GetLayout([&info](auto result) { info = std::move(result); });
    RunLoopUntil([&info] { return info.has_value(); });
    display_width_ = info->logical_size().width;
    display_height_ = info->logical_size().height;

    screenshotter_ = ConnectSyncIntoRealm<fuc::Screenshot>();
  }

  void TearDown() override {
    root_flatland_.Unbind();
    flatland_display_.Unbind();

    zxtest::Test::TearDown();
  }

  // Draws a rectangle of size |width|*|height|, color |color|, opacity |opacity| and origin
  // (|x|,|y|) in |flatland|'s view.
  // Note: |BlockingPresent| must be called after this function to present the rectangle on the
  // display.
  void DrawRectangle(fuc::FlatlandPtr& flatland, uint32_t width, uint32_t height, int32_t x,
                     int32_t y, utils::Pixel color, fuc::BlendMode blend_mode = fuc::BlendMode::SRC,
                     float opacity = 1.f) {
    const fuc::ContentId kFilledRectId = {get_next_resource_id()};
    const fuc::TransformId kTransformId = {get_next_resource_id()};

    flatland->CreateFilledRect(kFilledRectId);
    flatland->SetSolidFill(kFilledRectId, GetColorInFloat(color), {width, height});

    // Associate the rect with a transform.
    flatland->CreateTransform(kTransformId);
    flatland->SetContent(kTransformId, kFilledRectId);
    flatland->SetTranslation(kTransformId, {x, y});

    // Set the opacity and the BlendMode for the rectangle.
    flatland->SetImageBlendingFunction(kFilledRectId, blend_mode);
    flatland->SetOpacity(kTransformId, opacity);

    // Attach the transform to the view.
    flatland->AddChild(fuchsia::ui::composition::TransformId{kRootTransform}, kTransformId);
  }

  fuchsia::sysmem2::BufferCollectionConstraints GetBufferConstraints(
      fuchsia::images2::PixelFormat pixel_format, fuchsia::images2::ColorSpace color_space) {
    fuchsia::sysmem2::BufferCollectionConstraints constraints;
    auto& bmc = *constraints.mutable_buffer_memory_constraints();
    bmc.set_ram_domain_supported(true);
    bmc.set_cpu_domain_supported(true);
    constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE_OFTEN);
    constraints.set_min_buffer_count(1);
    auto& image_constraints = constraints.mutable_image_format_constraints()->emplace_back();
    image_constraints.set_pixel_format(pixel_format);
    image_constraints.set_pixel_format_modifier(fuchsia::images2::PixelFormatModifier::LINEAR);
    image_constraints.mutable_color_spaces()->emplace_back(color_space);
    image_constraints.set_required_min_size(
        fuchsia::math::SizeU{.width = display_width_, .height = display_height_});
    image_constraints.set_required_max_size(
        fuchsia::math::SizeU{.width = display_width_, .height = display_height_});
    return constraints;
  }

  // Draws the following coordinate test pattern without views:
  // ___________________________________
  // |                |                |
  // |     BLACK      |        RED     |
  // |           _____|_____           |
  // |___________|  GREEN  |___________|
  // |           |_________|           |
  // |                |                |
  // |      BLUE      |     MAGENTA    |
  // |________________|________________|
  //
  void Draw4RectanglesToDisplay() {
    const uint32_t view_width = display_width_;
    const uint32_t view_height = display_height_;

    const uint32_t pane_width =
        static_cast<uint32_t>(std::ceil(static_cast<float>(view_width) / 2.f));

    const uint32_t pane_height =
        static_cast<uint32_t>(std::ceil(static_cast<float>(view_height) / 2.f));

    // Draw the rectangles in the quadrants.
    for (uint32_t i = 0; i < 2; i++) {
      for (uint32_t j = 0; j < 2; j++) {
        utils::Pixel color(static_cast<uint8_t>(j * 255), 0, static_cast<uint8_t>(i * 255), 255);
        DrawRectangle(root_flatland_, pane_width, pane_height, i * pane_width, j * pane_height,
                      color);
      }
    }

    // Draw the rectangle in the center.
    DrawRectangle(root_flatland_, view_width / 4, view_height / 4, 3 * view_width / 8,
                  3 * view_height / 8, utils::kGreen);
  }

 protected:
  fuchsia::sysmem2::BufferCollectionInfo SetConstraintsAndAllocateBuffer(
      fuchsia::sysmem2::BufferCollectionTokenSyncPtr token,
      fuchsia::sysmem2::BufferCollectionConstraints constraints) {
    fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;
    fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
    bind_shared_request.set_token(std::move(token));
    bind_shared_request.set_buffer_collection_request(buffer_collection.NewRequest());
    auto status = sysmem_allocator_->BindSharedCollection(std::move(bind_shared_request));
    FX_CHECK(status == ZX_OK);

    uint32_t constraints_min_buffer_count = constraints.min_buffer_count();

    fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.set_constraints(std::move(constraints));
    status = buffer_collection->SetConstraints(std::move(set_constraints_request));
    FX_CHECK(status == ZX_OK);

    fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
    status = buffer_collection->WaitForAllBuffersAllocated(&wait_result);
    FX_CHECK(status == ZX_OK);
    FX_CHECK(!wait_result.is_framework_err());
    FX_CHECK(!wait_result.is_err());
    auto buffer_collection_info =
        std::move(*wait_result.response().mutable_buffer_collection_info());
    EXPECT_EQ(constraints_min_buffer_count, buffer_collection_info.buffers().size());
    FX_CHECK(buffer_collection->Release() == ZX_OK);
    return buffer_collection_info;
  }

  uint32_t display_width_ = 0;
  uint32_t display_height_ = 0;

  fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator_;
  fuc::AllocatorSyncPtr flatland_allocator_;
  fuc::FlatlandPtr root_flatland_;
  fuc::ScreenshotSyncPtr screenshotter_;
  uint64_t get_next_resource_id() { return resource_id_++; }

 private:
  uint64_t resource_id_ = kRootTransform.value + 1;
  fuc::FlatlandDisplayPtr flatland_display_;
};

class ParameterizedPixelFormatTest
    : public FlatlandPixelTestBase,
      public zxtest::WithParamInterface<fuchsia::images2::PixelFormat> {};

class ParameterizedYUVPixelTest : public ParameterizedPixelFormatTest {};

INSTANTIATE_TEST_SUITE_P(YuvPixelFormats, ParameterizedYUVPixelTest,
                         zxtest::Values(fuchsia::images2::PixelFormat::NV12,
                                        fuchsia::images2::PixelFormat::I420));

TEST_P(ParameterizedYUVPixelTest, YUVTest) {
  auto [local_token, scenic_token] = utils::CreateSysmemTokens(sysmem_allocator_.get());

  // Send one token to Flatland Allocator.
  allocation::BufferCollectionImportExportTokens bc_tokens =
      allocation::BufferCollectionImportExportTokens::New();
  fuc::RegisterBufferCollectionArgs rbc_args = {};
  rbc_args.set_export_token(std::move(bc_tokens.export_token));
  rbc_args.set_buffer_collection_token2(std::move(scenic_token));
  fuc::Allocator_RegisterBufferCollection_Result result;
  ASSERT_OK(flatland_allocator_->RegisterBufferCollection(std::move(rbc_args), &result));
  ASSERT_FALSE(result.is_err());

  // Use the local token to allocate a protected buffer.
  auto info = SetConstraintsAndAllocateBuffer(
      std::move(local_token),
      GetBufferConstraints(GetParam(), fuchsia::images2::ColorSpace::REC709));

  // Write the pixel values to the VMO.
  const uint32_t num_pixels = display_width_ * display_height_;
  const uint64_t image_vmo_bytes = (3 * num_pixels) / 2;

  zx::vmo& image_vmo = *info.mutable_buffers()->at(0).mutable_vmo();
  zx_status_t status = zx::vmo::create(image_vmo_bytes, 0, &image_vmo);
  EXPECT_EQ(ZX_OK, status);

  uint8_t* vmo_base;
  status = zx::vmar::root_self()->map(ZX_VM_PERM_WRITE | ZX_VM_PERM_READ, 0, image_vmo, 0,
                                      image_vmo_bytes, reinterpret_cast<uintptr_t*>(&vmo_base));
  EXPECT_EQ(ZX_OK, status);

  static const uint8_t kYValue = 110;
  static const uint8_t kUValue = 192;
  static const uint8_t kVValue = 192;

  // Set all the Y pixels at full res.
  for (uint32_t i = 0; i < num_pixels; ++i) {
    vmo_base[i] = kYValue;
  }

  if (GetParam() == fuchsia::images2::PixelFormat::NV12) {
    // Set all the UV pixels pairwise at half res.
    for (uint32_t i = num_pixels; i < image_vmo_bytes; i += 2) {
      vmo_base[i] = kUValue;
      vmo_base[i + 1] = kVValue;
    }
  } else if (GetParam() == fuchsia::images2::PixelFormat::I420) {
    for (uint32_t i = num_pixels; i < num_pixels + num_pixels / 4; ++i) {
      vmo_base[i] = kUValue;
    }
    for (uint32_t i = num_pixels + num_pixels / 4; i < image_vmo_bytes; ++i) {
      vmo_base[i] = kVValue;
    }
  } else {
    FX_NOTREACHED();
  }

  // Flush the cache after writing to host VMO.
  EXPECT_EQ(ZX_OK, zx_cache_flush(vmo_base, image_vmo_bytes,
                                  ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE));

  // Create the image in the Flatland instance.
  fuc::ImageProperties image_properties = {};
  image_properties.set_size({display_width_, display_height_});
  const fuc::ContentId kImageContentId{.value = 1};

  root_flatland_->CreateImage(kImageContentId, std::move(bc_tokens.import_token), 0,
                              std::move(image_properties));

  // Present the created Image.
  root_flatland_->SetContent(kRootTransform, kImageContentId);
  BlockingPresent(this, root_flatland_);

  // TODO(https://fxbug.dev/42144501): provide reasoning for why this is the correct expected color.
  const utils::Pixel expected_pixel(255, 85, 249, 255);

  auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_);
  auto histogram = screenshot.Histogram();
  EXPECT_EQ(histogram[expected_pixel], num_pixels);
}

class ParameterizedSRGBPixelTest : public ParameterizedPixelFormatTest {};

INSTANTIATE_TEST_SUITE_P(RgbPixelFormats, ParameterizedSRGBPixelTest,
                         zxtest::Values(fuchsia::images2::PixelFormat::B8G8R8A8,
                                        fuchsia::images2::PixelFormat::R8G8B8A8
// TODO(https://fxbug.dev/351833287): Enable test on X86 once goldfish supports R5G6B5.
#if defined(__aarch64__)
                                        ,
                                        fuchsia::images2::PixelFormat::R5G6B5
#endif

                                        ));

TEST_P(ParameterizedSRGBPixelTest, RGBTest) {
  auto [local_token, scenic_token] = utils::CreateSysmemTokens(sysmem_allocator_.get());

  // Send one token to Flatland Allocator.
  allocation::BufferCollectionImportExportTokens bc_tokens =
      allocation::BufferCollectionImportExportTokens::New();
  fuc::RegisterBufferCollectionArgs rbc_args = {};
  rbc_args.set_export_token(std::move(bc_tokens.export_token));
  rbc_args.set_buffer_collection_token2(std::move(scenic_token));
  fuc::Allocator_RegisterBufferCollection_Result result;
  flatland_allocator_->RegisterBufferCollection(std::move(rbc_args), &result);
  ASSERT_FALSE(result.is_err());

  uint32_t bytes_per_pixel = 4;
  if (GetParam() == fuchsia::images2::PixelFormat::R5G6B5) {
    bytes_per_pixel = 2;
  }

  // Use the local token to allocate a protected buffer.
  auto info = SetConstraintsAndAllocateBuffer(
      std::move(local_token), GetBufferConstraints(GetParam(), fuchsia::images2::ColorSpace::SRGB));

  // Write the pixel values to the VMO.
  const uint32_t num_pixels = display_width_ * display_height_;
  const uint64_t image_vmo_bytes = num_pixels * bytes_per_pixel;
  ASSERT_EQ(image_vmo_bytes, info.settings().buffer_settings().size_bytes());

  const zx::vmo& image_vmo = info.buffers()[0].vmo();

  uint8_t* vmo_base;
  auto status =
      zx::vmar::root_self()->map(ZX_VM_PERM_WRITE | ZX_VM_PERM_READ, 0, image_vmo, 0,
                                 image_vmo_bytes, reinterpret_cast<uintptr_t*>(&vmo_base));
  EXPECT_EQ(ZX_OK, status);

  utils::Pixel color = utils::kBlue;
  vmo_base += info.buffers()[0].vmo_usable_start();

  for (uint32_t i = 0; i < num_pixels * bytes_per_pixel; i += bytes_per_pixel) {
    if (GetParam() == fuchsia::images2::PixelFormat::R5G6B5) {
      uint16_t color16 = static_cast<uint16_t>(((color.red >> 3) << 11) |
                                               ((color.green >> 2) << 5) | (color.blue >> 3));
      *reinterpret_cast<uint16_t*>(&vmo_base[i]) = color16;
    } else {
      // For BGRA32 pixel format, the first and the third byte in the pixel corresponds to the blue
      // and the red channel respectively.
      if (GetParam() == fuchsia::images2::PixelFormat::B8G8R8A8) {
        vmo_base[i] = color.blue;
        vmo_base[i + 2] = color.red;
      }
      // For R8G8B8A8 pixel format, the first and the third byte in the pixel corresponds to the red
      // and the blue channel respectively.
      if (GetParam() == fuchsia::images2::PixelFormat::R8G8B8A8) {
        vmo_base[i] = color.red;
        vmo_base[i + 2] = color.blue;
      }
      vmo_base[i + 1] = color.green;
      vmo_base[i + 3] = color.alpha;
    }
  }

  if (info.settings().buffer_settings().coherency_domain() ==
      fuchsia::sysmem2::CoherencyDomain::RAM) {
    EXPECT_EQ(ZX_OK, zx_cache_flush(vmo_base, image_vmo_bytes, ZX_CACHE_FLUSH_DATA));
  }

  // Create the image in the Flatland instance.
  fuc::ImageProperties image_properties = {};
  image_properties.set_size({display_width_, display_height_});
  const fuc::ContentId kImageContentId{.value = 1};

  root_flatland_->CreateImage(kImageContentId, std::move(bc_tokens.import_token), 0,
                              std::move(image_properties));

  // Present the created Image.
  root_flatland_->SetContent(kRootTransform, kImageContentId);
  BlockingPresent(this, root_flatland_);

  fuchsia::ui::composition::ScreenshotFormat ss_format;
  switch (GetParam()) {
    case fuchsia::images2::PixelFormat::B8G8R8A8:
      ss_format = fuchsia::ui::composition::ScreenshotFormat::BGRA_RAW;
      break;
    case fuchsia::images2::PixelFormat::R8G8B8A8:
      ss_format = fuchsia::ui::composition::ScreenshotFormat::RGBA_RAW;
      break;
    case fuchsia::images2::PixelFormat::R5G6B5:
      ss_format = fuchsia::ui::composition::ScreenshotFormat::BGRA_RAW;
      break;
    default:
      FX_LOGS(ERROR) << "Unexpected PixelFormat: " << GetParam();
      FAIL();
  }
  auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_, ss_format);
  auto histogram = screenshot.Histogram();

  if (GetParam() == fuchsia::images2::PixelFormat::R5G6B5) {
    color.alpha = 0xff;
  }

  EXPECT_EQ(histogram[color], num_pixels);
}

// Test a combination of orientations and image flips to ensure that images are flipped before the
// parent transform orientation is set and that the output is the expected output. For an ASCII
// representation of the input image, see |GetImageColorSetter|. For an ASCII representation of the
// expected output, see the constructor for |ParameterizedFlipAndOrientationTest|.
using FlipAndOrientationTestParams =
    std::tuple<fuchsia::images2::PixelFormat, fuc::Orientation, fuc::ImageFlip>;

class ParameterizedFlipAndOrientationTest
    : public FlatlandPixelTestBase,
      public zxtest::WithParamInterface<FlipAndOrientationTestParams> {
 protected:
  ParameterizedFlipAndOrientationTest() {
    // Image flip: LEFT_RIGHT; Orientation: CCW_0.
    //
    // |Bk|R |     |R |Bk|
    // |--|--| --> |--|--|
    // |G |Be|     |Be|G |
    //
    expected_colors_map.insert(
        {std::make_pair(fuc::ImageFlip::LEFT_RIGHT, fuc::Orientation::CCW_0_DEGREES),
         {.top_left = utils::kRed,
          .top_right = utils::kBlack,
          .bottom_left = utils::kBlue,
          .bottom_right = utils::kGreen}});

    // Image flip: LEFT_RIGHT; Orientation: CCW_90.
    //
    // |Bk|R |     |R |Bk|     |Bk|G |
    // |--|--| --> |--|--| --> |--|--|
    // |G |Be|     |Be|G |     |R |Be|
    //
    expected_colors_map.insert(
        {std::make_pair(fuc::ImageFlip::LEFT_RIGHT, fuc::Orientation::CCW_90_DEGREES),
         {.top_left = utils::kBlack,
          .top_right = utils::kGreen,
          .bottom_left = utils::kRed,
          .bottom_right = utils::kBlue}});

    // Image flip: LEFT_RIGHT; Orientation: CCW_180.
    //
    // |Bk|R |     |R |Bk|     |G |Be|
    // |--|--| --> |--|--| --> |--|--|
    // |G |Be|     |Be|G |     |Bk|R |
    //
    expected_colors_map.insert(
        {std::make_pair(fuc::ImageFlip::LEFT_RIGHT, fuc::Orientation::CCW_180_DEGREES),
         {.top_left = utils::kGreen,
          .top_right = utils::kBlue,
          .bottom_left = utils::kBlack,
          .bottom_right = utils::kRed}});

    // Image flip: LEFT_RIGHT; Orientation: CCW_270.
    //
    // |Bk|R |     |R |Bk|     |Be|R |
    // |--|--| --> |--|--| --> |--|--|
    // |G |Be|     |Be|G |     |G |Bk|
    //
    expected_colors_map.insert(
        {std::make_pair(fuc::ImageFlip::LEFT_RIGHT, fuc::Orientation::CCW_270_DEGREES),
         {.top_left = utils::kBlue,
          .top_right = utils::kRed,
          .bottom_left = utils::kGreen,
          .bottom_right = utils::kBlack}});

    // Image flip: UP_DOWN; Orientation: CCW_0.
    //
    // |Bk|R |     |G |Be|
    // |--|--| --> |--|--|
    // |G |Be|     |Bk|R |
    //
    expected_colors_map.insert(
        {std::make_pair(fuc::ImageFlip::UP_DOWN, fuc::Orientation::CCW_0_DEGREES),
         {.top_left = utils::kGreen,
          .top_right = utils::kBlue,
          .bottom_left = utils::kBlack,
          .bottom_right = utils::kRed}});

    // Image flip: UP_DOWN; Orientation: CCW_90.
    //
    // |Bk|R |     |G |Be|     |Be|R |
    // |--|--| --> |--|--| --> |--|--|
    // |G |Be|     |Bk|R |     |G |Bk|
    //
    expected_colors_map.insert(
        {std::make_pair(fuc::ImageFlip::UP_DOWN, fuc::Orientation::CCW_90_DEGREES),
         {.top_left = utils::kBlue,
          .top_right = utils::kRed,
          .bottom_left = utils::kGreen,
          .bottom_right = utils::kBlack}});

    // Image flip: UP_DOWN; Orientation: CCW_180.
    //
    // |Bk|R |     |G |Be|     |R |Bk|
    // |--|--| --> |--|--| --> |--|--|
    // |G |Be|     |Bk|R |     |Be|G |
    //
    expected_colors_map.insert(
        {std::make_pair(fuc::ImageFlip::UP_DOWN, fuc::Orientation::CCW_180_DEGREES),
         {.top_left = utils::kRed,
          .top_right = utils::kBlack,
          .bottom_left = utils::kBlue,
          .bottom_right = utils::kGreen}});

    // Image flip: UP_DOWN; Orientation: CCW_270.
    //
    // |Bk|R |     |G |Be|     |Bk|G |
    // |--|--| --> |--|--| --> |--|--|
    // |G |Be|     |Bk|R |     |R |Be|
    //
    expected_colors_map.insert(
        {std::make_pair(fuc::ImageFlip::UP_DOWN, fuc::Orientation::CCW_270_DEGREES),
         {.top_left = utils::kBlack,
          .top_right = utils::kGreen,
          .bottom_left = utils::kRed,
          .bottom_right = utils::kBlue}});
  }

  // Returns a color given the pixel index, to produce the following image:
  //
  // ___________________________________
  // |                |                |
  // |     BLACK      |     RED        |
  // |                |                |
  // |________________|________________|
  // |                |                |
  // |                |                |
  // |      GREEN     |     BLUE       |
  // |________________|________________|
  //
  auto GetPixelColor(unsigned int pixel_index, unsigned int bytes_per_row,
                     uint64_t image_vmo_bytes) {
    const utils::Pixel color_quadrants[2][2] = {
        {utils::kBlack, utils::kRed},
        {utils::kGreen, utils::kBlue},
    };
    int vertical_half_index = pixel_index < (image_vmo_bytes / 2) ? 0 : 1;
    int horizontal_half_index = (pixel_index % bytes_per_row) < (bytes_per_row / 2) ? 0 : 1;
    return color_quadrants[vertical_half_index][horizontal_half_index];
  }

  struct FlipAndOrientationHash {
    std::size_t operator()(std::pair<fuc::ImageFlip, fuc::Orientation> v) const {
      return static_cast<size_t>(v.first) << 16 | static_cast<size_t>(v.second);
    }
  };

  struct ExpectedColors {
    utils::Pixel top_left;
    utils::Pixel top_right;
    utils::Pixel bottom_left;
    utils::Pixel bottom_right;
  };

  std::unordered_map<std::pair<fuc::ImageFlip, fuc::Orientation>, ExpectedColors,
                     FlipAndOrientationHash>
      expected_colors_map;
};

INSTANTIATE_TEST_SUITE_P(ParameterizedFlipAndOrientationTestWithParams,
                         ParameterizedFlipAndOrientationTest,
                         zxtest::Combine(zxtest::Values(fuchsia::images2::PixelFormat::B8G8R8A8,
                                                        fuchsia::images2::PixelFormat::R8G8B8A8),
                                         zxtest::Values(fuc::Orientation::CCW_0_DEGREES,
                                                        fuc::Orientation::CCW_90_DEGREES,
                                                        fuc::Orientation::CCW_180_DEGREES,
                                                        fuc::Orientation::CCW_270_DEGREES),
                                         zxtest::Values(fuc::ImageFlip::LEFT_RIGHT,
                                                        fuc::ImageFlip::UP_DOWN)));

TEST_P(ParameterizedFlipAndOrientationTest, FlipAndOrientationRenderTest) {
  auto [pixel_format, orientation, image_flip] = GetParam();

  const uint32_t num_pixels = display_width_ * display_height_;
  constexpr auto kByterPerPixel = 4;
  const uint64_t image_vmo_bytes = num_pixels * kByterPerPixel;

  auto [local_token, scenic_token] = utils::CreateSysmemTokens(sysmem_allocator_.get());

  // Send one token to Flatland Allocator.
  allocation::BufferCollectionImportExportTokens bc_tokens =
      allocation::BufferCollectionImportExportTokens::New();
  fuc::RegisterBufferCollectionArgs rbc_args = {};
  rbc_args.set_export_token(std::move(bc_tokens.export_token));
  rbc_args.set_buffer_collection_token2(std::move(scenic_token));
  fuc::Allocator_RegisterBufferCollection_Result result;
  flatland_allocator_->RegisterBufferCollection(std::move(rbc_args), &result);
  ASSERT_FALSE(result.is_err());

  // Use the local token to allocate a protected buffer.
  auto info = SetConstraintsAndAllocateBuffer(
      std::move(local_token),
      GetBufferConstraints(pixel_format, fuchsia::images2::ColorSpace::SRGB));

  // Write the pixel values to the VMO.
  ASSERT_EQ(image_vmo_bytes, info.settings().buffer_settings().size_bytes());

  const zx::vmo& image_vmo = info.buffers()[0].vmo();

  unsigned int current_image_content_id = 1;
  uint8_t* vmo_base;
  auto status =
      zx::vmar::root_self()->map(ZX_VM_PERM_WRITE | ZX_VM_PERM_READ, 0, image_vmo, 0,
                                 image_vmo_bytes, reinterpret_cast<uintptr_t*>(&vmo_base));
  EXPECT_EQ(ZX_OK, status);

  vmo_base += info.buffers()[0].vmo_usable_start();

  unsigned int image_width = display_width_;
  unsigned int image_height = display_height_;
  if (orientation == fuc::Orientation::CCW_90_DEGREES ||
      orientation == fuc::Orientation::CCW_270_DEGREES) {
    std::swap(image_width, image_height);
  }

  auto ss_format = fuchsia::ui::composition::ScreenshotFormat::BGRA_RAW;
  unsigned int bytes_per_row = image_width * kByterPerPixel;
  for (uint32_t i = 0; i < image_vmo_bytes; i += kByterPerPixel) {
    const utils::Pixel color = GetPixelColor(i, bytes_per_row, image_vmo_bytes);
    // For BGRA32 pixel format, the first and the third byte in the pixel corresponds to the
    // blue
    // and the red channel respectively.
    if (pixel_format == fuchsia::images2::PixelFormat::B8G8R8A8) {
      vmo_base[i] = color.blue;
      vmo_base[i + 2] = color.red;
    }
    // For R8G8B8A8 pixel format, the first and the third byte in the pixel corresponds to the
    // red and the blue channel respectively.
    if (pixel_format == fuchsia::images2::PixelFormat::R8G8B8A8) {
      vmo_base[i] = color.red;
      vmo_base[i + 2] = color.blue;
      ss_format = fuchsia::ui::composition::ScreenshotFormat::RGBA_RAW;
    }
    vmo_base[i + 1] = color.green;
    vmo_base[i + 3] = color.alpha;
  }

  if (info.settings().buffer_settings().coherency_domain() ==
      fuchsia::sysmem2::CoherencyDomain::RAM) {
    EXPECT_EQ(ZX_OK, zx_cache_flush(vmo_base, image_vmo_bytes, ZX_CACHE_FLUSH_DATA));
  }

  fuc::ImageProperties image_properties = {};
  image_properties.set_size({image_width, image_height});
  const fuc::ContentId kImageContentId{.value = current_image_content_id++};

  root_flatland_->CreateImage(kImageContentId, std::move(bc_tokens.import_token), 0,
                              std::move(image_properties));
  root_flatland_->SetImageFlip(kImageContentId, image_flip);

  // Present the created Image.
  root_flatland_->SetContent(kRootTransform, kImageContentId);
  root_flatland_->SetOrientation(kRootTransform, orientation);

  // Translate back into position after orientating around top-left corner.
  fuchsia::math::Vec translation;
  switch (orientation) {
    case fuc::Orientation::CCW_0_DEGREES:
      translation = {0, 0};
      break;
    case fuc::Orientation::CCW_90_DEGREES:
      translation = {0, static_cast<int32_t>(image_width)};
      break;
    case fuc::Orientation::CCW_180_DEGREES:
      translation = {static_cast<int32_t>(image_width), static_cast<int32_t>(image_height)};
      break;
    case fuc::Orientation::CCW_270_DEGREES:
      translation = {static_cast<int32_t>(image_height), 0};
      break;
  }
  root_flatland_->SetTranslation(kRootTransform, translation);

  BlockingPresent(this, root_flatland_);

  auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_, ss_format);

  // Verify that the number of pixels is the same (i.e. the image hasn't changed).
  auto histogram = screenshot.Histogram();
  const uint32_t pixel_color_count = num_pixels / 4;
  // TODO(https://fxbug.dev/42067818): Switch to exact comparisons after Astro precision issues are
  // resolved.
  EXPECT_NEAR(histogram[utils::kBlue], pixel_color_count, display_width_);
  EXPECT_NEAR(histogram[utils::kGreen], pixel_color_count, display_width_);
  EXPECT_NEAR(histogram[utils::kBlack], pixel_color_count, display_width_);
  EXPECT_NEAR(histogram[utils::kRed], pixel_color_count, display_width_);

  // Verify that the screenshot corners are the expected color.
  const auto expected_colors = expected_colors_map.find(std::make_pair(image_flip, orientation));
  ASSERT_NE(expected_colors, expected_colors_map.end());
  EXPECT_EQ(screenshot.GetPixelAt(0, 0), expected_colors->second.top_left);
  EXPECT_EQ(screenshot.GetPixelAt(screenshot.width() - 1, 0), expected_colors->second.top_right);
  EXPECT_EQ(screenshot.GetPixelAt(0, screenshot.height() - 1), expected_colors->second.bottom_left);
  EXPECT_EQ(screenshot.GetPixelAt(screenshot.width() - 1, screenshot.height() - 1),
            expected_colors->second.bottom_right);
}

class ParameterizedScreenshotFormatTest
    : public FlatlandPixelTestBase,
      public zxtest::WithParamInterface<fuchsia::ui::composition::ScreenshotFormat> {};

INSTANTIATE_TEST_SUITE_P(ParameterizedScreenshotFormatTestWithParams,
                         ParameterizedScreenshotFormatTest,
                         zxtest::Values(fuchsia::ui::composition::ScreenshotFormat::BGRA_RAW,
                                        fuchsia::ui::composition::ScreenshotFormat::RGBA_RAW));
TEST_P(ParameterizedScreenshotFormatTest, CoordinateViewTest) {
  Draw4RectanglesToDisplay();

  BlockingPresent(this, root_flatland_);

  auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_, GetParam());

  // Check pixel content at all four corners.
  EXPECT_EQ(screenshot.GetPixelAt(0, 0), utils::kBlack);  // Top left
  EXPECT_EQ(screenshot.GetPixelAt(0, screenshot.height() - 1),
            utils::kBlue);  // Bottom left
  EXPECT_EQ(screenshot.GetPixelAt(screenshot.width() - 1, 0),
            utils::kRed);  // Top right
  EXPECT_EQ(screenshot.GetPixelAt(screenshot.width() - 1, screenshot.height() - 1),
            utils::kMagenta);  // Bottom right

  // Check pixel content at center of each rectangle.
  EXPECT_EQ(screenshot.GetPixelAt(screenshot.width() / 4, screenshot.height() / 4),
            utils::kBlack);  // Top left
  EXPECT_EQ(screenshot.GetPixelAt(screenshot.width() / 4, (3 * screenshot.height()) / 4),
            utils::kBlue);  // Bottom left
  EXPECT_EQ(screenshot.GetPixelAt((3 * screenshot.width()) / 4, screenshot.height() / 4),
            utils::kRed);  // Top right
  EXPECT_EQ(screenshot.GetPixelAt((3 * screenshot.width()) / 4, (3 * screenshot.height()) / 4),
            utils::kMagenta);  // Bottom right
  EXPECT_EQ(screenshot.GetPixelAt(screenshot.width() / 2, screenshot.height() / 2),
            utils::kGreen);  // Center
}

TEST_F(FlatlandPixelTestBase, TakeScreenshotCompressionTest) {
  Draw4RectanglesToDisplay();

  BlockingPresent(this, root_flatland_);

  auto raw_screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_);
  auto png_screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_,
                                       fuchsia::ui::composition::ScreenshotFormat::PNG);

  EXPECT_GE(png_screenshot.ComputeSimilarity(raw_screenshot), 100.f);
}

TEST_F(FlatlandPixelTestBase, TakeFileScreenshotCompressionTest) {
  Draw4RectanglesToDisplay();

  BlockingPresent(this, root_flatland_);

  auto raw_screenshot = TakeFileScreenshot(screenshotter_, display_width_, display_height_);
  auto png_screenshot = TakeFileScreenshot(screenshotter_, display_width_, display_height_,
                                           fuchsia::ui::composition::ScreenshotFormat::PNG);

  EXPECT_GE(png_screenshot.ComputeSimilarity(raw_screenshot), 100.f);
}

struct OpacityTestParams {
  float opacity;
  utils::Pixel expected_pixel;
};

class ParameterizedOpacityPixelTest : public FlatlandPixelTestBase,
                                      public zxtest::WithParamInterface<OpacityTestParams> {};

// We use the same background/foreground color for each test iteration, but
// vary the opacity.  When the opacity is 0% we expect the pure background
// color, and when it is 100% we expect the pure foreground color.  When
// opacity is 50% we expect a blend of the two when |f.u.c.BlendMode| is |f.u.c.BlendMode.SRC_OVER|.
INSTANTIATE_TEST_SUITE_P(
    Opacity, ParameterizedOpacityPixelTest,
    zxtest::Values(OpacityTestParams{.opacity = 0.0f, .expected_pixel = {0, 0, 255, 255}},
                   OpacityTestParams{.opacity = 0.5f, .expected_pixel = {0, 188, 188, 255}},
                   OpacityTestParams{.opacity = 1.0f, .expected_pixel = {0, 255, 0, 255}}));

// This test first draws a rectangle of size |display_width_* display_height_| and then draws
// another rectangle having same dimensions on the top.
TEST_P(ParameterizedOpacityPixelTest, OpacityTest) {
  utils::Pixel background_color(utils::kRed);
  utils::Pixel foreground_color(utils::kGreen);

  // Draw the background rectangle.
  DrawRectangle(root_flatland_, display_width_, display_height_, 0, 0, background_color);

  // Draw the foreground rectangle.
  DrawRectangle(root_flatland_, display_width_, display_height_, 0, 0, foreground_color,
                fuc::BlendMode::SRC_OVER, GetParam().opacity);

  BlockingPresent(this, root_flatland_);

  const auto num_pixels = display_width_ * display_height_;

  auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_);
  auto histogram = screenshot.Histogram();

  // There should be only one color here in the histogram.
  ASSERT_EQ(histogram.size(), 1u);
  CompareColor(histogram.begin()->first, GetParam().expected_pixel);

  EXPECT_EQ(histogram.begin()->second, num_pixels);
}

// This test checks whether any content drawn outside the view bounds are correctly clipped.
// The test draws a scene as shown below:-
//  bbbbbbbbbbxxxxxxxxxx
//  bbbbbbbbbbxxxxxxxxxx
//  bbbbbbbbbbxxxxxxxxxx
//  bbbbbbbbbbxxxxxxxxxx
//  bbbbbbbbbbxxxxxxxxxx
//  bbbbbbbbbbxxxxxxxxxx
//  bbbbbbbbbbxxxxxxxxxx
//  bbbbbbbbbbxxxxxxxxxx
//  bbbbbbbbbbxxxxxxxxxx
//  bbbbbbbbbbxxxxxxxxxx
// The first rectangle gets clipped outide the left half of the display and the second rectangle
// gets completely clipped because it was drawn outside of the view bounds.
TEST_F(FlatlandPixelTestBase, ViewBoundClipping) {
  // Create a child view.
  fuc::FlatlandPtr child;
  child = ConnectAsyncIntoRealm<fuc::Flatland>();
  uint32_t child_width = 0, child_height = 0;

  auto [view_creation_token, viewport_token] = scenic::ViewCreationTokenPair::New();
  fidl::InterfacePtr<fuc::ParentViewportWatcher> parent_viewport_watcher;
  child->CreateView2(std::move(view_creation_token), scenic::NewViewIdentityOnCreation(), {},
                     parent_viewport_watcher.NewRequest());
  BlockingPresent(this, child);

  // Connect the child view to the root view.
  const fuc::TransformId viewport_transform = {get_next_resource_id()};
  const fuc::ContentId viewport_content = {get_next_resource_id()};

  root_flatland_->CreateTransform(viewport_transform);
  fuc::ViewportProperties properties;

  // Allow the child view to draw content in the left half of the display.
  properties.set_logical_size({display_width_ / 2, display_height_});
  fidl::InterfacePtr<fuc::ChildViewWatcher> child_view_watcher;
  root_flatland_->CreateViewport(viewport_content, std::move(viewport_token), std::move(properties),
                                 child_view_watcher.NewRequest());
  root_flatland_->SetContent(viewport_transform, viewport_content);
  root_flatland_->AddChild(kRootTransform, viewport_transform);
  BlockingPresent(this, root_flatland_);

  parent_viewport_watcher->GetLayout([&child_width, &child_height](auto layout_info) {
    child_width = layout_info.logical_size().width;
    child_height = layout_info.logical_size().height;
  });
  RunLoopUntil([&child_width, &child_height] { return child_width > 0 && child_height > 0; });

  // Create the root transform for the child view.
  child->CreateTransform(kRootTransform);
  child->SetRootTransform(kRootTransform);

  const utils::Pixel default_color(0, 0, 0, 0);

  // The child view draws a rectangle partially outside of its view bounds.
  DrawRectangle(child, 2 * child_width, child_height, 0, 0, utils::kBlue);

  // The child view draws a rectangle completely outside its view bounds.
  DrawRectangle(child, 2 * child_width, child_height, display_width_ / 2, display_height_ / 2,
                utils::kGreen);
  BlockingPresent(this, child);

  auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_);
  EXPECT_EQ(screenshot.GetPixelAt(0, 0), utils::kBlue);
  EXPECT_EQ(screenshot.GetPixelAt(0, display_height_ - 1), utils::kBlue);

  // The top left and bottom right corner of the display lies outside the child view's bounds so
  // we do not see any color there.
  EXPECT_EQ(screenshot.GetPixelAt(display_width_ - 1, 0), default_color);
  EXPECT_EQ(screenshot.GetPixelAt(display_width_ - 1, display_height_ - 1), default_color);

  auto histogram = screenshot.Histogram();
  const auto num_pixels = static_cast<uint32_t>(display_width_ * display_height_);

  // The child view can only draw content inside its view bounds, hence we see |num_pixels/2| pixels
  // for the first rectangle.
  EXPECT_EQ(histogram[utils::kBlue], num_pixels / 2);

  // No pixels are seen for the second rectangle as it was drawn completely outside the view bounds.
  EXPECT_EQ(histogram[utils::kGreen], 0u);
  EXPECT_EQ(histogram[default_color], num_pixels / 2);
}

// This test verifies the behavior of view bound clipping when multiple views exist under a node
// that itself has a translation applied to it. We initially add a child view which is subsequently
// replaced with two new views, all which have a rectangle in each. The parent view is under a node
// that is translated (display_width/2, 0). We expect the two child views added with
// ReplaceChildren to apply the parent's translation to their translation. On the other hand, the
// second view, initially added as a child of the parent view, but removed with ReplaceChildren,
// should be removed from the parent's graph. This means that what you see on the screen should
// look like the following:
//
//  xxxxxxxxxxbbbbbbbbbb
//  xxxxxxxxxxbbbbbbbbbb
//  xxxxxxxxxxbbbbbbbbbb
//  xxxxxxxxxxbbbbbbbbbb
//  xxxxxxxxxxrrrrrrrrrr
//  xxxxxxxxxxrrrrrrrrrr
//  xxxxxxxxxxgggggggggg
//  xxxxxxxxxxgggggggggg
//
//
// Where x refers to empty display pixels.
//       b refers to blue pixels covered by the parent view's bounds.
//       r refers to red pixels covered by the first child of the parent view.
//       g refers to green pixels covered by the second child of the parent view.
TEST_F(FlatlandPixelTestBase, TranslateInheritsFromParent) {
  // Draw the first rectangle in the top right quadrant.
  const fuc::ContentId kFilledRectId1 = {get_next_resource_id()};
  const fuc::TransformId kTransformId1 = {get_next_resource_id()};

  root_flatland_->CreateFilledRect(kFilledRectId1);
  root_flatland_->SetSolidFill(kFilledRectId1, GetColorInFloat(utils::kBlue),
                               {display_width_ / 2, display_height_ / 2});

  // Associate the rect with a transform.
  root_flatland_->CreateTransform(kTransformId1);
  root_flatland_->SetContent(kTransformId1, kFilledRectId1);
  root_flatland_->SetTranslation(kTransformId1, {static_cast<int32_t>(display_width_ / 2), 0});

  // Attach the transform to the view.
  root_flatland_->AddChild(kRootTransform, kTransformId1);

  // Draw the second rectangle which should be removed from the view, after ReplaceChildren
  // removes it's child-parent connection.
  const fuc::ContentId kFilledRectId2 = {get_next_resource_id()};
  const fuc::TransformId kTransformId2 = {get_next_resource_id()};

  root_flatland_->CreateFilledRect(kFilledRectId2);
  root_flatland_->SetSolidFill(kFilledRectId2, GetColorInFloat(utils::kMagenta),
                               {display_width_ / 2, display_height_ / 2});

  // Associate the rect with a transform.
  root_flatland_->CreateTransform(kTransformId2);
  root_flatland_->SetContent(kTransformId2, kFilledRectId2);
  root_flatland_->SetTranslation(kTransformId2, {0, static_cast<int32_t>(display_height_ / 2)});

  // Add the |kTransformId2| as the child of |kTransformId1| temporarily, but expect that
  // ReplaceChildren undoes this.
  root_flatland_->AddChild(kTransformId1, kTransformId2);
  BlockingPresent(this, root_flatland_);

  // Draw the first child rectangle which should appear in the top half of the bottom right
  // quadrant.
  const fuc::ContentId kFilledChildRectId1 = {get_next_resource_id()};
  const fuc::TransformId kChildTransformId1 = {get_next_resource_id()};

  root_flatland_->CreateFilledRect(kFilledChildRectId1);
  root_flatland_->SetSolidFill(kFilledChildRectId1, GetColorInFloat(utils::kRed),
                               {display_width_ / 2, display_height_ / 4});

  // Associate the rect with a transform.
  root_flatland_->CreateTransform(kChildTransformId1);
  root_flatland_->SetContent(kChildTransformId1, kFilledChildRectId1);
  root_flatland_->SetTranslation(kChildTransformId1,
                                 {0, static_cast<int32_t>(display_height_ / 2)});

  // Draw the second child rectangle which should appear in the bottom half of the bottom right
  // quadrant.
  const fuc::ContentId kFilledChildRectId2 = {get_next_resource_id()};
  const fuc::TransformId kChildTransformId2 = {get_next_resource_id()};

  root_flatland_->CreateFilledRect(kFilledChildRectId2);
  root_flatland_->SetSolidFill(kFilledChildRectId2, GetColorInFloat(utils::kGreen),
                               {display_width_ / 2, display_height_ / 4});

  // Associate the rect with a transform.
  root_flatland_->CreateTransform(kChildTransformId2);
  root_flatland_->SetContent(kChildTransformId2, kFilledChildRectId2);
  root_flatland_->SetTranslation(kChildTransformId2,
                                 {0, static_cast<int32_t>(3 * display_height_ / 4)});

  // Add |kChildTransformId1| and |kChildTransformId2| as children of |kTransformId1| by calling
  // ReplaceChildren, which also should remove any previous children of |kTransformId1|.
  root_flatland_->ReplaceChildren(kTransformId1, {kChildTransformId1, kChildTransformId2});
  BlockingPresent(this, root_flatland_);

  const utils::Pixel default_color(0, 0, 0, 0);

  auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_);

  EXPECT_EQ(screenshot.GetPixelAt(0, 0), default_color);

  // Top left corner of the first rectangle drawn aka the parent transform.
  EXPECT_EQ(screenshot.GetPixelAt(display_width_ / 2, 0), utils::kBlue);

  // Top left corner of the first child of the parent transform.
  EXPECT_EQ(screenshot.GetPixelAt(display_width_ / 2, display_height_ / 2), utils::kRed);

  // Top left corner of the second child of the parent transform.
  EXPECT_EQ(screenshot.GetPixelAt(display_width_ / 2, 3 * display_height_ / 4), utils::kGreen);

  const auto num_pixels = display_width_ * display_height_;

  auto histogram = screenshot.Histogram();

  EXPECT_EQ(histogram[default_color], num_pixels / 2);
  EXPECT_EQ(histogram[utils::kBlue], num_pixels / 4);
  // Expect |kTransformId2| was removed after ReplaceChildren, so we expect no matching pixels.
  EXPECT_EQ(histogram[utils::kMagenta], 0);
  EXPECT_EQ(histogram[utils::kRed], num_pixels / 8);
  EXPECT_EQ(histogram[utils::kGreen], num_pixels / 8);
}

// This test zooms the entire content by a factor of 2 and verifies that only the top left quadrant
// is shown.
// Before zoom:-
// ______________DISPLAY______________
// |                |                |
// |     BLACK      |        RED     |
// |                |                |
// |________________|________________|
// |                |                |
// |                |                |
// |      BLUE      |     MAGENTA    |
// |________________|________________|
//
// After zoom:-
// ______________DISPLAY______________
// |                                 |
// |                                 |
// |                                 |
// |             BLACK               |
// |                                 |
// |                                 |
// |                                 |
// |_________________________________|
//
// The remaining rectangles get clipped out because they fall outside the view bounds.
TEST_F(FlatlandPixelTestBase, ScaleTest) {
  const uint32_t view_width = display_width_;
  const uint32_t view_height = display_height_;

  const uint32_t pane_width =
      static_cast<uint32_t>(std::ceil(static_cast<float>(view_width) / 2.f));

  const uint32_t pane_height =
      static_cast<uint32_t>(std::ceil(static_cast<float>(view_height) / 2.f));

  // Draw the rectangles in the quadrants.
  for (uint32_t i = 0; i < 2; i++) {
    for (uint32_t j = 0; j < 2; j++) {
      utils::Pixel color(static_cast<uint8_t>(j * 255), 0, static_cast<uint8_t>(i * 255), 255);
      DrawRectangle(root_flatland_, pane_width, pane_height, i * pane_width, j * pane_height,
                    color);
    }
  }

  // Set a scale factor for 2.
  root_flatland_->SetScale(kRootTransform, {2, 2});
  BlockingPresent(this, root_flatland_);

  const auto num_pixels = display_width_ * display_height_;
  auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_);

  // Only the top left quadrant is shown on the screen as the rest of the quadrant are clipped.
  EXPECT_EQ(screenshot.GetPixelAt(0, 0), utils::kBlack);
  EXPECT_EQ(screenshot.GetPixelAt(0, display_height_ - 1), utils::kBlack);
  EXPECT_EQ(screenshot.GetPixelAt(display_width_ - 1, 0), utils::kBlack);
  EXPECT_EQ(screenshot.GetPixelAt(display_width_ - 1, display_height_ - 1), utils::kBlack);

  auto histogram = screenshot.Histogram();
  EXPECT_EQ(histogram[utils::kBlack], num_pixels);
  EXPECT_EQ(histogram[utils::kBlue], 0u);
  EXPECT_EQ(histogram[utils::kRed], 0u);
  EXPECT_EQ(histogram[utils::kMagenta], 0u);
}

// This test ensures that detaching a viewport ceases rendering the view.
TEST_F(FlatlandPixelTestBase, ViewportDetach) {
  fuc::FlatlandPtr child;
  child = ConnectAsyncIntoRealm<fuc::Flatland>();

  // Create the child view.
  auto [view_creation_token, viewport_creation_token] = scenic::ViewCreationTokenPair::New();
  fidl::InterfacePtr<fuc::ParentViewportWatcher> parent_viewport_watcher;
  child->CreateView2(std::move(view_creation_token), scenic::NewViewIdentityOnCreation(), {},
                     parent_viewport_watcher.NewRequest());
  BlockingPresent(this, child);

  // Connect the child view to the root view.
  fuc::TransformId viewport_transform = {get_next_resource_id()};
  fuc::ContentId viewport_content = {get_next_resource_id()};
  root_flatland_->CreateTransform(viewport_transform);
  fidl::InterfacePtr<fuc::ChildViewWatcher> child_view_watcher;
  fuc::ViewportProperties properties;
  properties.set_logical_size({display_width_, display_height_});
  root_flatland_->CreateViewport(viewport_content, std::move(viewport_creation_token),
                                 std::move(properties), child_view_watcher.NewRequest());
  root_flatland_->SetContent(viewport_transform, viewport_content);
  root_flatland_->AddChild(kRootTransform, viewport_transform);

  BlockingPresent(this, root_flatland_);

  // Child view draws a solid filled rectangle.
  child->CreateTransform(kRootTransform);
  child->SetRootTransform(kRootTransform);
  DrawRectangle(child, display_width_, display_height_, 0, 0, utils::kBlue);
  BlockingPresent(this, child);

  const auto num_pixels = display_width_ * display_height_;
  // The screenshot taken should reflect the content drawn by the child view.
  {
    auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_);
    auto histogram = screenshot.Histogram();
    EXPECT_EQ(histogram[utils::kBlue], num_pixels);
  }

  // Root view releases the viewport.
  root_flatland_->ReleaseViewport(viewport_content, [](auto token) {});
  BlockingPresent(this, root_flatland_);

  // The screenshot taken should not reflect the content drawn by the child view as its viewport was
  // released.
  {
    auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_);
    auto histogram = screenshot.Histogram();
    EXPECT_EQ(histogram[utils::kBlue], 0u);
  }
}

// This test ensures that |fuchsia.ui.composition.ViewportProperties.inset| is only used
// as hints for clients, and they won't affect rendering of views in Scenic.
TEST_F(FlatlandPixelTestBase, InsetNotEnforced) {
  fuc::FlatlandPtr child;
  child = ConnectAsyncIntoRealm<fuc::Flatland>();

  // Create the child view.
  auto [view_creation_token, viewport_creation_token] = scenic::ViewCreationTokenPair::New();
  fidl::InterfacePtr<fuc::ParentViewportWatcher> parent_viewport_watcher;
  child->CreateView2(std::move(view_creation_token), scenic::NewViewIdentityOnCreation(), {},
                     parent_viewport_watcher.NewRequest());
  BlockingPresent(this, child);

  // Connect the child view to the root view.
  fuc::TransformId viewport_transform = {get_next_resource_id()};
  fuc::ContentId viewport_content = {get_next_resource_id()};
  root_flatland_->CreateTransform(viewport_transform);
  fidl::InterfacePtr<fuc::ChildViewWatcher> child_view_watcher;
  fuc::ViewportProperties properties;
  properties.set_logical_size({display_width_, display_height_});

  // We set non-zero |inset|. These properties should work only as hints, but not affect actual
  // rendered views.
  properties.set_inset({
      .top = static_cast<int32_t>(display_height_) / 4,
      .right = static_cast<int32_t>(display_width_) / 4,
      .bottom = static_cast<int32_t>(display_height_) / 4,
      .left = static_cast<int32_t>(display_width_) / 4,
  });

  root_flatland_->CreateViewport(viewport_content, std::move(viewport_creation_token),
                                 std::move(properties), child_view_watcher.NewRequest());
  root_flatland_->SetContent(viewport_transform, viewport_content);
  root_flatland_->AddChild(kRootTransform, viewport_transform);

  BlockingPresent(this, root_flatland_);

  // Child view draws a solid filled rectangle.
  child->CreateTransform(kRootTransform);
  child->SetRootTransform(kRootTransform);
  DrawRectangle(child, display_width_, display_height_, 0, 0, utils::kBlue);
  BlockingPresent(this, child);

  // The size of the solid filled rectangle exceeds the child view's bounding box with
  // inset. Since inset properties are only hints, they should not affect the
  // rendered size of the rectangle.
  const auto num_pixels = display_width_ * display_height_;
  auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_);
  auto histogram = screenshot.Histogram();
  EXPECT_EQ(histogram[utils::kBlue], num_pixels);
}

}  // namespace integration_tests
