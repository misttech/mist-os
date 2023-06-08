// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/fake-display.h"

#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/image-format/image_format.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/errors.h>
#include <zircon/rights.h>

#include <cstddef>
#include <limits>
#include <memory>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "fidl/fuchsia.sysmem/cpp/natural_types.h"
#include "src/devices/sysmem/drivers/sysmem/device.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/drivers/fake/fake-display-stack.h"
#include "src/graphics/display/drivers/fake/sysmem-device-wrapper.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/lib/testing/predicates/status.h"

namespace fake_display {

namespace {

class FakeDisplayTest : public testing::Test {
 public:
  FakeDisplayTest() = default;

  void SetUp() override {
    std::shared_ptr<zx_device> mock_root = MockDevice::FakeRootParent();
    auto sysmem = std::make_unique<display::GenericSysmemDeviceWrapper<sysmem_driver::Device>>(
        mock_root.get());
    static constexpr FakeDisplayDeviceConfig kDeviceConfig = {
        .manual_vsync_trigger = true,
    };
    tree_ = std::make_unique<display::FakeDisplayStack>(std::move(mock_root), std::move(sysmem),
                                                        kDeviceConfig);
  }

  void TearDown() override {
    tree_->AsyncShutdown();
    tree_.reset();
  }

  fake_display::FakeDisplay* display() { return tree_->display(); }

  const fidl::WireSyncClient<fuchsia_sysmem2::DriverConnector>& sysmem_fidl() {
    return tree_->sysmem_client();
  }

 private:
  std::unique_ptr<display::FakeDisplayStack> tree_;
};

class FakeDisplaySysmemTest : public FakeDisplayTest {
 public:
  struct BufferCollectionAndToken {
    fidl::WireSyncClient<fuchsia_sysmem::BufferCollection> collection_client;
    fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken> token;
  };

  FakeDisplaySysmemTest() = default;
  ~FakeDisplaySysmemTest() override = default;

  zx::result<BufferCollectionAndToken> CreateBufferCollection() {
    zx::result<fidl::Endpoints<fuchsia_sysmem::BufferCollectionToken>> token_endpoints =
        fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();

    EXPECT_OK(token_endpoints.status_value());
    if (!token_endpoints.is_ok()) {
      return token_endpoints.take_error();
    }

    auto& [token_client, token_server] = token_endpoints.value();
    fidl::Status allocate_token_status = sysmem_->AllocateSharedCollection(std::move(token_server));
    EXPECT_OK(allocate_token_status.status());
    if (!allocate_token_status.ok()) {
      return zx::error(allocate_token_status.status());
    }

    fidl::Status sync_status = fidl::WireCall(token_client)->Sync();
    EXPECT_OK(sync_status.status());
    if (!sync_status.ok()) {
      return zx::error(sync_status.status());
    }

    // At least one sysmem participant should specify buffer memory constraints.
    // The driver may not specify buffer memory constraints, so the test should
    // always provide one for sysmem through `buffer_collection_` client.
    //
    // Here we duplicate the token to set buffer collection constraints in
    // the test.
    std::vector<zx_rights_t> rights = {ZX_RIGHT_SAME_RIGHTS};
    fidl::WireResult duplicate_result =
        fidl::WireCall(token_client)
            ->DuplicateSync(fidl::VectorView<zx_rights_t>::FromExternal(rights));
    EXPECT_OK(duplicate_result.status());
    if (!duplicate_result.ok()) {
      return zx::error(duplicate_result.status());
    }

    auto& duplicate_value = duplicate_result.value();
    EXPECT_EQ(duplicate_value.tokens.count(), 1u);
    if (duplicate_value.tokens.count() != 1u) {
      return zx::error(ZX_ERR_BAD_STATE);
    }

    // Bind duplicated token to BufferCollection client.
    zx::result<fidl::Endpoints<fuchsia_sysmem::BufferCollection>> collection_endpoints =
        fidl::CreateEndpoints<fuchsia_sysmem::BufferCollection>();
    EXPECT_OK(collection_endpoints.status_value());
    if (!collection_endpoints.is_ok()) {
      return collection_endpoints.take_error();
    }

    auto& [collection_client, collection_server] = collection_endpoints.value();
    fidl::Status bind_status = sysmem_->BindSharedCollection(std::move(duplicate_value.tokens[0]),
                                                             std::move(collection_server));
    EXPECT_OK(bind_status.status());
    if (!bind_status.ok()) {
      return zx::error(bind_status.status());
    }

    return zx::ok(BufferCollectionAndToken{
        .collection_client = fidl::WireSyncClient(std::move(collection_client)),
        .token = std::move(token_client),
    });
  }

  void SetUp() override {
    FakeDisplayTest::SetUp();

    zx::result<fidl::Endpoints<fuchsia_sysmem::Allocator>> sysmem_endpoints =
        fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
    ASSERT_OK(sysmem_endpoints.status_value());
    auto& [sysmem_client, sysmem_server] = sysmem_endpoints.value();
    EXPECT_TRUE(sysmem_fidl()->ConnectV1(std::move(sysmem_server)).ok());
    sysmem_ = fidl::WireSyncClient(std::move(sysmem_client));
  }

  void TearDown() override {
    sysmem_ = {};
    FakeDisplayTest::TearDown();
  }

  const fidl::WireSyncClient<fuchsia_sysmem::Allocator>& sysmem() const { return sysmem_; }

 private:
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_;
};

// A completion semaphore indicating the display capture is completed.
class DisplayCaptureCompletion {
 public:
  // Tests can import the capture interface protocol to set up the callback to
  // trigger the semaphore.
  display_capture_interface_protocol_t GetDisplayCaptureInterfaceProtocol() {
    static constexpr display_capture_interface_protocol_ops_t kDisplayCaptureInterfaceProtocolOps =
        {
            .on_capture_complete =
                [](void* ctx) {
                  reinterpret_cast<DisplayCaptureCompletion*>(ctx)->OnCaptureComplete();
                },
        };
    return display_capture_interface_protocol_t{
        .ops = &kDisplayCaptureInterfaceProtocolOps,
        .ctx = this,
    };
  }

  libsync::Completion& completed() { return completed_; }

 private:
  void OnCaptureComplete() { completed().Signal(); }
  libsync::Completion completed_;
};

// Creates a BufferCollectionConstraints that tests can use to configure their
// own BufferCollections to allocate buffers.
//
// It provides sysmem Constraints that request exactly 1 CPU-readable and
// writable buffer with size of at least `min_size_bytes` bytes and can hold
// an image of pixel format `pixel_format`.
//
// As we require the image to be both readable and writable, the constraints
// will work for both simple (scanout) images and capture images.
fuchsia_sysmem::wire::BufferCollectionConstraints CreateImageConstraints(
    uint32_t min_size_bytes, fuchsia_sysmem::wire::PixelFormat pixel_format) {
  return fuchsia_sysmem::wire::BufferCollectionConstraints{
      .usage =
          {
              .cpu = fuchsia_sysmem::wire::kCpuUsageRead | fuchsia_sysmem::wire::kCpuUsageWrite,
          },
      .min_buffer_count = 1,
      .max_buffer_count = 1,
      .has_buffer_memory_constraints = true,
      .buffer_memory_constraints =
          {
              .min_size_bytes = min_size_bytes,
              .max_size_bytes = std::numeric_limits<uint32_t>::max(),
              // The test cases need direct CPU access to the buffers and we
              // don't enforce cache flushing, so we should narrow down the
              // allowed sysmem heaps to the heaps supporting CPU domain and
              // reject all the other heaps.
              .ram_domain_supported = false,
              .cpu_domain_supported = true,
              .inaccessible_domain_supported = false,
          },
      // fake-display driver doesn't add extra image format constraints when
      // SetBufferCollectionConstraints() is called. To make sure we allocate an
      // image buffer, we add constraints here.
      .image_format_constraints_count = 1,
      .image_format_constraints =
          {
              fuchsia_sysmem::wire::ImageFormatConstraints{
                  .pixel_format = pixel_format,
                  .color_spaces_count = 1,
                  .color_space =
                      {
                          fuchsia_sysmem::wire::ColorSpace{
                              .type = fuchsia_sysmem::ColorSpaceType::kSrgb,
                          },
                      },
              },
          },
  };
}

// Creates a primary layer config for an opaque layer that holds the `image`
// on the top-left corner of the screen without any scaling.
layer_t CreatePrimaryLayerConfig(const image_t& image) {
  return layer_t{
      .type = LAYER_TYPE_PRIMARY,
      .z_index = 0,
      .cfg =
          {
              .primary =
                  {
                      .image = image,
                      .alpha_mode = ALPHA_DISABLE,
                      .alpha_layer_val = 1.0,
                      .transform_mode = FRAME_TRANSFORM_IDENTITY,
                      .src_frame =
                          {
                              .x_pos = 0,
                              .y_pos = 0,
                              .width = image.width,
                              .height = image.height,
                          },
                      .dest_frame =
                          {
                              .x_pos = 0,
                              .y_pos = 0,
                              .width = image.width,
                              .height = image.height,
                          },
                  },
          },
  };
}

std::pair<zx::vmo, fuchsia_sysmem::wire::SingleBufferSettings> GetAllocatedBufferAndSettings(
    const fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>& client) {
  auto wait_result = client->WaitForBuffersAllocated();
  ZX_ASSERT_MSG(wait_result.ok(), "WaitForBuffersAllocated() FIDL call failed: %s",
                wait_result.status_string());
  ZX_ASSERT_MSG(wait_result.value().status == ZX_OK,
                "WaitForBuffersAllocated() responds with error: %s",
                zx_status_get_string(wait_result.value().status));
  auto& buffer_collection_info = wait_result.value().buffer_collection_info;
  ZX_ASSERT_MSG(buffer_collection_info.buffer_count == 1u,
                "Incorrect number of buffers allocated: actual %u, expected 1",
                buffer_collection_info.buffer_count);
  return {std::move(buffer_collection_info.buffers[0].vmo), buffer_collection_info.settings};
}

void FillImageWithColor(cpp20::span<uint8_t> image_buffer, const std::vector<uint8_t>& color_raw,
                        int width, int height, uint32_t bytes_per_row_divisor) {
  size_t bytes_per_pixel = color_raw.size();
  size_t row_stride_bytes = fbl::round_up(width * bytes_per_pixel, bytes_per_row_divisor);

  for (int row = 0; row < height; ++row) {
    auto row_buffer = image_buffer.subspan(row * row_stride_bytes, row_stride_bytes);
    auto it = row_buffer.begin();
    for (int col = 0; col < width; ++col) {
      it = std::copy(color_raw.begin(), color_raw.end(), it);
    }
  }
}

TEST_F(FakeDisplaySysmemTest, ImportBufferCollection) {
  zx::result<BufferCollectionAndToken> new_buffer_collection_result = CreateBufferCollection();
  ASSERT_OK(new_buffer_collection_result.status_value());
  auto [collection_client, token] = std::move(new_buffer_collection_result.value());

  // Test ImportBufferCollection().
  constexpr uint64_t kValidBufferCollectionId = 1u;
  EXPECT_OK(display()->DisplayControllerImplImportBufferCollection(kValidBufferCollectionId,
                                                                   token.TakeChannel()));

  // `collection_id` must be unused.
  zx::result<fidl::Endpoints<fuchsia_sysmem::BufferCollectionToken>> another_token_endpoints =
      fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_OK(another_token_endpoints.status_value());
  EXPECT_EQ(display()->DisplayControllerImplImportBufferCollection(
                kValidBufferCollectionId, another_token_endpoints->client.TakeChannel()),
            ZX_ERR_ALREADY_EXISTS);

  // Driver sets BufferCollection buffer memory constraints.
  const image_t kDefaultConfig = {
      .width = 1024,
      .height = 768,
      .type = IMAGE_TYPE_SIMPLE,
      .handle = 0,
  };
  EXPECT_OK(display()->DisplayControllerImplSetBufferCollectionConstraints(
      &kDefaultConfig, kValidBufferCollectionId));

  // Set BufferCollection buffer memory constraints.
  fidl::Status set_constraints_status = collection_client->SetConstraints(
      /* has_constraints= */ true,
      CreateImageConstraints(/*min_size_bytes=*/4096,
                             fuchsia_sysmem::wire::PixelFormat{
                                 .type = fuchsia_sysmem::PixelFormatType::kR8G8B8A8,
                                 .has_format_modifier = true,
                                 .format_modifier =
                                     {
                                         .value = fuchsia_sysmem::wire::kFormatModifierLinear,
                                     },
                             }));
  EXPECT_TRUE(set_constraints_status.ok());

  // Both the test-side client and the driver have  set the constraints.
  // The buffer should be allocated correctly in sysmem.
  EXPECT_TRUE(collection_client->WaitForBuffersAllocated().ok());

  // Test ReleaseBufferCollection().
  // TODO(fxbug.dev/128574): Consider adding RAII handles to release the
  // imported buffer collections.
  constexpr uint64_t kInvalidBufferCollectionId = 2u;
  EXPECT_EQ(display()->DisplayControllerImplReleaseBufferCollection(kInvalidBufferCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display()->DisplayControllerImplReleaseBufferCollection(kValidBufferCollectionId));
}

TEST_F(FakeDisplaySysmemTest, ImportImage) {
  zx::result<BufferCollectionAndToken> new_buffer_collection_result = CreateBufferCollection();
  ASSERT_OK(new_buffer_collection_result.status_value());
  auto [collection_client, token] = std::move(new_buffer_collection_result.value());

  constexpr uint64_t kBufferCollectionId = 1u;
  EXPECT_OK(display()->DisplayControllerImplImportBufferCollection(kBufferCollectionId,
                                                                   token.TakeChannel()));

  // Driver sets BufferCollection buffer memory constraints.
  const image_t kDefaultConfig = {
      .width = 1024,
      .height = 768,
      .type = IMAGE_TYPE_SIMPLE,
      .handle = 0,
  };
  const auto kPixelFormat = fuchsia_sysmem::wire::PixelFormat{
      .type = fuchsia_sysmem::PixelFormatType::kBgra32,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = fuchsia_sysmem::wire::kFormatModifierLinear,
          },
  };
  const uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(kPixelFormat);

  EXPECT_OK(display()->DisplayControllerImplSetBufferCollectionConstraints(&kDefaultConfig,
                                                                           kBufferCollectionId));

  // Set BufferCollection buffer memory constraints.
  fidl::Status set_constraints_status = collection_client->SetConstraints(
      /* has_constraints= */ true,
      CreateImageConstraints(
          /*min_size_bytes=*/kDefaultConfig.width * kDefaultConfig.height * bytes_per_pixel,
          kPixelFormat));
  EXPECT_TRUE(set_constraints_status.ok());

  // Both the test-side client and the driver have set the constraints.
  // The buffer should be allocated correctly in sysmem.
  EXPECT_TRUE(collection_client->WaitForBuffersAllocated().ok());

  // TODO(fxbug.dev/128571): Split all valid / invalid imports into separate
  // test cases.
  // Invalid import: Bad image type.
  image_t invalid_config = kDefaultConfig;
  invalid_config.type = IMAGE_TYPE_CAPTURE;
  EXPECT_EQ(display()->DisplayControllerImplImportImage(&invalid_config, kBufferCollectionId,
                                                        /*index=*/0),
            ZX_ERR_INVALID_ARGS);

  // Invalid import: Invalid collection ID.
  invalid_config = kDefaultConfig;
  constexpr uint64_t kInvalidBufferCollectionId = 100u;
  EXPECT_EQ(display()->DisplayControllerImplImportImage(&invalid_config, kInvalidBufferCollectionId,
                                                        /*index=*/0),
            ZX_ERR_NOT_FOUND);

  // Invalid import: Invalid buffer collection index.
  invalid_config = kDefaultConfig;
  constexpr uint64_t kInvalidBufferCollectionIndex = 100u;
  EXPECT_EQ(display()->DisplayControllerImplImportImage(&invalid_config, kBufferCollectionId,
                                                        kInvalidBufferCollectionIndex),
            ZX_ERR_OUT_OF_RANGE);

  // Valid import.
  image_t valid_config = kDefaultConfig;
  EXPECT_EQ(valid_config.handle, 0u);
  EXPECT_OK(display()->DisplayControllerImplImportImage(&valid_config, kBufferCollectionId,
                                                        /*index=*/0));
  EXPECT_NE(valid_config.handle, 0u);

  // Release the image.
  display()->DisplayControllerImplReleaseImage(&valid_config);

  EXPECT_OK(display()->DisplayControllerImplReleaseBufferCollection(kBufferCollectionId));
}

TEST_F(FakeDisplaySysmemTest, ImportImageForCapture) {
  zx::result<BufferCollectionAndToken> new_buffer_collection_result = CreateBufferCollection();
  ASSERT_OK(new_buffer_collection_result.status_value());
  auto [collection_client, token] = std::move(new_buffer_collection_result.value());

  constexpr uint64_t kBufferCollectionId = 1u;
  EXPECT_OK(display()->DisplayControllerImplImportBufferCollection(kBufferCollectionId,
                                                                   token.TakeChannel()));

  const auto kPixelFormat = fuchsia_sysmem::wire::PixelFormat{
      .type = fuchsia_sysmem::PixelFormatType::kBgra32,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = fuchsia_sysmem::wire::kFormatModifierLinear,
          },
  };

  constexpr uint32_t kDisplayWidth = 1280;
  constexpr uint32_t kDisplayHeight = 800;

  const image_t kCaptureConfig = {
      .width = kDisplayWidth,
      .height = kDisplayHeight,
      .type = IMAGE_TYPE_CAPTURE,
      .handle = 0,
  };
  EXPECT_OK(display()->DisplayControllerImplSetBufferCollectionConstraints(&kCaptureConfig,
                                                                           kBufferCollectionId));
  const uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(kPixelFormat);
  const uint32_t size_bytes = kDisplayWidth * kDisplayHeight * bytes_per_pixel;
  // Set BufferCollection buffer memory constraints.
  fidl::Status set_constraints_status = collection_client->SetConstraints(
      /* has_constraints= */ true, CreateImageConstraints(size_bytes, kPixelFormat));
  EXPECT_TRUE(set_constraints_status.ok());

  // Both the test-side client and the driver have set the constraints.
  // The buffer should be allocated correctly in sysmem.
  EXPECT_TRUE(collection_client->WaitForBuffersAllocated().ok());

  uint64_t out_capture_handle = INVALID_ID;

  // TODO(fxbug.dev/128571): Split all valid / invalid imports into separate
  // test cases.
  // Invalid import: Invalid collection ID.
  constexpr uint64_t kInvalidBufferCollectionId = 100u;
  EXPECT_EQ(display()->DisplayControllerImplImportImageForCapture(kInvalidBufferCollectionId,
                                                                  /*index=*/0, &out_capture_handle),
            ZX_ERR_NOT_FOUND);

  // Invalid import: Invalid buffer collection index.
  constexpr uint64_t kInvalidBufferCollectionIndex = 100u;
  EXPECT_EQ(display()->DisplayControllerImplImportImageForCapture(
                kBufferCollectionId, kInvalidBufferCollectionIndex, &out_capture_handle),
            ZX_ERR_OUT_OF_RANGE);

  // Valid import.
  EXPECT_OK(display()->DisplayControllerImplImportImageForCapture(kBufferCollectionId, /*index=*/0,
                                                                  &out_capture_handle));
  EXPECT_NE(out_capture_handle, INVALID_ID);

  // Release the image.
  // TODO(fxbug.dev/128574): Consider adding RAII handles to release the
  // imported images and buffer collections.
  display()->DisplayControllerImplReleaseCapture(out_capture_handle);

  EXPECT_OK(display()->DisplayControllerImplReleaseBufferCollection(kBufferCollectionId));
}

TEST_F(FakeDisplaySysmemTest, Capture) {
  zx::result<BufferCollectionAndToken> new_capture_buffer_collection_result =
      CreateBufferCollection();
  ASSERT_OK(new_capture_buffer_collection_result.status_value());
  auto [capture_collection_client, capture_token] =
      std::move(new_capture_buffer_collection_result.value());

  zx::result<BufferCollectionAndToken> new_framebuffer_buffer_collection_result =
      CreateBufferCollection();
  ASSERT_OK(new_framebuffer_buffer_collection_result.status_value());
  auto [framebuffer_collection_client, framebuffer_token] =
      std::move(new_framebuffer_buffer_collection_result.value());

  DisplayCaptureCompletion display_capture_completion = {};
  const display_capture_interface_protocol_t& capture_protocol =
      display_capture_completion.GetDisplayCaptureInterfaceProtocol();
  EXPECT_OK(display()->DisplayControllerImplSetDisplayCaptureInterface(&capture_protocol));

  constexpr uint64_t kCaptureBufferCollectionId = 1u;
  constexpr uint64_t kFramebufferBufferCollectionId = 2u;
  EXPECT_OK(display()->DisplayControllerImplImportBufferCollection(kCaptureBufferCollectionId,
                                                                   capture_token.TakeChannel()));
  EXPECT_OK(display()->DisplayControllerImplImportBufferCollection(
      kFramebufferBufferCollectionId, framebuffer_token.TakeChannel()));

  const auto kPixelFormat = fuchsia_sysmem::wire::PixelFormat{
      .type = fuchsia_sysmem::PixelFormatType::kBgra32,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = fuchsia_sysmem::wire::kFormatModifierLinear,
          },
  };

  // Must match kWidth and kHeight defined in fake-display.cc.
  // TODO(fxbug.dev/128486): Do not hardcode the display width and height.
  constexpr int kDisplayWidth = 1280;
  constexpr int kDisplayHeight = 800;

  image_t framebuffer_config = {
      .width = kDisplayWidth,
      .height = kDisplayHeight,
      .type = IMAGE_TYPE_SIMPLE,
      .handle = 0,
  };
  const image_t kCaptureConfig = {
      .width = kDisplayWidth,
      .height = kDisplayHeight,
      .type = IMAGE_TYPE_CAPTURE,
      .handle = 0,
  };

  // Set BufferCollection buffer memory constraints from the display driver's
  // end.
  EXPECT_OK(display()->DisplayControllerImplSetBufferCollectionConstraints(
      &framebuffer_config, kFramebufferBufferCollectionId));
  EXPECT_OK(display()->DisplayControllerImplSetBufferCollectionConstraints(
      &kCaptureConfig, kCaptureBufferCollectionId));

  // Set BufferCollection buffer memory constraints from the test's end.
  const uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(kPixelFormat);
  const uint32_t size_bytes = kDisplayWidth * kDisplayHeight * bytes_per_pixel;
  fidl::Status set_framebuffer_constraints_status = framebuffer_collection_client->SetConstraints(
      /* has_constraints= */ true, CreateImageConstraints(size_bytes, kPixelFormat));
  EXPECT_TRUE(set_framebuffer_constraints_status.ok());
  fidl::Status set_capture_constraints_status = capture_collection_client->SetConstraints(
      /* has_constraints= */ true, CreateImageConstraints(size_bytes, kPixelFormat));
  EXPECT_TRUE(set_capture_constraints_status.ok());

  // Both the test-side client and the driver have set the constraints.
  // The buffers should be allocated correctly in sysmem.
  auto [framebuffer_vmo, framebuffer_settings] =
      GetAllocatedBufferAndSettings(framebuffer_collection_client);
  auto [capture_vmo, capture_settings] = GetAllocatedBufferAndSettings(capture_collection_client);

  // Fill the framebuffer.
  fzl::VmoMapper framebuffer_mapper;
  ASSERT_OK(framebuffer_mapper.Map(framebuffer_vmo));
  cpp20::span<uint8_t> framebuffer_bytes(reinterpret_cast<uint8_t*>(framebuffer_mapper.start()),
                                         framebuffer_mapper.size());
  const std::vector<uint8_t> kBlueBgra = {0xff, 0, 0, 0xff};
  FillImageWithColor(framebuffer_bytes, kBlueBgra, kDisplayWidth, kDisplayHeight,
                     framebuffer_settings.image_format_constraints.bytes_per_row_divisor);
  zx_cache_flush(framebuffer_bytes.data(), framebuffer_bytes.size(),
                 ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
  framebuffer_mapper.Unmap();

  // Import capture image.
  uint64_t capture_handle = INVALID_ID;
  EXPECT_OK(display()->DisplayControllerImplImportImageForCapture(kCaptureBufferCollectionId,
                                                                  /*index=*/0, &capture_handle));
  EXPECT_NE(capture_handle, INVALID_ID);

  // Import framebuffer image.
  EXPECT_OK(display()->DisplayControllerImplImportImage(
      &framebuffer_config, kFramebufferBufferCollectionId, /*index=*/0));
  EXPECT_NE(framebuffer_config.handle, INVALID_ID);

  // Create display configuration.
  layer_t layer = CreatePrimaryLayerConfig(framebuffer_config);

  constexpr size_t kNumLayers = 1;
  std::array<layer_t*, kNumLayers> layers = {&layer};

  // Must match kDisplayId in fake-display.cc.
  // TODO(fxbug.dev/128486): Do not hardcode the display ID.
  constexpr uint64_t kDisplayId = 1;
  display_config_t display_config = {
      .display_id = kDisplayId,
      .mode = {},

      .cc_flags = 0u,
      .cc_preoffsets = {},
      .cc_coefficients = {},
      .cc_postoffsets = {},

      .layer_list = layers.data(),
      .layer_count = layers.size(),
  };
  constexpr size_t kNumDisplays = 1;
  std::array<const display_config_t*, kNumDisplays> display_configs = {&display_config};

  std::array<uint32_t, kNumLayers + 1> layer_cfg_results_for_display = {0u, 0u};
  std::array<uint32_t*, kNumDisplays> layer_cfg_results = {layer_cfg_results_for_display.data()};
  size_t layer_cfg_result_count = 0;

  // Check and apply the display configuration.
  config_check_result_t config_check_result = display()->DisplayControllerImplCheckConfiguration(
      display_configs.data(), display_configs.size(), layer_cfg_results.data(),
      &layer_cfg_result_count);
  EXPECT_EQ(config_check_result, CONFIG_CHECK_RESULT_OK);

  const display::ConfigStamp config_stamp(1);
  const config_stamp_t banjo_config_stamp = display::ToBanjoConfigStamp(config_stamp);
  display()->DisplayControllerImplApplyConfiguration(display_configs.data(), display_configs.size(),
                                                     &banjo_config_stamp);

  // Start capture; wait until the capture ends.
  EXPECT_FALSE(display_capture_completion.completed().signaled());
  EXPECT_OK(display()->DisplayControllerImplStartCapture(capture_handle));
  display_capture_completion.completed().Wait();
  EXPECT_TRUE(display_capture_completion.completed().signaled());

  // Verify the captured image has the same content as the original image.
  constexpr int kCaptureBytesPerPixel = 4;
  uint32_t capture_bytes_per_row_divisor =
      capture_settings.image_format_constraints.bytes_per_row_divisor;
  uint32_t capture_row_stride_bytes =
      fbl::round_up(uint32_t{kDisplayWidth} * kCaptureBytesPerPixel, capture_bytes_per_row_divisor);

  {
    fzl::VmoMapper capture_mapper;
    ASSERT_OK(capture_mapper.Map(capture_vmo));
    cpp20::span<const uint8_t> capture_bytes(
        reinterpret_cast<const uint8_t*>(capture_mapper.start()), /*count=*/capture_mapper.size());
    zx_cache_flush(capture_bytes.data(), capture_bytes.size(), ZX_CACHE_FLUSH_DATA);

    for (int row = 0; row < kDisplayHeight; ++row) {
      cpp20::span<const uint8_t> capture_row =
          capture_bytes.subspan(row * capture_row_stride_bytes, capture_row_stride_bytes);
      auto it = capture_row.begin();
      for (int col = 0; col < kDisplayWidth; ++col) {
        std::vector<uint8_t> curr_color(it, it + kCaptureBytesPerPixel);
        EXPECT_THAT(curr_color, testing::ElementsAreArray(kBlueBgra))
            << "Color mismatch at row " << row << " column " << col;
        it += kCaptureBytesPerPixel;
      }
    }
  }

  // Release the image.
  // TODO(fxbug.dev/128574): Consider adding RAII handles to release the
  // imported images and buffer collections.
  display()->DisplayControllerImplReleaseImage(&framebuffer_config);
  display()->DisplayControllerImplReleaseCapture(capture_handle);

  EXPECT_OK(
      display()->DisplayControllerImplReleaseBufferCollection(kFramebufferBufferCollectionId));
  EXPECT_OK(display()->DisplayControllerImplReleaseBufferCollection(kCaptureBufferCollectionId));
}

}  // namespace
}  // namespace fake_display
