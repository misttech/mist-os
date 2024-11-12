// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/sysmem/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_creation_tokens.h>
#include <lib/ui/scenic/cpp/view_identity.h>

#include <cstdint>

#include <zxtest/zxtest.h>

#include "fuchsia/ui/test/context/cpp/fidl.h"
#include "src/ui/scenic/lib/allocation/buffer_collection_import_export_tokens.h"
#include "src/ui/scenic/lib/flatland/buffers/util.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/lib/utils/pixel.h"
#include "src/ui/scenic/tests/utils/blocking_present.h"
#include "src/ui/scenic/tests/utils/scenic_ctf_test_base.h"
#include "src/ui/scenic/tests/utils/utils.h"
#include "src/ui/testing/util/screenshot_helper.h"

namespace integration_tests {

using fuchsia::ui::composition::ChildViewWatcher;
using fuchsia::ui::composition::ContentId;
using fuchsia::ui::composition::FlatlandPtr;
using fuchsia::ui::composition::ParentViewportWatcher;
using fuchsia::ui::composition::TransformId;

class CpuRendererIntegrationTest : public ScenicCtfTest {
 public:
  void SetUp() override {
    ScenicCtfTest::SetUp();

    LocalServiceDirectory()->Connect(sysmem_allocator_.NewRequest());

    flatland_display_ = ConnectSyncIntoRealm<fuchsia::ui::composition::FlatlandDisplay>();
    flatland_allocator_ = ConnectSyncIntoRealm<fuchsia::ui::composition::Allocator>();
    root_flatland_ = ConnectAsyncIntoRealm<fuchsia::ui::composition::Flatland>();

    // Attach |root_flatland_| as the only Flatland under |flatland_display_|.
    auto [child_token, parent_token] = scenic::ViewCreationTokenPair::New();
    fidl::InterfacePtr<ChildViewWatcher> child_view_watcher;
    flatland_display_->SetContent(std::move(parent_token), child_view_watcher.NewRequest());
    fidl::InterfacePtr<ParentViewportWatcher> parent_viewport_watcher;
    root_flatland_->CreateView2(std::move(child_token), scenic::NewViewIdentityOnCreation(), {},
                                parent_viewport_watcher.NewRequest());

    // Get the display's width and height. Since there is no Present in FlatlandDisplay, receiving
    // this callback ensures that all |flatland_display_| calls are processed.
    std::optional<fuchsia::ui::composition::LayoutInfo> info;
    parent_viewport_watcher->GetLayout([&info](auto result) { info = std::move(result); });
    RunLoopUntil([&info] { return info.has_value(); });
    display_width_ = info->logical_size().width;
    display_height_ = info->logical_size().height;

    screenshotter_ = ConnectSyncIntoRealm<fuchsia::ui::composition::Screenshot>();
  }

  fuchsia::ui::test::context::RendererType Renderer() const override {
    return fuchsia::ui::test::context::RendererType::CPU;
  }

  uint64_t DisplayRotation() const override { return 0; }

  bool DisplayComposition() const override { return false; }

 protected:
  fuchsia::sysmem2::BufferCollectionInfo SetConstraintsAndAllocateBuffer(
      fuchsia::sysmem2::BufferCollectionTokenSyncPtr token, fuchsia::images2::PixelFormat format) {
    fuchsia::sysmem2::BufferCollectionSyncPtr buffer_collection;
    fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
    bind_shared_request.set_token(std::move(token));
    bind_shared_request.set_buffer_collection_request(buffer_collection.NewRequest());
    auto status = sysmem_allocator_->BindSharedCollection(std::move(bind_shared_request));
    FX_CHECK(status == ZX_OK);
    fuchsia::sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
    auto& constraints = *set_constraints_request.mutable_constraints();
    constraints.mutable_usage()->set_cpu(fuchsia::sysmem2::CPU_USAGE_WRITE);
    constraints.set_min_buffer_count(1);
    auto& image_constraints = constraints.mutable_image_format_constraints()->emplace_back();
    image_constraints.set_pixel_format(format);
    image_constraints.mutable_color_spaces()->emplace_back(fuchsia::images2::ColorSpace::SRGB);
    image_constraints.set_required_min_size(
        fuchsia::math::SizeU{.width = display_width_, .height = display_height_});
    image_constraints.set_required_max_size(
        fuchsia::math::SizeU{.width = display_width_, .height = display_height_});
    status = buffer_collection->SetConstraints(std::move(set_constraints_request));
    FX_CHECK(status == ZX_OK);
    fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_result;
    status = buffer_collection->WaitForAllBuffersAllocated(&wait_result);
    FX_CHECK(status == ZX_OK);
    FX_CHECK(!wait_result.is_framework_err());
    FX_CHECK(!wait_result.is_err());
    FX_CHECK(wait_result.is_response());
    auto buffer_collection_info =
        std::move(*wait_result.response().mutable_buffer_collection_info());
    EXPECT_EQ(constraints.min_buffer_count(), buffer_collection_info.buffers().size());
    FX_CHECK(buffer_collection->Release() == ZX_OK);
    return buffer_collection_info;
  }

  const TransformId kRootTransform{.value = 1};
  uint32_t display_width_ = 0;
  uint32_t display_height_ = 0;

  fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator_;
  fuchsia::ui::composition::AllocatorSyncPtr flatland_allocator_;
  FlatlandPtr root_flatland_;
  fuchsia::ui::composition::ScreenshotSyncPtr screenshotter_;

 private:
  fuchsia::ui::composition::FlatlandDisplaySyncPtr flatland_display_;
};

TEST_F(CpuRendererIntegrationTest, RenderSmokeTest) {
  auto [local_token, scenic_token] = utils::CreateSysmemTokens(sysmem_allocator_.get());

  // Send one token to Flatland Allocator.
  allocation::BufferCollectionImportExportTokens bc_tokens =
      allocation::BufferCollectionImportExportTokens::New();
  fuchsia::ui::composition::RegisterBufferCollectionArgs rbc_args = {};
  rbc_args.set_export_token(std::move(bc_tokens.export_token));
  rbc_args.set_buffer_collection_token2(std::move(scenic_token));
  fuchsia::ui::composition::Allocator_RegisterBufferCollection_Result result;
  flatland_allocator_->RegisterBufferCollection(std::move(rbc_args), &result);
  ASSERT_FALSE(result.is_err());

  // Use the local token to allocate a protected buffer. CpuRenderer sets
  // constraint to complete the allocation.
  SetConstraintsAndAllocateBuffer(std::move(local_token), fuchsia::images2::PixelFormat::B8G8R8A8);

  // Create the image in the Flatland instance.
  fuchsia::ui::composition::ImageProperties image_properties = {};
  image_properties.set_size({display_width_, display_height_});
  const ContentId kImageContentId{.value = 1};
  root_flatland_->CreateImage(kImageContentId, std::move(bc_tokens.import_token),
                              /*buffer_collection_index=*/0, std::move(image_properties));
  BlockingPresent(this, root_flatland_);

  // Present the created Image. Verify that render happened without any errors.
  root_flatland_->CreateTransform(kRootTransform);
  root_flatland_->SetRootTransform(kRootTransform);
  root_flatland_->SetContent(kRootTransform, kImageContentId);
  fuchsia::ui::composition::PresentArgs args;
  args.set_release_fences(utils::CreateEventArray(1));
  auto release_fence_copy = utils::CopyEvent(args.release_fences()[0]);
  BlockingPresent(this, root_flatland_, std::move(args));

  // Ensure that release fence for the previous frame is singalled after a Present.
  root_flatland_->Clear();
  BlockingPresent(this, root_flatland_);
  EXPECT_TRUE(utils::IsEventSignalled(release_fence_copy, ZX_EVENT_SIGNALED));
}

class CpuRendererIntegrationTestWithFormat
    : public CpuRendererIntegrationTest,
      public zxtest::WithParamInterface<fuchsia::images2::PixelFormat> {};

TEST_P(CpuRendererIntegrationTestWithFormat, RendersImage) {
  auto [local_token, scenic_token] = utils::CreateSysmemTokens(sysmem_allocator_.get());

  // Send one token to Flatland Allocator.
  allocation::BufferCollectionImportExportTokens bc_tokens =
      allocation::BufferCollectionImportExportTokens::New();
  fuchsia::ui::composition::RegisterBufferCollectionArgs rbc_args = {};
  rbc_args.set_export_token(std::move(bc_tokens.export_token));
  rbc_args.set_buffer_collection_token2(std::move(scenic_token));
  fuchsia::ui::composition::Allocator_RegisterBufferCollection_Result result;
  flatland_allocator_->RegisterBufferCollection(std::move(rbc_args), &result);
  ASSERT_FALSE(result.is_err());

  // Use the local token to allocate a protected buffer.
  auto info = SetConstraintsAndAllocateBuffer(std::move(local_token), GetParam());

  const uint32_t bytes_per_pixel = GetParam() == fuchsia::images2::PixelFormat::B8G8R8A8 ? 4 : 2;

  // Write the pixel values to the VMO.
  const uint32_t num_pixels = display_width_ * display_height_;

  const utils::Pixel color = utils::kBlue;
  std::vector<uint8_t> pixel_data = color.ToFormat(GetParam());
  flatland::MapHostPointer(info.buffers()[0].vmo(), flatland::HostPointerAccessMode::kWriteOnly,
                           [&num_pixels, &color, &pixel_data, &info, bytes_per_pixel](
                               uint8_t* vmo_ptr, uint32_t num_bytes) {
                             ASSERT_EQ(num_bytes, num_pixels * bytes_per_pixel);
                             ASSERT_EQ(num_bytes, info.settings().buffer_settings().size_bytes());

                             if (info.settings().image_format_constraints().pixel_format() ==
                                 fuchsia::images2::PixelFormat::B8G8R8A8) {
                               for (uint32_t i = 0; i < num_bytes; i += bytes_per_pixel) {
                                 // For BGRA32 pixel format, the first and the third byte in the
                                 // pixel corresponds to the blue and the red channel respectively.
                                 vmo_ptr[i] = color.blue;
                                 vmo_ptr[i + 1] = color.green;
                                 vmo_ptr[i + 2] = color.red;
                                 vmo_ptr[i + 3] = color.alpha;
                               }
                             } else {
                               ASSERT_EQ(info.settings().image_format_constraints().pixel_format(),
                                         fuchsia::images2::PixelFormat::R5G6B5);
                               for (uint32_t i = 0; i < num_bytes; i += 2) {
                                 vmo_ptr[i] = pixel_data[0];
                                 vmo_ptr[i + 1] = pixel_data[1];
                               }
                             }

                             if (info.settings().buffer_settings().coherency_domain() ==
                                 fuchsia::sysmem2::CoherencyDomain::RAM) {
                               EXPECT_EQ(ZX_OK,
                                         info.buffers()[0].vmo().op_range(ZX_VMO_OP_CACHE_CLEAN, 0,
                                                                          num_bytes, nullptr, 0));
                             }
                           });

  // Create the image in the Flatland instance.
  fuchsia::ui::composition::ImageProperties image_properties = {};
  image_properties.set_size({display_width_, display_height_});
  const fuchsia::ui::composition::ContentId kImageContentId{.value = 1};

  root_flatland_->CreateImage(kImageContentId, std::move(bc_tokens.import_token), 0,
                              std::move(image_properties));

  // Present the created Image.
  root_flatland_->CreateTransform(kRootTransform);
  root_flatland_->SetRootTransform(kRootTransform);
  root_flatland_->SetContent(kRootTransform, kImageContentId);
  BlockingPresent(this, root_flatland_);

  auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_);
  auto histogram = screenshot.Histogram();

  EXPECT_EQ(histogram[color], num_pixels);
}

INSTANTIATE_TEST_SUITE_P(Formats, CpuRendererIntegrationTestWithFormat,
                         zxtest::Values(fuchsia::images2::PixelFormat::B8G8R8A8,
                                        fuchsia::images2::PixelFormat::R5G6B5));

TEST_F(CpuRendererIntegrationTest, RendersSolidFill) {
  // Create a red solid fill.
  const ContentId kFilledRectContentId{.value = 1};
  root_flatland_->CreateFilledRect(kFilledRectContentId);
  const fuchsia::ui::composition::ColorRgba kRed{
      .red = 1.f, .green = 0.f, .blue = 0.f, .alpha = 1.f};
  root_flatland_->SetSolidFill(kFilledRectContentId, kRed,
                               {.width = display_width_, .height = display_height_});

  // Present the created solid fill.
  root_flatland_->CreateTransform(kRootTransform);
  root_flatland_->SetRootTransform(kRootTransform);
  root_flatland_->SetContent(kRootTransform, kFilledRectContentId);
  BlockingPresent(this, root_flatland_);

  auto screenshot = TakeScreenshot(screenshotter_, display_width_, display_height_);
  auto histogram = screenshot.Histogram();

  EXPECT_EQ(histogram[utils::kRed], display_width_ * display_height_);
}

}  // namespace integration_tests
