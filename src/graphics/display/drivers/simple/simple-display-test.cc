// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/simple/simple-display.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire_test_base.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire_test_base.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/wait.h>
#include <lib/zx/object.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/rights.h>

#include <list>
#include <memory>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/testing/predicates/status.h"

namespace {

// TODO(fxbug.dev/121924): Consider creating and using a unified set of sysmem
// testing doubles instead of writing mocks for each display driver test.
class FakeBufferCollection : public fidl::testing::WireTestBase<fuchsia_sysmem::BufferCollection> {
 public:
  explicit FakeBufferCollection(zx::unowned_vmo framebuffer_vmo)
      : framebuffer_vmo_(std::move(framebuffer_vmo)) {}

  void SetConstraints(::fuchsia_sysmem::wire::BufferCollectionSetConstraintsRequest* request,
                      SetConstraintsCompleter::Sync& completer) override {}
  void CheckBuffersAllocated(CheckBuffersAllocatedCompleter::Sync& completer) override {
    completer.Reply(ZX_OK);
  }
  void WaitForBuffersAllocated(WaitForBuffersAllocatedCompleter::Sync& completer) override {
    zx::vmo vmo;
    EXPECT_OK(framebuffer_vmo_->duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));

    completer.Reply(ZX_OK,
                    {
                        .buffer_count = 1,
                        .settings =
                            {
                                .buffer_settings =
                                    {
                                        .heap = fuchsia_sysmem::HeapType::kFramebuffer,
                                    },
                                .has_image_format_constraints = true,
                                .image_format_constraints =
                                    {
                                        .pixel_format =
                                            {
                                                .type = fuchsia_sysmem::PixelFormatType::kBgra32,
                                                .has_format_modifier = true,
                                                .format_modifier =
                                                    {fuchsia_sysmem::wire::kFormatModifierLinear},
                                            },
                                    },
                            },
                        .buffers =
                            {
                                fuchsia_sysmem::wire::VmoBuffer{std::move(vmo), 0},
                            },
                    });
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {}

 private:
  zx::unowned_vmo framebuffer_vmo_;
};

class MockAllocator : public fidl::testing::WireTestBase<fuchsia_sysmem::Allocator> {
 public:
  explicit MockAllocator(async_dispatcher_t* dispatcher, zx::unowned_vmo framebuffer_vmo)
      : dispatcher_(dispatcher), framebuffer_vmo_(std::move(framebuffer_vmo)) {
    ZX_ASSERT(dispatcher_);
  }

  void BindSharedCollection(BindSharedCollectionRequestView request,
                            BindSharedCollectionCompleter::Sync& completer) override {
    auto buffer_collection_id = next_buffer_collection_id_++;
    active_buffer_collections_.emplace(
        buffer_collection_id,
        BufferCollection{
            .token_client = std::move(request->token),
            .unowned_collection_server = request->buffer_collection_request,
            .fake_buffer_collection = FakeBufferCollection(framebuffer_vmo_->borrow())});
    fidl::BindServer(
        dispatcher_, std::move(request->buffer_collection_request),
        &active_buffer_collections_.at(buffer_collection_id).fake_buffer_collection,
        [this, buffer_collection_id](FakeBufferCollection*, fidl::UnbindInfo,
                                     fidl::ServerEnd<fuchsia_sysmem::BufferCollection>) {
          inactive_buffer_collection_tokens_.push_back(
              std::move(active_buffer_collections_.at(buffer_collection_id).token_client));
          active_buffer_collections_.erase(buffer_collection_id);
        });
  }

  std::vector<std::pair<fidl::UnownedClientEnd<fuchsia_sysmem::BufferCollectionToken>,
                        fidl::UnownedServerEnd<fuchsia_sysmem::BufferCollection>>>
  GetBufferCollectionConnections() {
    if (active_buffer_collections_.empty()) {
      return {};
    }

    std::vector<std::pair<fidl::UnownedClientEnd<fuchsia_sysmem::BufferCollectionToken>,
                          fidl::UnownedServerEnd<fuchsia_sysmem::BufferCollection>>>
        result;
    for (const auto& kv : active_buffer_collections_) {
      result.emplace_back(kv.second.token_client, kv.second.unowned_collection_server);
    }
    return result;
  }

  void SetDebugClientInfo(SetDebugClientInfoRequestView request,
                          SetDebugClientInfoCompleter::Sync& completer) override {
    EXPECT_EQ(request->name.get().find("simple-display"), 0u);
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    zxlogf(ERROR, "%s not implemented", name.c_str());
    EXPECT_TRUE(false);
  }

 private:
  struct BufferCollection {
    fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken> token_client;
    fidl::UnownedServerEnd<fuchsia_sysmem::BufferCollection> unowned_collection_server;
    FakeBufferCollection fake_buffer_collection;
  };

  using BufferCollectionId = int;

  std::unordered_map<BufferCollectionId, BufferCollection> active_buffer_collections_;
  std::vector<fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>>
      inactive_buffer_collection_tokens_;

  BufferCollectionId next_buffer_collection_id_ = 0;
  async_dispatcher_t* dispatcher_ = nullptr;
  zx::unowned_vmo framebuffer_vmo_;
};

class FakeSysmem : public fidl::testing::WireTestBase<fuchsia_hardware_sysmem::Sysmem> {
 public:
  explicit FakeSysmem(async_dispatcher_t* dispatcher, zx::unowned_vmo framebuffer_vmo)
      : dispatcher_(dispatcher), framebuffer_vmo_(std::move(framebuffer_vmo)) {
    EXPECT_TRUE(dispatcher_);
  }

  void ConnectServer(ConnectServerRequestView request,
                     ConnectServerCompleter::Sync& completer) override {
    mock_allocators_.emplace_front(dispatcher_, framebuffer_vmo_->borrow());
    auto it = mock_allocators_.begin();
    fidl::BindServer(dispatcher_, std::move(request->allocator_request), &*it);
  }

  std::list<MockAllocator>& mock_allocators() { return mock_allocators_; }

  // FIDL methods
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  std::list<MockAllocator> mock_allocators_;
  async_dispatcher_t* dispatcher_ = nullptr;
  zx::unowned_vmo framebuffer_vmo_ = {};
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

TEST(SimpleDisplay, ImportBufferCollection) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FakeSysmem fake_sysmem(loop.dispatcher(), /*framebuffer_vmo=*/{});
  FakeMmio fake_mmio;

  auto sysmem_endpoints = fidl::CreateEndpoints<fuchsia_hardware_sysmem::Sysmem>();
  ASSERT_TRUE(sysmem_endpoints.is_ok());
  auto& [sysmem_client, sysmem_server] = sysmem_endpoints.value();
  fidl::BindServer(loop.dispatcher(), std::move(sysmem_server), &fake_sysmem);

  constexpr uint32_t kWidth = 800;
  constexpr uint32_t kHeight = 600;
  constexpr uint32_t kStride = 800;
  constexpr auto kPixelFormat = fuchsia_images2::wire::PixelFormat::kBgra32;

  SimpleDisplay display(nullptr, fidl::WireSyncClient(std::move(sysmem_client)),
                        fake_mmio.MmioBuffer(), kWidth, kHeight, kStride, kPixelFormat);

  zx::result token1_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token1_endpoints.is_ok());
  zx::result token2_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token2_endpoints.is_ok());

  // Test ImportBufferCollection().
  const display::DriverBufferCollectionId kValidCollectionId(1);
  const uint64_t kBanjoValidCollectionId =
      display::ToBanjoDriverBufferCollectionId(kValidCollectionId);
  EXPECT_OK(display.DisplayControllerImplImportBufferCollection(
      kBanjoValidCollectionId, token1_endpoints->client.TakeChannel()));

  // `collection_id` must be unused.
  EXPECT_EQ(display.DisplayControllerImplImportBufferCollection(
                kBanjoValidCollectionId, token2_endpoints->client.TakeChannel()),
            ZX_ERR_ALREADY_EXISTS);

  loop.RunUntilIdle();

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

    const auto& buffer_collection_token_server = token1_endpoints->server;
    const auto& buffer_collection_token_client = buffer_collection_connections[0].first;
    ExpectObjectsArePaired(buffer_collection_token_server.handle(),
                           buffer_collection_token_client.handle());
  }

  // Test ReleaseBufferCollection().
  const uint64_t kBanjoInvalidCollectionId = 2u;
  EXPECT_EQ(display.DisplayControllerImplReleaseBufferCollection(kBanjoInvalidCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display.DisplayControllerImplReleaseBufferCollection(kBanjoValidCollectionId));

  loop.RunUntilIdle();

  // Verify that the current buffer collection token is released.
  {
    const std::vector buffer_collection_connections = allocator.GetBufferCollectionConnections();
    ASSERT_EQ(buffer_collection_connections.size(), 0u);
  }

  // Shutdown the loop before destroying the FakeSysmem and MockAllocator which
  // may still have pending callbacks.
  loop.Shutdown();
}

TEST(SimpleDisplay, ImportKernelFramebufferImage) {
  constexpr uint32_t kWidth = 800;
  constexpr uint32_t kHeight = 600;
  constexpr uint32_t kStride = 800;
  constexpr auto kPixelFormat = fuchsia_images2::wire::PixelFormat::kBgra32;
  constexpr size_t kBytesPerPixel = 4;

  // `framebuffer_vmo` must outlive `fake_sysmem`.
  zx::vmo framebuffer_vmo;
  size_t kImageBytes = uint64_t{kStride} * kHeight * kBytesPerPixel;
  EXPECT_OK(zx::vmo::create(/*size=*/kImageBytes, /*options=*/0, &framebuffer_vmo));

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  FakeSysmem fake_sysmem(loop.dispatcher(), framebuffer_vmo.borrow());
  FakeMmio fake_mmio;

  loop.StartThread("sysmem loop");

  auto sysmem_endpoints = fidl::CreateEndpoints<fuchsia_hardware_sysmem::Sysmem>();
  ASSERT_TRUE(sysmem_endpoints.is_ok());
  auto& [sysmem_client, sysmem_server] = sysmem_endpoints.value();
  fidl::BindServer(loop.dispatcher(), std::move(sysmem_server), &fake_sysmem);

  SimpleDisplay display(nullptr, fidl::WireSyncClient(std::move(sysmem_client)),
                        fake_mmio.MmioBuffer(), kWidth, kHeight, kStride, kPixelFormat);

  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());

  // Import BufferCollection.
  const uint64_t kBanjoCollectionId = 1u;
  EXPECT_OK(display.DisplayControllerImplImportBufferCollection(
      kBanjoCollectionId, token_endpoints->client.TakeChannel()));

  // Import kernel framebuffer.
  zx::vmo heap_vmo;
  ASSERT_OK(framebuffer_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &heap_vmo));

  zx::result heap_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::Heap>();
  ASSERT_TRUE(heap_endpoints.is_ok());
  auto& [heap_client, heap_server] = heap_endpoints.value();
  auto bind_ref = fidl::BindServer(loop.dispatcher(), std::move(heap_server), &display);
  fidl::WireSyncClient heap{std::move(heap_client)};
  EXPECT_OK(heap->CreateResource(std::move(heap_vmo), {}).status());
  bind_ref.Unbind();

  // Set Buffer collection constraints.
  const image_t kDefaultImage = {
      .width = kWidth,
      .height = kHeight,
      .type = IMAGE_TYPE_SIMPLE,
      .handle = 0,
  };
  EXPECT_OK(display.DisplayControllerImplSetBufferCollectionConstraints(&kDefaultImage,
                                                                        kBanjoCollectionId));

  // Invalid import: bad collection id
  image_t invalid_image = kDefaultImage;
  uint64_t kBanjoInvalidCollectionId = 100;
  EXPECT_EQ(display.DisplayControllerImplImportImage(&invalid_image, kBanjoInvalidCollectionId, 0),
            ZX_ERR_NOT_FOUND);

  // Invalid import: bad index
  invalid_image = kDefaultImage;
  uint32_t kInvalidIndex = 100;
  EXPECT_EQ(
      display.DisplayControllerImplImportImage(&invalid_image, kBanjoCollectionId, kInvalidIndex),
      ZX_ERR_OUT_OF_RANGE);

  // Invalid import: bad width
  invalid_image = kDefaultImage;
  invalid_image.width = invalid_image.width * 2;
  EXPECT_EQ(
      display.DisplayControllerImplImportImage(&invalid_image, kBanjoCollectionId, /*index=*/0),
      ZX_ERR_INVALID_ARGS);

  // Invalid import: bad height
  invalid_image = kDefaultImage;
  invalid_image.height = invalid_image.height * 2;
  EXPECT_EQ(
      display.DisplayControllerImplImportImage(&invalid_image, kBanjoCollectionId, /*index=*/0),
      ZX_ERR_INVALID_ARGS);

  // Valid import
  image_t valid_image = kDefaultImage;
  EXPECT_EQ(valid_image.handle, 0u);
  EXPECT_OK(
      display.DisplayControllerImplImportImage(&valid_image, kBanjoCollectionId, /*index=*/0));
  EXPECT_NE(valid_image.handle, 0u);

  // Release buffer collection.
  EXPECT_OK(display.DisplayControllerImplReleaseBufferCollection(kBanjoCollectionId));

  loop.RunUntilIdle();

  // Shutdown the loop before destroying the FakeSysmem and MockAllocator which
  // may still have pending callbacks.
  loop.Shutdown();
}

}  // namespace
