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

#include <list>
#include <memory>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <zxtest/zxtest.h>

#include "src/lib/fsl/handles/object_info.h"

namespace {

// TODO(fxbug.dev/121924): Consider creating and using a unified set of sysmem
// testing doubles instead of writing mocks for each display driver test.
class FakeBufferCollection : public fidl::testing::WireTestBase<fuchsia_sysmem::BufferCollection> {
 public:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {}
};

class MockAllocator : public fidl::testing::WireTestBase<fuchsia_sysmem::Allocator> {
 public:
  explicit MockAllocator(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {
    ASSERT_TRUE(dispatcher_);
  }

  void BindSharedCollection(BindSharedCollectionRequestView request,
                            BindSharedCollectionCompleter::Sync& completer) override {
    auto buffer_collection_id = next_buffer_collection_id_++;
    active_buffer_collections_.emplace(
        buffer_collection_id,
        BufferCollection{.token_client = std::move(request->token),
                         .unowned_collection_server = request->buffer_collection_request,
                         .fake_buffer_collection = FakeBufferCollection()});
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
};

class FakeSysmem : public fidl::testing::WireTestBase<fuchsia_hardware_sysmem::Sysmem> {
 public:
  explicit FakeSysmem(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {
    EXPECT_TRUE(dispatcher_);
  }

  void ConnectServer(ConnectServerRequestView request,
                     ConnectServerCompleter::Sync& completer) override {
    mock_allocators_.emplace_front(dispatcher_);
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
};

class FakeMmio {
 public:
  FakeMmio() {
    regs_ = std::make_unique<ddk_fake::FakeMmioReg[]>(kRegArrayLength);
    mmio_ = std::make_unique<ddk_fake::FakeMmioRegRegion>(regs_.get(), sizeof(uint32_t),
                                                          kRegArrayLength);
  }

  fdf::MmioBuffer MmioBuffer() { return fdf::MmioBuffer(mmio_->GetMmioBuffer()); }

  ddk_fake::FakeMmioReg& FakeRegister(size_t address) { return regs_[address >> 2]; }

 private:
  static constexpr size_t kMmioBufferSize = 0x5000;
  static constexpr size_t kRegArrayLength = kMmioBufferSize / sizeof(uint32_t);
  std::unique_ptr<ddk_fake::FakeMmioReg[]> regs_;
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
  FakeSysmem fake_sysmem(loop.dispatcher());
  FakeMmio fake_mmio;

  auto sysmem_endpoints = fidl::CreateEndpoints<fuchsia_hardware_sysmem::Sysmem>();
  ASSERT_TRUE(sysmem_endpoints.is_ok());
  auto& [sysmem_client, sysmem_server] = sysmem_endpoints.value();
  fidl::BindServer(loop.dispatcher(), std::move(sysmem_server), &fake_sysmem);

  constexpr uint32_t kWidth = 800;
  constexpr uint32_t kHeight = 600;
  constexpr uint32_t kStride = 800;
  constexpr zx_pixel_format_t kPixelFormat = ZX_PIXEL_FORMAT_RGB_x888;

  SimpleDisplay display(nullptr, fidl::WireSyncClient(std::move(sysmem_client)),
                        fake_mmio.MmioBuffer(), kWidth, kHeight, kStride, kPixelFormat);

  zx::result token1_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token1_endpoints.is_ok());
  zx::result token2_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token2_endpoints.is_ok());

  // Test ImportBufferCollection().
  const uint64_t kValidCollectionId = 1u;
  EXPECT_OK(display.DisplayControllerImplImportBufferCollection(
      kValidCollectionId, token1_endpoints->client.TakeChannel()));

  // `collection_id` must be unused.
  EXPECT_EQ(display.DisplayControllerImplImportBufferCollection(
                kValidCollectionId, token2_endpoints->client.TakeChannel()),
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
  const uint64_t kInvalidCollectionId = 2u;
  EXPECT_EQ(display.DisplayControllerImplReleaseBufferCollection(kInvalidCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display.DisplayControllerImplReleaseBufferCollection(kValidCollectionId));

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

}  // namespace
