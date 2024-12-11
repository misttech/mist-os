// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/framebuffer-display/framebuffer-display.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire_test_base.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/wait.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>
#include <lib/fit/defer.h>
#include <lib/zx/object.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/rights.h>

#include <list>
#include <memory>

#include <bind/fuchsia/sysmem/heap/cpp/bind.h>
#include <fake-mmio-reg/fake-mmio-reg.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/testing/predicates/status.h"

namespace framebuffer_display {

namespace {

// TODO(https://fxbug.dev/42072949): Consider creating and using a unified set of sysmem
// testing doubles instead of writing mocks for each display driver test.
class FakeBufferCollection : public fidl::testing::WireTestBase<fuchsia_sysmem2::BufferCollection> {
 public:
  explicit FakeBufferCollection(zx::unowned_vmo framebuffer_vmo)
      : framebuffer_vmo_(std::move(framebuffer_vmo)) {}

  void SetConstraints(::fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest* request,
                      SetConstraintsCompleter::Sync& completer) override {}
  void CheckAllBuffersAllocated(CheckAllBuffersAllocatedCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }
  void WaitForAllBuffersAllocated(WaitForAllBuffersAllocatedCompleter::Sync& completer) override {
    zx::vmo vmo;
    EXPECT_OK(framebuffer_vmo_->duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));

    fidl::Arena arena;
    auto response =
        fuchsia_sysmem2::wire::BufferCollectionWaitForAllBuffersAllocatedResponse::Builder(arena);
    auto collection_info = fuchsia_sysmem2::wire::BufferCollectionInfo::Builder(arena);
    auto single_buffer_settings = fuchsia_sysmem2::wire::SingleBufferSettings::Builder(arena);
    auto buffer_memory_settings = fuchsia_sysmem2::wire::BufferMemorySettings::Builder(arena);
    auto heap = fuchsia_sysmem2::wire::Heap::Builder(arena);
    heap.heap_type(arena, bind_fuchsia_sysmem_heap::HEAP_TYPE_FRAMEBUFFER);
    // no need to set heap.id - defaults to 0 server-side
    buffer_memory_settings.heap(heap.Build());
    single_buffer_settings.buffer_settings(buffer_memory_settings.Build());
    auto image_format_constraints = fuchsia_sysmem2::wire::ImageFormatConstraints::Builder(arena);
    image_format_constraints.pixel_format(fuchsia_images2::wire::PixelFormat::kB8G8R8A8);
    image_format_constraints.pixel_format_modifier(
        fuchsia_images2::wire::PixelFormatModifier::kLinear);
    single_buffer_settings.image_format_constraints(image_format_constraints.Build());
    collection_info.settings(single_buffer_settings.Build());
    auto vmo_buffer = fuchsia_sysmem2::wire::VmoBuffer::Builder(arena);
    vmo_buffer.vmo(std::move(vmo));
    vmo_buffer.vmo_usable_start(0);
    collection_info.buffers(std::array{vmo_buffer.Build()});
    response.buffer_collection_info(collection_info.Build());

    completer.ReplySuccess(response.Build());
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {}

 private:
  zx::unowned_vmo framebuffer_vmo_;
};

using BufferCollectionId = uint64_t;

class FakeSysmemBase {
 public:
  virtual BufferCollectionId AllocBufferCollectionId() = 0;
  virtual std::optional<std::pair<uint64_t, uint32_t>> GetFakeVmoInfo() = 0;
};

class MockAllocator : public fidl::testing::WireTestBase<fuchsia_sysmem2::Allocator> {
 public:
  explicit MockAllocator(FakeSysmemBase& parent, async_dispatcher_t* dispatcher,
                         zx::unowned_vmo framebuffer_vmo)
      : parent_(parent), dispatcher_(dispatcher), framebuffer_vmo_(std::move(framebuffer_vmo)) {
    ZX_ASSERT(dispatcher_);
  }

  void BindSharedCollection(BindSharedCollectionRequestView request,
                            BindSharedCollectionCompleter::Sync& completer) override {
    auto buffer_collection_id = parent_.AllocBufferCollectionId();
    active_buffer_collections_.emplace(
        buffer_collection_id,
        BufferCollection{
            .token_client = std::move(request->token()),
            .unowned_collection_server = request->buffer_collection_request(),
            .fake_buffer_collection = FakeBufferCollection(framebuffer_vmo_->borrow())});
    fidl::BindServer(
        dispatcher_, std::move(request->buffer_collection_request()),
        &active_buffer_collections_.at(buffer_collection_id).fake_buffer_collection,
        [this, buffer_collection_id](FakeBufferCollection*, fidl::UnbindInfo,
                                     fidl::ServerEnd<fuchsia_sysmem2::BufferCollection>) {
          inactive_buffer_collection_tokens_.push_back(
              std::move(active_buffer_collections_.at(buffer_collection_id).token_client));
          active_buffer_collections_.erase(buffer_collection_id);
        });
  }

  void GetVmoInfo(GetVmoInfoRequestView request, GetVmoInfoCompleter::Sync& completer) override {
    auto fake_vmo_info_result = parent_.GetFakeVmoInfo();
    // Call SetupFakeVmoInfo() in the test before GetVmoInfo() gets called.
    ZX_ASSERT(fake_vmo_info_result.has_value());
    fidl::Arena arena;
    auto response = fuchsia_sysmem2::wire::AllocatorGetVmoInfoResponse::Builder(arena);
    response.buffer_collection_id(fake_vmo_info_result->first);
    response.buffer_index(fake_vmo_info_result->second);

    completer.ReplySuccess(response.Build());
  }

  std::vector<std::pair<fidl::UnownedClientEnd<fuchsia_sysmem2::BufferCollectionToken>,
                        fidl::UnownedServerEnd<fuchsia_sysmem2::BufferCollection>>>
  GetBufferCollectionConnections() {
    if (active_buffer_collections_.empty()) {
      return {};
    }

    std::vector<std::pair<fidl::UnownedClientEnd<fuchsia_sysmem2::BufferCollectionToken>,
                          fidl::UnownedServerEnd<fuchsia_sysmem2::BufferCollection>>>
        result;
    for (const auto& kv : active_buffer_collections_) {
      result.emplace_back(kv.second.token_client, kv.second.unowned_collection_server);
    }
    return result;
  }

  void SetDebugClientInfo(SetDebugClientInfoRequestView request,
                          SetDebugClientInfoCompleter::Sync& completer) override {
    EXPECT_EQ(request->name().get().find("framebuffer-display"), 0u);
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    FDF_LOG(ERROR, "%s not implemented", name.c_str());
    EXPECT_TRUE(false);
  }

 private:
  struct BufferCollection {
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> token_client;
    fidl::UnownedServerEnd<fuchsia_sysmem2::BufferCollection> unowned_collection_server;
    FakeBufferCollection fake_buffer_collection;
  };

  FakeSysmemBase& parent_;
  std::unordered_map<BufferCollectionId, BufferCollection> active_buffer_collections_;
  std::vector<fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
      inactive_buffer_collection_tokens_;

  async_dispatcher_t* dispatcher_ = nullptr;
  zx::unowned_vmo framebuffer_vmo_;
};

class FakeSysmem : public fidl::testing::WireTestBase<fuchsia_hardware_sysmem::Sysmem>,
                   public FakeSysmemBase {
 public:
  explicit FakeSysmem(async_dispatcher_t* dispatcher, zx::unowned_vmo framebuffer_vmo,
                      uint64_t first_buffer_collection_id)
      : dispatcher_(dispatcher),
        framebuffer_vmo_(std::move(framebuffer_vmo)),
        next_buffer_collection_id_(first_buffer_collection_id) {
    EXPECT_TRUE(dispatcher_);
  }

  fit::result<zx_status_t, fidl::WireSyncClient<fuchsia_sysmem2::Allocator>>
  MakeFakeSysmemAllocator() {
    auto [sysmem_client, sysmem_server] = fidl::Endpoints<fuchsia_sysmem2::Allocator>::Create();

    mock_allocators_.emplace_front(*this, dispatcher_, framebuffer_vmo_->borrow());
    auto it = mock_allocators_.begin();
    fidl::BindServer(dispatcher_, std::move(sysmem_server), &*it);

    return fit::ok(fidl::WireSyncClient(std::move(sysmem_client)));
  }

  std::list<MockAllocator>& mock_allocators() { return mock_allocators_; }

  // FIDL methods
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  BufferCollectionId AllocBufferCollectionId() override { return next_buffer_collection_id_++; }

  std::optional<std::pair<uint64_t, uint32_t>> GetFakeVmoInfo() override { return fake_vmo_info_; }

  void SetupFakeVmoInfo(uint64_t buffer_collection_id, uint32_t buffer_index) {
    fake_vmo_info_.emplace(std::make_pair(buffer_collection_id, buffer_index));
  }

 private:
  friend class MockAllocator;
  std::list<MockAllocator> mock_allocators_;
  async_dispatcher_t* dispatcher_ = nullptr;
  zx::unowned_vmo framebuffer_vmo_ = {};
  BufferCollectionId next_buffer_collection_id_ = 0;
  std::optional<std::pair<uint64_t, uint32_t>> fake_vmo_info_;
};

class FakeMmio {
 public:
  FakeMmio() {
    mmio_ = std::make_unique<ddk_fake::FakeMmioRegRegion>(sizeof(uint32_t), kRegArrayLength);
  }

  fdf::MmioBuffer MmioBuffer() { return mmio_->GetMmioBuffer(); }

  ddk_fake::FakeMmioReg& FakeRegister(size_t address) { return (*mmio_)[address]; }

 private:
  static constexpr size_t kMmioBufferSize = 0x5000;
  static constexpr size_t kRegArrayLength = kMmioBufferSize / sizeof(uint32_t);
  std::unique_ptr<ddk_fake::FakeMmioRegRegion> mmio_;
};

class FramebufferDisplayTest : public ::testing::Test {
 private:
  fdf_testing::ScopedGlobalLogger logger_;
};

void ExpectHandlesArePaired(zx_handle_t lhs, zx_handle_t rhs) {
  auto [lhs_koid, lhs_related_koid] = fsl::GetKoids(lhs);
  auto [rhs_koid, rhs_related_koid] = fsl::GetKoids(rhs);

  EXPECT_NE(lhs_koid, ZX_KOID_INVALID);
  EXPECT_NE(lhs_related_koid, ZX_KOID_INVALID);
  EXPECT_NE(rhs_koid, ZX_KOID_INVALID);
  EXPECT_NE(rhs_related_koid, ZX_KOID_INVALID);

  EXPECT_EQ(lhs_koid, rhs_related_koid);
  EXPECT_EQ(rhs_koid, lhs_related_koid);
}

template <typename T>
void ExpectObjectsArePaired(zx::unowned<T> lhs, zx::unowned<T> rhs) {
  return ExpectHandlesArePaired(lhs->get(), rhs->get());
}

TEST_F(FramebufferDisplayTest, ImportBufferCollection) {
  async::Loop env_loop(&kAsyncLoopConfigAttachToCurrentThread);
  FakeSysmem fake_sysmem(env_loop.dispatcher(), /*framebuffer_vmo=*/{}, 0);
  FakeMmio fake_mmio;

  auto [hardware_sysmem_client, hardware_sysmem_server] =
      fidl::Endpoints<fuchsia_hardware_sysmem::Sysmem>::Create();
  fidl::BindServer(env_loop.dispatcher(), std::move(hardware_sysmem_server), &fake_sysmem);

  auto sysmem_client_result = fake_sysmem.MakeFakeSysmemAllocator();
  ASSERT_TRUE(sysmem_client_result.is_ok());
  auto& sysmem_client = sysmem_client_result.value();

  constexpr int32_t kWidthPx = 800;
  constexpr int32_t kHeightPx = 600;
  constexpr int32_t kStridePx = 800;
  constexpr auto kPixelFormat = fuchsia_images2::wire::PixelFormat::kB8G8R8A8;
  constexpr DisplayProperties kDisplayProperties = {
      .width_px = kWidthPx,
      .height_px = kHeightPx,
      .row_stride_px = kStridePx,
      .pixel_format = kPixelFormat,
  };

  async::Loop display_loop(&kAsyncLoopConfigNeverAttachToThread);
  display_loop.StartThread("framebuffer-display-loop");
  FramebufferDisplay display(fidl::WireSyncClient(std::move(hardware_sysmem_client)),
                             std::move(sysmem_client), fake_mmio.MmioBuffer(), kDisplayProperties,
                             display_loop.dispatcher());

  auto token1_endpoints = fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();
  zx::result token2_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token2_endpoints.is_ok());

  // Test ImportBufferCollection().
  const display::DriverBufferCollectionId kValidCollectionId(1);
  const uint64_t kBanjoValidCollectionId =
      display::ToBanjoDriverBufferCollectionId(kValidCollectionId);
  EXPECT_OK(display.DisplayEngineImportBufferCollection(kBanjoValidCollectionId,
                                                        token1_endpoints.client.TakeChannel()));

  // `collection_id` must be unused.
  EXPECT_EQ(display.DisplayEngineImportBufferCollection(kBanjoValidCollectionId,
                                                        token2_endpoints->client.TakeChannel()),
            ZX_ERR_ALREADY_EXISTS);

  env_loop.RunUntilIdle();

  EXPECT_EQ(fake_sysmem.mock_allocators().size(), 1u);
  auto& allocator = fake_sysmem.mock_allocators().front();

  // Verify that the current buffer collection token is used.
  {
    const std::vector buffer_collection_connections = allocator.GetBufferCollectionConnections();
    ASSERT_EQ(buffer_collection_connections.size(), 1u);

    const auto& buffer_collection_server = buffer_collection_connections[0].second;
    const auto& buffer_collection_client =
        display.GetBufferCollectionsForTesting().at(kValidCollectionId).client_end();
    ExpectObjectsArePaired(buffer_collection_server.handle(), buffer_collection_client.handle());

    const auto& buffer_collection_token_server = token1_endpoints.server;
    const auto& buffer_collection_token_client = buffer_collection_connections[0].first;
    ExpectObjectsArePaired(buffer_collection_token_server.handle(),
                           buffer_collection_token_client.handle());
  }

  // Test ReleaseBufferCollection().
  const uint64_t kBanjoInvalidCollectionId = 2u;
  EXPECT_STATUS(display.DisplayEngineReleaseBufferCollection(kBanjoInvalidCollectionId),
                ZX_ERR_NOT_FOUND);
  EXPECT_OK(display.DisplayEngineReleaseBufferCollection(kBanjoValidCollectionId));

  env_loop.RunUntilIdle();

  // Verify that the current buffer collection token is released.
  {
    const std::vector buffer_collection_connections = allocator.GetBufferCollectionConnections();
    ASSERT_EQ(buffer_collection_connections.size(), 0u);
  }

  // Shutdown the loop before destroying the FakeSysmem and MockAllocator which
  // may still have pending callbacks.
  env_loop.Shutdown();
  display_loop.Shutdown();
}

TEST_F(FramebufferDisplayTest, ImportKernelFramebufferImage) {
  constexpr int32_t kWidthPx = 800;
  constexpr int32_t kHeightPx = 600;
  constexpr int32_t kStridePx = 800;
  constexpr auto kPixelFormat = fuchsia_images2::wire::PixelFormat::kB8G8R8A8;
  constexpr size_t kBytesPerPixel = 4;
  const uint64_t kBanjoCollectionId = 1u;
  constexpr size_t kImageBytes = uint64_t{kStridePx} * kHeightPx * kBytesPerPixel;

  // `framebuffer_vmo` must outlive `fake_sysmem`.
  zx::vmo framebuffer_vmo;
  EXPECT_OK(zx::vmo::create(/*size=*/kImageBytes, /*options=*/0, &framebuffer_vmo));

  async::Loop env_loop(&kAsyncLoopConfigNeverAttachToThread);
  FakeSysmem fake_sysmem(env_loop.dispatcher(), framebuffer_vmo.borrow(), kBanjoCollectionId);
  FakeMmio fake_mmio;

  env_loop.StartThread("env-loop");

  auto [hardware_sysmem_client, hardware_sysmem_server] =
      fidl::Endpoints<fuchsia_hardware_sysmem::Sysmem>::Create();
  fidl::BindServer(env_loop.dispatcher(), std::move(hardware_sysmem_server), &fake_sysmem);

  auto sysmem_client_result = fake_sysmem.MakeFakeSysmemAllocator();
  ASSERT_TRUE(sysmem_client_result.is_ok());
  auto& sysmem_client = sysmem_client_result.value();

  constexpr DisplayProperties kDisplayProperties = {
      .width_px = kWidthPx,
      .height_px = kHeightPx,
      .row_stride_px = kStridePx,
      .pixel_format = kPixelFormat,
  };

  async::Loop display_loop(&kAsyncLoopConfigNeverAttachToThread);
  display_loop.StartThread("framebuffer-display-loop");
  FramebufferDisplay display(fidl::WireSyncClient(std::move(hardware_sysmem_client)),
                             std::move(sysmem_client), fake_mmio.MmioBuffer(), kDisplayProperties,
                             display_loop.dispatcher());

  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());

  // Import BufferCollection.
  EXPECT_OK(display.DisplayEngineImportBufferCollection(kBanjoCollectionId,
                                                        token_endpoints->client.TakeChannel()));

  // Set Buffer collection constraints.
  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(
      display.DisplayEngineSetBufferCollectionConstraints(&kDisplayUsage, kBanjoCollectionId));

  auto [heap_client, heap_server] = fidl::Endpoints<fuchsia_hardware_sysmem::Heap>::Create();
  auto bind_ref = fidl::BindServer(env_loop.dispatcher(), std::move(heap_server), &display);
  fidl::WireSyncClient heap{std::move(heap_client)};

  fidl::Arena arena;
  // At least for now we use empty settings, because currently FramebufferDisplay doesn't pay
  // attention to any settings, so this way if that changes, this test will fail intentionally so
  // that this test can be updated to have settings that achieve this test's goals.
  auto settings = fuchsia_sysmem2::wire::SingleBufferSettings::Builder(arena);
  EXPECT_OK(heap->AllocateVmo(0, settings.Build(), kBanjoCollectionId, 0).status());

  bind_ref.Unbind();

  fake_sysmem.SetupFakeVmoInfo(kBanjoCollectionId, 0);

  // Invalid import: bad collection id
  static constexpr image_metadata_t kDisplayImageMetadata = {
      .dimensions = {.width = kWidthPx, .height = kHeightPx},
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  uint64_t kBanjoInvalidCollectionId = 100;
  uint64_t image_handle = 0;
  EXPECT_EQ(display.DisplayEngineImportImage(&kDisplayImageMetadata, kBanjoInvalidCollectionId, 0,
                                             &image_handle),
            ZX_ERR_NOT_FOUND);

  // Invalid import: bad index
  uint32_t kInvalidIndex = 100;
  image_handle = 0;
  EXPECT_EQ(display.DisplayEngineImportImage(&kDisplayImageMetadata, kBanjoCollectionId,
                                             kInvalidIndex, &image_handle),
            ZX_ERR_OUT_OF_RANGE);

  // Invalid import: bad width
  static constexpr image_metadata_t kImageMetadataWithIncorrectWidth = {
      .dimensions = {.width = kWidthPx * 2, .height = kHeightPx},
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  image_handle = 0;
  EXPECT_EQ(display.DisplayEngineImportImage(&kImageMetadataWithIncorrectWidth, kBanjoCollectionId,
                                             /*index=*/0, &image_handle),
            ZX_ERR_INVALID_ARGS);

  // Invalid import: bad height
  static constexpr image_metadata_t kImageMetadataWithIncorrectHeight = {
      .dimensions = {.width = kWidthPx, .height = kHeightPx * 2},
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  image_handle = 0;
  EXPECT_EQ(display.DisplayEngineImportImage(&kImageMetadataWithIncorrectHeight, kBanjoCollectionId,
                                             /*index=*/0, &image_handle),
            ZX_ERR_INVALID_ARGS);

  // Valid import
  image_handle = 0;
  EXPECT_OK(display.DisplayEngineImportImage(&kDisplayImageMetadata, kBanjoCollectionId,
                                             /*index=*/0, &image_handle));
  EXPECT_NE(image_handle, 0u);

  // Release buffer collection.
  EXPECT_OK(display.DisplayEngineReleaseBufferCollection(kBanjoCollectionId));

  env_loop.RunUntilIdle();

  // Shutdown the loop before destroying the FakeSysmem and MockAllocator which
  // may still have pending callbacks.
  env_loop.Shutdown();
  display_loop.Shutdown();
}

}  // namespace

}  // namespace framebuffer_display
