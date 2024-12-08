// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/fake-display.h"

#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/fpromise/result.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/image-format/image_format.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/reader.h>
#include <lib/inspect/cpp/vmo/types.h>
#include <lib/stdcompat/span.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <memory>
#include <utility>
#include <vector>

#include <fbl/algorithm.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/fake/fake-display-stack.h"
#include "src/graphics/display/drivers/fake/fake-sysmem-device-hierarchy.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/lib/testing/predicates/status.h"

namespace fake_display {

namespace {

class FakeDisplayTest : public testing::Test {
 public:
  FakeDisplayTest() = default;

  void SetUp() override {
    zx::result<std::unique_ptr<display::FakeSysmemDeviceHierarchy>> create_sysmem_provider_result =
        display::FakeSysmemDeviceHierarchy::Create();
    ASSERT_OK(create_sysmem_provider_result);
    tree_ = std::make_unique<display::FakeDisplayStack>(
        std::move(create_sysmem_provider_result).value(), GetFakeDisplayDeviceConfig());
  }

  void TearDown() override {
    tree_->SyncShutdown();
    tree_.reset();
  }

  virtual FakeDisplayDeviceConfig GetFakeDisplayDeviceConfig() const {
    return FakeDisplayDeviceConfig{
        .manual_vsync_trigger = true,
        .no_buffer_access = false,
    };
  }

  fake_display::FakeDisplay* display() { return tree_->display(); }

  fidl::WireSyncClient<fuchsia_sysmem2::Allocator> ConnectToSysmemAllocatorV2() {
    return fidl::WireSyncClient(tree_->ConnectToSysmemAllocatorV2());
  }

 private:
  std::unique_ptr<display::FakeDisplayStack> tree_;
};

TEST_F(FakeDisplayTest, Inspect) {
  fpromise::result<inspect::Hierarchy> read_result =
      inspect::ReadFromVmo(display()->inspector().DuplicateVmo());
  ASSERT_TRUE(read_result.is_ok());

  const inspect::Hierarchy& hierarchy = read_result.value();
  const inspect::Hierarchy* config = hierarchy.GetByPath({"device_config"});
  ASSERT_NE(config, nullptr);

  // Must be the same as `kWidth` defined in fake-display.cc.
  // TODO(https://fxbug.dev/42065258): Use configurable values instead.
  constexpr int kWidth = 1280;
  const inspect::IntPropertyValue* width_px =
      config->node().get_property<inspect::IntPropertyValue>("width_px");
  ASSERT_NE(width_px, nullptr);
  EXPECT_EQ(width_px->value(), kWidth);

  // Must be the same as `kHeight` defined in fake-display.cc.
  // TODO(https://fxbug.dev/42065258): Use configurable values instead.
  constexpr int kHeight = 800;
  const inspect::IntPropertyValue* height_px =
      config->node().get_property<inspect::IntPropertyValue>("height_px");
  ASSERT_NE(height_px, nullptr);
  EXPECT_EQ(height_px->value(), kHeight);

  // Must be the same as `kRefreshRateFps` defined in fake-display.cc.
  // TODO(https://fxbug.dev/42065258): Use configurable values instead.
  constexpr double kRefreshRateHz = 60.0;
  const inspect::DoublePropertyValue* refresh_rate_hz =
      config->node().get_property<inspect::DoublePropertyValue>("refresh_rate_hz");
  ASSERT_NE(refresh_rate_hz, nullptr);
  EXPECT_DOUBLE_EQ(refresh_rate_hz->value(), kRefreshRateHz);

  const inspect::BoolPropertyValue* manual_vsync_trigger =
      config->node().get_property<inspect::BoolPropertyValue>("manual_vsync_trigger");
  ASSERT_NE(manual_vsync_trigger, nullptr);
  EXPECT_EQ(manual_vsync_trigger->value(), true);

  const inspect::BoolPropertyValue* no_buffer_access =
      config->node().get_property<inspect::BoolPropertyValue>("no_buffer_access");
  ASSERT_NE(no_buffer_access, nullptr);
  EXPECT_EQ(no_buffer_access->value(), false);
}

class FakeDisplayRealSysmemTest : public FakeDisplayTest {
 public:
  struct BufferCollectionAndToken {
    fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection> collection_client;
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> token;
  };

  FakeDisplayRealSysmemTest() = default;
  ~FakeDisplayRealSysmemTest() override = default;

  zx::result<BufferCollectionAndToken> CreateBufferCollection() {
    zx::result<fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>> token_endpoints =
        fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();

    EXPECT_OK(token_endpoints);
    if (!token_endpoints.is_ok()) {
      return token_endpoints.take_error();
    }

    auto& [token_client, token_server] = token_endpoints.value();
    fidl::Arena arena;
    auto allocate_shared_request =
        fuchsia_sysmem2::wire::AllocatorAllocateSharedCollectionRequest::Builder(arena);
    allocate_shared_request.token_request(std::move(token_server));
    fidl::Status allocate_token_status =
        sysmem_->AllocateSharedCollection(allocate_shared_request.Build());
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
    auto duplicate_request =
        fuchsia_sysmem2::wire::BufferCollectionTokenDuplicateSyncRequest::Builder(arena);
    duplicate_request.rights_attenuation_masks(rights);
    fidl::WireResult duplicate_result =
        fidl::WireCall(token_client)->DuplicateSync(duplicate_request.Build());
    EXPECT_OK(duplicate_result.status());
    if (!duplicate_result.ok()) {
      return zx::error(duplicate_result.status());
    }

    auto& duplicate_value = duplicate_result.value();
    EXPECT_EQ(duplicate_value.tokens().count(), 1u);
    if (duplicate_value.tokens().count() != 1u) {
      return zx::error(ZX_ERR_BAD_STATE);
    }

    // Bind duplicated token to BufferCollection client.
    auto [collection_client, collection_server] =
        fidl::Endpoints<fuchsia_sysmem2::BufferCollection>::Create();
    auto bind_request = fuchsia_sysmem2::wire::AllocatorBindSharedCollectionRequest::Builder(arena);
    bind_request.token(std::move(duplicate_value.tokens()[0]));
    bind_request.buffer_collection_request(std::move(collection_server));
    fidl::Status bind_status = sysmem_->BindSharedCollection(bind_request.Build());
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
    sysmem_ = ConnectToSysmemAllocatorV2();
    EXPECT_TRUE(sysmem_.is_valid());
  }

  void TearDown() override {
    sysmem_ = {};
    FakeDisplayTest::TearDown();
  }

  const fidl::WireSyncClient<fuchsia_sysmem2::Allocator>& sysmem() const { return sysmem_; }

 private:
  fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem_;
};

// A completion semaphore indicating the display capture is completed.
class DisplayCaptureCompletion {
 public:
  // Tests can import the display controller interface protocol to set up the
  // callback to trigger the semaphore.
  display_engine_listener_protocol_t GetDisplayEngineListenerProtocol() {
    static constexpr display_engine_listener_protocol_ops_t kDisplayEngineListenerProtocolOps = {
        .on_display_added = [](void* ctx, const raw_display_info_t* display_info) {},
        .on_display_removed = [](void* ctx, uint64_t display_id) {},
        .on_display_vsync = [](void* ctx, uint64_t display_id, zx_time_t timestamp,
                               const config_stamp_t* config_stamp) {},
        .on_capture_complete =
            [](void* ctx) {
              reinterpret_cast<DisplayCaptureCompletion*>(ctx)->OnCaptureComplete();
            },
    };
    return display_engine_listener_protocol_t{
        .ops = &kDisplayEngineListenerProtocolOps,
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
fuchsia_sysmem2::wire::BufferCollectionConstraints CreateImageConstraints(
    fidl::AnyArena& arena, uint32_t min_size_bytes,
    fuchsia_images2::wire::PixelFormat pixel_format) {
  // fake-display driver doesn't add extra image format constraints when
  // SetBufferCollectionConstraints() is called. To make sure we allocate an
  // image buffer, we add constraints here.
  auto constraints = fuchsia_sysmem2::wire::BufferCollectionConstraints::Builder(arena);
  constraints.usage(
      fuchsia_sysmem2::wire::BufferUsage::Builder(arena)
          .cpu(fuchsia_sysmem2::wire::kCpuUsageRead | fuchsia_sysmem2::wire::kCpuUsageWrite)
          .Build());
  constraints.min_buffer_count(1);
  constraints.max_buffer_count(1);
  auto bmc = fuchsia_sysmem2::wire::BufferMemoryConstraints::Builder(arena);
  bmc.min_size_bytes(min_size_bytes);
  bmc.ram_domain_supported(false);
  // The test cases need direct CPU access to the buffers and we
  // don't enforce cache flushing, so we should narrow down the
  // allowed sysmem heaps to the heaps supporting CPU domain and
  // reject all the other heaps.
  bmc.cpu_domain_supported(true);
  bmc.inaccessible_domain_supported(false);
  constraints.buffer_memory_constraints(bmc.Build());
  auto ifc = fuchsia_sysmem2::wire::ImageFormatConstraints::Builder(arena);
  ifc.pixel_format(pixel_format);
  ifc.color_spaces(std::array{fuchsia_images2::ColorSpace::kSrgb});
  constraints.image_format_constraints(std::array{ifc.Build()});
  return constraints.Build();
}

// Creates a primary layer config for an opaque layer that holds the `image`
// on the top-left corner of the screen without any scaling.
layer_t CreatePrimaryLayerConfig(uint64_t image_handle, const image_metadata_t& image_metadata) {
  return layer_t{
      .display_destination =
          {
              .x = 0,
              .y = 0,
              .width = image_metadata.width,
              .height = image_metadata.height,
          },
      .image_source =
          {
              .x = 0,
              .y = 0,
              .width = image_metadata.width,
              .height = image_metadata.height,
          },
      .image_handle = image_handle,
      .image_metadata = image_metadata,
      .alpha_mode = ALPHA_DISABLE,
      .alpha_layer_val = 1.0,
      .image_source_transformation = COORDINATE_TRANSFORMATION_IDENTITY,
  };
}

std::pair<zx::vmo, fuchsia_sysmem2::wire::SingleBufferSettings> GetAllocatedBufferAndSettings(
    fidl::AnyArena& arena, const fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>& client) {
  auto wait_result = client->WaitForAllBuffersAllocated();
  ZX_ASSERT_MSG(wait_result.ok(), "WaitForBuffersAllocated() FIDL call failed: %s (%u)",
                wait_result.status_string(), fidl::ToUnderlying(wait_result->error_value()));
  auto& buffer_collection_info = wait_result.value()->buffer_collection_info();
  ZX_ASSERT_MSG(buffer_collection_info.buffers().count() == 1u,
                "Incorrect number of buffers allocated: actual %zu, expected 1",
                buffer_collection_info.buffers().count());
  // wire types don't provide a way to clone into a different arena short of converting to natural
  // type and back; we could consider returning the natural type instead, or converting this whole
  // file to natural types, but for now this allows the caller to use wire types everyhwere (not
  // necessarily a goal; just how the client code currently works)
  auto settings_clone = fidl::ToWire(arena, fidl::ToNatural(buffer_collection_info.settings()));
  return {std::move(buffer_collection_info.buffers()[0].vmo()), std::move(settings_clone)};
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

TEST_F(FakeDisplayRealSysmemTest, ImportBufferCollection) {
  zx::result<BufferCollectionAndToken> new_buffer_collection_result = CreateBufferCollection();
  ASSERT_OK(new_buffer_collection_result);
  auto [collection_client, token] = std::move(new_buffer_collection_result.value());

  // Test ImportBufferCollection().
  constexpr display::DriverBufferCollectionId kValidBufferCollectionId(1);
  constexpr uint64_t kBanjoValidBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kValidBufferCollectionId);
  EXPECT_OK(display()->DisplayEngineImportBufferCollection(kBanjoValidBufferCollectionId,
                                                           token.TakeChannel()));

  // `driver_buffer_collection_id` must be unused.
  zx::result<fidl::Endpoints<fuchsia_sysmem::BufferCollectionToken>> another_token_endpoints =
      fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_OK(another_token_endpoints);
  EXPECT_EQ(display()->DisplayEngineImportBufferCollection(
                kBanjoValidBufferCollectionId, another_token_endpoints->client.TakeChannel()),
            ZX_ERR_ALREADY_EXISTS);

  // Driver sets BufferCollection buffer memory constraints.
  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(display()->DisplayEngineSetBufferCollectionConstraints(&kDisplayUsage,
                                                                   kBanjoValidBufferCollectionId));

  // Set BufferCollection buffer memory constraints.
  fidl::Arena arena;
  auto set_constraints_request =
      fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest::Builder(arena);
  set_constraints_request.constraints(CreateImageConstraints(
      arena, /*min_size_bytes=*/4096, fuchsia_images2::PixelFormat::kR8G8B8A8));
  fidl::Status set_constraints_status =
      collection_client->SetConstraints(set_constraints_request.Build());
  EXPECT_TRUE(set_constraints_status.ok());

  // Both the test-side client and the driver have  set the constraints.
  // The buffer should be allocated correctly in sysmem.
  EXPECT_TRUE(collection_client->WaitForAllBuffersAllocated().ok());

  // Test ReleaseBufferCollection().
  // TODO(https://fxbug.dev/42079040): Consider adding RAII handles to release the
  // imported buffer collections.
  constexpr display::DriverBufferCollectionId kInvalidBufferCollectionId(2);
  constexpr uint64_t kBanjoInvalidBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kInvalidBufferCollectionId);
  EXPECT_EQ(display()->DisplayEngineReleaseBufferCollection(kBanjoInvalidBufferCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display()->DisplayEngineReleaseBufferCollection(kBanjoValidBufferCollectionId));
}

TEST_F(FakeDisplayRealSysmemTest, ImportImage) {
  zx::result<BufferCollectionAndToken> new_buffer_collection_result = CreateBufferCollection();
  ASSERT_OK(new_buffer_collection_result);
  auto [collection_client, token] = std::move(new_buffer_collection_result.value());

  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  EXPECT_OK(display()->DisplayEngineImportBufferCollection(kBanjoBufferCollectionId,
                                                           token.TakeChannel()));

  // Driver sets BufferCollection buffer memory constraints.
  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(display()->DisplayEngineSetBufferCollectionConstraints(&kDisplayUsage,
                                                                   kBanjoBufferCollectionId));

  // Set BufferCollection buffer memory constraints.
  static constexpr const image_metadata_t kDisplayImageMetadata = {
      .width = 1024,
      .height = 768,
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };

  const auto kPixelFormat = PixelFormatAndModifier(fuchsia_images2::PixelFormat::kB8G8R8A8,
                                                   fuchsia_images2::PixelFormatModifier::kLinear);

  const uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(kPixelFormat);
  fidl::Arena arena;
  auto set_constraints_request =
      fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest::Builder(arena);
  set_constraints_request.constraints(
      CreateImageConstraints(arena,
                             /*min_size_bytes=*/kDisplayImageMetadata.width *
                                 kDisplayImageMetadata.height * bytes_per_pixel,
                             kPixelFormat.pixel_format));
  fidl::Status set_constraints_status =
      collection_client->SetConstraints(set_constraints_request.Build());
  EXPECT_TRUE(set_constraints_status.ok());

  // Both the test-side client and the driver have set the constraints.
  // The buffer should be allocated correctly in sysmem.
  EXPECT_TRUE(collection_client->WaitForAllBuffersAllocated().ok());

  // TODO(https://fxbug.dev/42079037): Split all valid / invalid imports into separate
  // test cases.
  // Invalid import: Bad image type.
  static constexpr const image_metadata_t kInvalidTilingTypeMetadata = {
      .width = 1024,
      .height = 768,
      .tiling_type = IMAGE_TILING_TYPE_CAPTURE,
  };
  uint64_t image_handle = 0;
  EXPECT_EQ(
      display()->DisplayEngineImportImage(&kInvalidTilingTypeMetadata, kBanjoBufferCollectionId,
                                          /*index=*/0, &image_handle),
      ZX_ERR_INVALID_ARGS);

  // Invalid import: Invalid collection ID.
  constexpr display::DriverBufferCollectionId kInvalidBufferCollectionId(100);
  constexpr uint64_t kBanjoInvalidBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kInvalidBufferCollectionId);
  image_handle = 0;
  EXPECT_EQ(
      display()->DisplayEngineImportImage(&kDisplayImageMetadata, kBanjoInvalidBufferCollectionId,
                                          /*index=*/0, &image_handle),
      ZX_ERR_NOT_FOUND);

  // Invalid import: Invalid buffer collection index.
  constexpr uint64_t kInvalidBufferCollectionIndex = 100u;
  image_handle = 0;
  EXPECT_EQ(display()->DisplayEngineImportImage(&kDisplayImageMetadata, kBanjoBufferCollectionId,
                                                kInvalidBufferCollectionIndex, &image_handle),
            ZX_ERR_OUT_OF_RANGE);

  // Valid import.
  image_handle = 0;
  EXPECT_OK(display()->DisplayEngineImportImage(&kDisplayImageMetadata, kBanjoBufferCollectionId,
                                                /*index=*/0, &image_handle));
  EXPECT_NE(image_handle, 0u);

  // Release the image.
  display()->DisplayEngineReleaseImage(image_handle);

  EXPECT_OK(display()->DisplayEngineReleaseBufferCollection(kBanjoBufferCollectionId));
}

TEST_F(FakeDisplayRealSysmemTest, ImportImageForCapture) {
  zx::result<BufferCollectionAndToken> new_buffer_collection_result = CreateBufferCollection();
  ASSERT_OK(new_buffer_collection_result);
  auto [collection_client, token] = std::move(new_buffer_collection_result.value());

  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  EXPECT_OK(display()->DisplayEngineImportBufferCollection(kBanjoBufferCollectionId,
                                                           token.TakeChannel()));

  const auto kPixelFormat = PixelFormatAndModifier(fuchsia_images2::PixelFormat::kB8G8R8A8,
                                                   fuchsia_images2::PixelFormatModifier::kLinear);

  constexpr uint32_t kDisplayWidth = 1280;
  constexpr uint32_t kDisplayHeight = 800;

  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(display()->DisplayEngineSetBufferCollectionConstraints(&kDisplayUsage,
                                                                   kBanjoBufferCollectionId));
  const uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(kPixelFormat);
  const uint32_t size_bytes = kDisplayWidth * kDisplayHeight * bytes_per_pixel;
  // Set BufferCollection buffer memory constraints.
  fidl::Arena arena;
  auto set_constraints_request =
      fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest::Builder(arena);
  set_constraints_request.constraints(
      CreateImageConstraints(arena, size_bytes, kPixelFormat.pixel_format));
  fidl::Status set_constraints_status =
      collection_client->SetConstraints(set_constraints_request.Build());
  EXPECT_TRUE(set_constraints_status.ok());

  // Both the test-side client and the driver have set the constraints.
  // The buffer should be allocated correctly in sysmem.
  EXPECT_TRUE(collection_client->WaitForAllBuffersAllocated().ok());

  uint64_t out_capture_handle = INVALID_ID;

  // TODO(https://fxbug.dev/42079037): Split all valid / invalid imports into separate
  // test cases.
  // Invalid import: Invalid collection ID.
  constexpr display::DriverBufferCollectionId kInvalidBufferCollectionId(100);
  constexpr uint64_t kBanjoInvalidBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kInvalidBufferCollectionId);
  EXPECT_EQ(display()->DisplayEngineImportImageForCapture(kBanjoInvalidBufferCollectionId,
                                                          /*index=*/0, &out_capture_handle),
            ZX_ERR_NOT_FOUND);

  // Invalid import: Invalid buffer collection index.
  constexpr uint64_t kInvalidBufferCollectionIndex = 100u;
  EXPECT_EQ(display()->DisplayEngineImportImageForCapture(
                kBanjoBufferCollectionId, kInvalidBufferCollectionIndex, &out_capture_handle),
            ZX_ERR_OUT_OF_RANGE);

  // Valid import.
  EXPECT_OK(display()->DisplayEngineImportImageForCapture(kBanjoBufferCollectionId, /*index=*/0,
                                                          &out_capture_handle));
  EXPECT_NE(out_capture_handle, INVALID_ID);

  // Release the image.
  // TODO(https://fxbug.dev/42079040): Consider adding RAII handles to release the
  // imported images and buffer collections.
  display()->DisplayEngineReleaseCapture(out_capture_handle);

  EXPECT_OK(display()->DisplayEngineReleaseBufferCollection(kBanjoBufferCollectionId));
}

TEST_F(FakeDisplayRealSysmemTest, Capture) {
  zx::result<BufferCollectionAndToken> new_capture_buffer_collection_result =
      CreateBufferCollection();
  ASSERT_OK(new_capture_buffer_collection_result);
  auto [capture_collection_client, capture_token] =
      std::move(new_capture_buffer_collection_result.value());

  zx::result<BufferCollectionAndToken> new_framebuffer_buffer_collection_result =
      CreateBufferCollection();
  ASSERT_OK(new_framebuffer_buffer_collection_result);
  auto [framebuffer_collection_client, framebuffer_token] =
      std::move(new_framebuffer_buffer_collection_result.value());

  DisplayCaptureCompletion display_capture_completion = {};
  const display_engine_listener_protocol_t& controller_protocol =
      display_capture_completion.GetDisplayEngineListenerProtocol();
  display()->DisplayEngineSetListener(&controller_protocol);

  constexpr display::DriverBufferCollectionId kCaptureBufferCollectionId(1);
  constexpr uint64_t kBanjoCaptureBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kCaptureBufferCollectionId);
  constexpr display::DriverBufferCollectionId kFramebufferBufferCollectionId(2);
  constexpr uint64_t kBanjoFramebufferBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kFramebufferBufferCollectionId);
  EXPECT_OK(display()->DisplayEngineImportBufferCollection(kBanjoCaptureBufferCollectionId,
                                                           capture_token.TakeChannel()));
  EXPECT_OK(display()->DisplayEngineImportBufferCollection(kBanjoFramebufferBufferCollectionId,
                                                           framebuffer_token.TakeChannel()));

  const auto kPixelFormat = PixelFormatAndModifier(fuchsia_images2::PixelFormat::kB8G8R8A8,
                                                   fuchsia_images2::PixelFormatModifier::kLinear);

  // Must match kWidth and kHeight defined in fake-display.cc.
  // TODO(https://fxbug.dev/42078942): Do not hardcode the display width and height.
  constexpr int kDisplayWidth = 1280;
  constexpr int kDisplayHeight = 800;

  // Set BufferCollection buffer memory constraints from the display driver's
  // end.
  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(display()->DisplayEngineSetBufferCollectionConstraints(
      &kDisplayUsage, kBanjoFramebufferBufferCollectionId));
  static constexpr image_buffer_usage_t kCaptureUsage = {
      .tiling_type = IMAGE_TILING_TYPE_CAPTURE,
  };
  EXPECT_OK(display()->DisplayEngineSetBufferCollectionConstraints(
      &kCaptureUsage, kBanjoCaptureBufferCollectionId));

  // Set BufferCollection buffer memory constraints from the test's end.
  const uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(kPixelFormat);
  const uint32_t size_bytes = kDisplayWidth * kDisplayHeight * bytes_per_pixel;

  fidl::Arena arena;
  auto framebuffer_set_constraints_request =
      fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest::Builder(arena);
  framebuffer_set_constraints_request.constraints(
      CreateImageConstraints(arena, size_bytes, kPixelFormat.pixel_format));
  fidl::Status set_framebuffer_constraints_status =
      framebuffer_collection_client->SetConstraints(framebuffer_set_constraints_request.Build());
  EXPECT_TRUE(set_framebuffer_constraints_status.ok());
  arena.Reset();

  auto capture_set_constraints_request =
      fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest::Builder(arena);
  capture_set_constraints_request.constraints(
      CreateImageConstraints(arena, size_bytes, kPixelFormat.pixel_format));
  fidl::Status set_capture_constraints_status =
      capture_collection_client->SetConstraints(capture_set_constraints_request.Build());
  EXPECT_TRUE(set_capture_constraints_status.ok());
  arena.Reset();

  // Both the test-side client and the driver have set the constraints.
  // The buffers should be allocated correctly in sysmem.
  auto [framebuffer_vmo, framebuffer_settings] =
      GetAllocatedBufferAndSettings(arena, framebuffer_collection_client);
  auto [capture_vmo, capture_settings] =
      GetAllocatedBufferAndSettings(arena, capture_collection_client);

  // Fill the framebuffer.
  fzl::VmoMapper framebuffer_mapper;
  ASSERT_OK(framebuffer_mapper.Map(framebuffer_vmo));
  cpp20::span<uint8_t> framebuffer_bytes(reinterpret_cast<uint8_t*>(framebuffer_mapper.start()),
                                         framebuffer_mapper.size());
  const std::vector<uint8_t> kBlueBgra = {0xff, 0, 0, 0xff};
  FillImageWithColor(framebuffer_bytes, kBlueBgra, kDisplayWidth, kDisplayHeight,
                     framebuffer_settings.image_format_constraints().bytes_per_row_divisor());
  zx_cache_flush(framebuffer_bytes.data(), framebuffer_bytes.size(),
                 ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
  framebuffer_mapper.Unmap();

  // Import capture image.
  uint64_t capture_handle = INVALID_ID;
  EXPECT_OK(display()->DisplayEngineImportImageForCapture(kBanjoCaptureBufferCollectionId,
                                                          /*index=*/0, &capture_handle));
  EXPECT_NE(capture_handle, INVALID_ID);

  // Import framebuffer image.
  static constexpr image_metadata_t kFramebufferImageMetadata = {
      .width = kDisplayWidth,
      .height = kDisplayHeight,
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };

  uint64_t framebuffer_image_handle = 0;
  EXPECT_OK(display()->DisplayEngineImportImage(&kFramebufferImageMetadata,
                                                kBanjoFramebufferBufferCollectionId,
                                                /*index=*/0, &framebuffer_image_handle));
  EXPECT_NE(framebuffer_image_handle, INVALID_ID);

  // Create display configuration.
  constexpr size_t kLayerCount = 1;
  std::array<const layer_t, kLayerCount> kLayers = {
      CreatePrimaryLayerConfig(framebuffer_image_handle, kFramebufferImageMetadata),
  };

  // Must match kDisplayId in fake-display.cc.
  // TODO(https://fxbug.dev/42078942): Do not hardcode the display ID.
  constexpr display::DisplayId kDisplayId(1);
  constexpr size_t kDisplayCount = 1;
  std::array<const display_config_t, kDisplayCount> kDisplayConfigs = {
      display_config_t{
          .display_id = display::ToBanjoDisplayId(kDisplayId),
          .mode = {},

          .cc_flags = 0u,
          .cc_preoffsets = {},
          .cc_coefficients = {},
          .cc_postoffsets = {},

          .layer_list = kLayers.data(),
          .layer_count = kLayers.size(),
      },
  };

  std::array<layer_composition_operations_t, kLayerCount> layer_composition_operations = {0u};
  size_t layer_composition_operations_count = 0;

  // Check and apply the display configuration.
  config_check_result_t config_check_result = display()->DisplayEngineCheckConfiguration(
      kDisplayConfigs.data(), kDisplayConfigs.size(), layer_composition_operations.data(),
      layer_composition_operations.size(), &layer_composition_operations_count);
  EXPECT_EQ(config_check_result, CONFIG_CHECK_RESULT_OK);

  const display::ConfigStamp config_stamp(1);
  const config_stamp_t banjo_config_stamp = display::ToBanjoConfigStamp(config_stamp);
  display()->DisplayEngineApplyConfiguration(kDisplayConfigs.data(), kDisplayConfigs.size(),
                                             &banjo_config_stamp);

  // Start capture; wait until the capture ends.
  EXPECT_FALSE(display_capture_completion.completed().signaled());
  EXPECT_OK(display()->DisplayEngineStartCapture(capture_handle));
  display_capture_completion.completed().Wait();
  EXPECT_TRUE(display_capture_completion.completed().signaled());

  // Verify the captured image has the same content as the original image.
  constexpr int kCaptureBytesPerPixel = 4;
  uint32_t capture_bytes_per_row_divisor =
      capture_settings.image_format_constraints().bytes_per_row_divisor();
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
  // TODO(https://fxbug.dev/42079040): Consider adding RAII handles to release the
  // imported images and buffer collections.
  display()->DisplayEngineReleaseImage(framebuffer_image_handle);
  display()->DisplayEngineReleaseCapture(capture_handle);

  EXPECT_OK(display()->DisplayEngineReleaseBufferCollection(kBanjoFramebufferBufferCollectionId));
  EXPECT_OK(display()->DisplayEngineReleaseBufferCollection(kBanjoCaptureBufferCollectionId));
}

class FakeDisplayWithoutCaptureRealSysmemTest : public FakeDisplayRealSysmemTest {
 public:
  FakeDisplayDeviceConfig GetFakeDisplayDeviceConfig() const override {
    return {
        .manual_vsync_trigger = true,
        .no_buffer_access = true,
    };
  }
};

TEST_F(FakeDisplayWithoutCaptureRealSysmemTest, SetDisplayCaptureInterface) {
  EXPECT_EQ(display()->DisplayEngineIsCaptureSupported(), false);
}

TEST_F(FakeDisplayWithoutCaptureRealSysmemTest, ImportImageForCapture) {
  constexpr uint64_t kFakeCollectionId = 1;
  constexpr uint32_t kFakeCollectionIndex = 0;
  uint64_t out_capture_handle;
  EXPECT_STATUS(display()->DisplayEngineImportImageForCapture(
                    kFakeCollectionId, kFakeCollectionIndex, &out_capture_handle),
                ZX_ERR_NOT_SUPPORTED);
}

TEST_F(FakeDisplayWithoutCaptureRealSysmemTest, StartCapture) {
  constexpr uint64_t kFakeCaptureHandle = 1;
  EXPECT_STATUS(display()->DisplayEngineStartCapture(kFakeCaptureHandle), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(FakeDisplayWithoutCaptureRealSysmemTest, ReleaseCapture) {
  constexpr uint64_t kFakeCaptureHandle = 1;
  EXPECT_STATUS(display()->DisplayEngineReleaseCapture(kFakeCaptureHandle), ZX_ERR_NOT_SUPPORTED);
}

}  // namespace
}  // namespace fake_display
