// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpu.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/wire_test_base.h>
#include <fidl/fuchsia.sysmem/cpp/wire_test_base.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fake-bti/bti.h>
#include <lib/fidl-async/cpp/bind.h>
#include <lib/virtio/backends/fake.h>
#include <zircon/compiler.h>

#include <list>

#include <fbl/auto_lock.h>
#include <zxtest/zxtest.h>

#include "src/lib/fsl/handles/object_info.h"

namespace sysmem = fuchsia_sysmem;

namespace {
// Use a stub buffer collection instead of the real sysmem since some tests may
// require things that aren't available on the current system.
//
// TODO(fxbug.dev/121924): Consider creating and using a unified set of sysmem
// testing doubles instead of writing mocks for each display driver test.
class StubBufferCollection : public fidl::testing::WireTestBase<fuchsia_sysmem::BufferCollection> {
 public:
  void SetConstraints(SetConstraintsRequestView request,
                      SetConstraintsCompleter::Sync& _completer) override {
    if (!request->has_constraints) {
      return;
    }
    auto& image_constraints = request->constraints.image_format_constraints[0];
    EXPECT_EQ(sysmem::wire::PixelFormatType::kBgra32, image_constraints.pixel_format.type);
    EXPECT_EQ(4u, image_constraints.bytes_per_row_divisor);
  }

  void CheckBuffersAllocated(CheckBuffersAllocatedCompleter::Sync& completer) override {
    completer.Reply(ZX_OK);
  }

  void WaitForBuffersAllocated(WaitForBuffersAllocatedCompleter::Sync& _completer) override {
    sysmem::wire::BufferCollectionInfo2 info;
    info.settings.has_image_format_constraints = true;
    info.buffer_count = 1;
    ASSERT_OK(zx::vmo::create(4096, 0, &info.buffers[0].vmo));
    sysmem::wire::ImageFormatConstraints& constraints = info.settings.image_format_constraints;
    constraints.pixel_format.type = sysmem::wire::PixelFormatType::kBgra32;
    constraints.pixel_format.has_format_modifier = true;
    constraints.pixel_format.format_modifier.value = sysmem::wire::kFormatModifierLinear;
    constraints.max_coded_width = 1000;
    constraints.max_bytes_per_row = 4000;
    constraints.bytes_per_row_divisor = 1;
    _completer.Reply(ZX_OK, std::move(info));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    EXPECT_TRUE(false);
  }
};

class MockAllocator : public fidl::testing::WireTestBase<fuchsia_sysmem::Allocator> {
 public:
  explicit MockAllocator(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {
    EXPECT_TRUE(dispatcher_);
  }

  void BindSharedCollection(BindSharedCollectionRequestView request,
                            BindSharedCollectionCompleter::Sync& completer) override {
    auto buffer_collection_id = next_buffer_collection_id_++;
    fbl::AutoLock lock(&lock_);
    active_buffer_collections_.emplace(
        buffer_collection_id,
        BufferCollection{.token_client = std::move(request->token),
                         .unowned_collection_server = request->buffer_collection_request,
                         .mock_buffer_collection = std::make_unique<StubBufferCollection>()});

    auto ref = fidl::BindServer(
        dispatcher_, std::move(request->buffer_collection_request),
        active_buffer_collections_.at(buffer_collection_id).mock_buffer_collection.get(),
        [this, buffer_collection_id](StubBufferCollection*, fidl::UnbindInfo,
                                     fidl::ServerEnd<fuchsia_sysmem::BufferCollection>) {
          fbl::AutoLock lock(&lock_);
          inactive_buffer_collection_tokens_.push_back(
              std::move(active_buffer_collections_.at(buffer_collection_id).token_client));
          active_buffer_collections_.erase(buffer_collection_id);
        });
  }

  void SetDebugClientInfo(SetDebugClientInfoRequestView request,
                          SetDebugClientInfoCompleter::Sync& completer) override {
    EXPECT_EQ(request->name.get().find("virtio-gpu-display"), 0u);
  }

  std::vector<fidl::UnownedClientEnd<fuchsia_sysmem::BufferCollectionToken>>
  GetActiveBufferCollectionTokenClients() const {
    fbl::AutoLock lock(&lock_);
    std::vector<fidl::UnownedClientEnd<fuchsia_sysmem::BufferCollectionToken>>
        unowned_token_clients;
    unowned_token_clients.reserve(active_buffer_collections_.size());

    for (const auto& kv : active_buffer_collections_) {
      unowned_token_clients.push_back(kv.second.token_client);
    }
    return unowned_token_clients;
  }

  std::vector<fidl::UnownedClientEnd<fuchsia_sysmem::BufferCollectionToken>>
  GetInactiveBufferCollectionTokenClients() const {
    fbl::AutoLock lock(&lock_);
    std::vector<fidl::UnownedClientEnd<fuchsia_sysmem::BufferCollectionToken>>
        unowned_token_clients;
    unowned_token_clients.reserve(inactive_buffer_collection_tokens_.size());

    for (const auto& token : inactive_buffer_collection_tokens_) {
      unowned_token_clients.push_back(token);
    }
    return unowned_token_clients;
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    EXPECT_TRUE(false);
  }

 private:
  struct BufferCollection {
    fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken> token_client;
    fidl::UnownedServerEnd<fuchsia_sysmem::BufferCollection> unowned_collection_server;
    std::unique_ptr<StubBufferCollection> mock_buffer_collection;
  };

  using BufferCollectionId = int;

  mutable fbl::Mutex lock_;
  std::unordered_map<BufferCollectionId, BufferCollection> active_buffer_collections_
      __TA_GUARDED(lock_);
  std::vector<fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>>
      inactive_buffer_collection_tokens_ __TA_GUARDED(lock_);

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
    fbl::AutoLock lock(&lock_);
    mock_allocators_.emplace_front(dispatcher_);
    auto it = mock_allocators_.begin();
    fidl::BindServer(dispatcher_, std::move(request->allocator_request), &*it);
  }

  const MockAllocator* GetLatestMockAllocator() const {
    // Note that the created MockAllocators are not destroyed until FakeSysmem
    // is destroyed; so it's always safe to access the returned allocator in the
    // test body.
    fbl::AutoLock lock(&lock_);
    return mock_allocators_.empty() ? nullptr : &mock_allocators_.front();
  }

  // FIDL methods
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  mutable fbl::Mutex lock_;
  std::list<MockAllocator> mock_allocators_ __TA_GUARDED(lock_);
  async_dispatcher_t* dispatcher_ = nullptr;
};

class FakeGpuBackend : public virtio::FakeBackend {
 public:
  FakeGpuBackend() : FakeBackend({{0, 1024}}) {}
};

class VirtioGpuTest : public zxtest::Test {
 public:
  VirtioGpuTest()
      : loop_(&kAsyncLoopConfigAttachToCurrentThread), fake_sysmem_(loop_.dispatcher()) {}
  void SetUp() override {
    zx::bti bti;
    fake_bti_create(bti.reset_and_get_address());
    device_ = std::make_unique<virtio::GpuDevice>(nullptr, std::move(bti),
                                                  std::make_unique<FakeGpuBackend>());

    // TODO(fxbug.dev/121411): Remove once SetBufferCollectionConstraints()
    // doesn't use borrowed client channel anymore.
    zx::result server_channel = fidl::CreateEndpoints(&client_channel_);
    ASSERT_OK(server_channel);
    ASSERT_OK(fidl::BindSingleInFlightOnly(loop_.dispatcher(), std::move(server_channel.value()),
                                           &collection_));

    zx::result sysmem_endpoints = fidl::CreateEndpoints<fuchsia_hardware_sysmem::Sysmem>();
    ASSERT_OK(sysmem_endpoints);
    auto& [sysmem_client, sysmem_server] = sysmem_endpoints.value();
    fidl::BindSingleInFlightOnly(loop_.dispatcher(), std::move(sysmem_server), &fake_sysmem_);

    loop_.StartThread();

    ASSERT_OK(device_->SetAndInitSysmemForTesting(fidl::WireSyncClient(std::move(sysmem_client))));
  }
  void TearDown() override {
    // Ensure the loop processes all queued FIDL messages.
    loop_.Quit();
    loop_.JoinThreads();
    loop_.ResetQuit();
    loop_.RunUntilIdle();
  }

 protected:
  std::unique_ptr<virtio::GpuDevice> device_;

  async::Loop loop_;

  // TODO(fxbug.dev/121411): Remove once SetBufferCollectionConstraints()
  // doesn't use borrowed client channel anymore.
  StubBufferCollection collection_;
  fidl::ClientEnd<fuchsia_sysmem::BufferCollection> client_channel_;

  FakeSysmem fake_sysmem_;
};

template <typename Lambda>
bool PollUntil(Lambda predicate, zx::duration poll_interval, int max_intervals) {
  ZX_ASSERT(max_intervals >= 0);

  for (int sleeps_left = max_intervals; sleeps_left > 0; --sleeps_left) {
    if (predicate())
      return true;
    zx::nanosleep(zx::deadline_after(poll_interval));
  }

  return predicate();
}

TEST_F(VirtioGpuTest, ImportVmo) {
  image_t image = {};
  image.pixel_format = ZX_PIXEL_FORMAT_RGB_x888;
  image.width = 4;
  image.height = 4;

  zx::vmo vmo;
  size_t offset;
  uint32_t pixel_size;
  uint32_t row_bytes;
  EXPECT_OK(
      device_->GetVmoAndStride(&image, client_channel_, 0, &vmo, &offset, &pixel_size, &row_bytes));
  EXPECT_EQ(4, pixel_size);
  EXPECT_EQ(16, row_bytes);
}

TEST_F(VirtioGpuTest, SetConstraints) {
  image_t image = {};
  image.pixel_format = ZX_PIXEL_FORMAT_RGB_x888;
  image.width = 4;
  image.height = 4;
  display_controller_impl_protocol_t proto;
  EXPECT_OK(device_->DdkGetProtocol(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL,
                                    reinterpret_cast<void*>(&proto)));
  EXPECT_OK(proto.ops->set_buffer_collection_constraints(device_.get(), &image,
                                                         client_channel_.channel().get()));
}

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

TEST_F(VirtioGpuTest, ImportBufferCollection) {
  EXPECT_TRUE(PollUntil([&]() { return fake_sysmem_.GetLatestMockAllocator() != nullptr; },
                        zx::msec(5), 1000));

  // This allocator is expected to be alive as long as the `device_` is
  // available, so it should outlive the test body.
  const MockAllocator* allocator = fake_sysmem_.GetLatestMockAllocator();

  zx::result token1_endpoints = fidl::CreateEndpoints<sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token1_endpoints.is_ok());
  zx::result token2_endpoints = fidl::CreateEndpoints<sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token2_endpoints.is_ok());

  display_controller_impl_protocol_t proto;
  EXPECT_OK(device_->DdkGetProtocol(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL,
                                    reinterpret_cast<void*>(&proto)));

  // Test ImportBufferCollection().
  constexpr uint64_t kValidBufferCollectionId = 1u;
  EXPECT_OK(proto.ops->import_buffer_collection(device_.get(), kValidBufferCollectionId,
                                                token1_endpoints->client.handle()->get()));

  // `collection_id` must be unused.
  EXPECT_EQ(proto.ops->import_buffer_collection(device_.get(), kValidBufferCollectionId,
                                                token2_endpoints->client.handle()->get()),
            ZX_ERR_ALREADY_EXISTS);

  EXPECT_TRUE(
      PollUntil([&]() { return !allocator->GetActiveBufferCollectionTokenClients().empty(); },
                zx::msec(5), 1000));

  // Verify that the current buffer collection token is used.
  {
    auto active_buffer_token_clients = allocator->GetActiveBufferCollectionTokenClients();
    EXPECT_EQ(active_buffer_token_clients.size(), 1u);

    auto inactive_buffer_token_clients = allocator->GetInactiveBufferCollectionTokenClients();
    EXPECT_EQ(inactive_buffer_token_clients.size(), 0u);

    ExpectObjectsArePaired(active_buffer_token_clients[0].channel(),
                           token1_endpoints->server.channel().borrow());
  }

  // Test ReleaseBufferCollection().
  constexpr uint64_t kInvalidBufferCollectionId = 2u;
  EXPECT_EQ(proto.ops->release_buffer_collection(device_.get(), kInvalidBufferCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(proto.ops->release_buffer_collection(device_.get(), kValidBufferCollectionId));

  EXPECT_TRUE(
      PollUntil([&]() { return allocator->GetActiveBufferCollectionTokenClients().empty(); },
                zx::msec(5), 1000));

  // Verify that the current buffer collection token is released.
  {
    auto active_buffer_token_clients = allocator->GetActiveBufferCollectionTokenClients();
    EXPECT_EQ(active_buffer_token_clients.size(), 0u);

    auto inactive_buffer_token_clients = allocator->GetInactiveBufferCollectionTokenClients();
    EXPECT_EQ(inactive_buffer_token_clients.size(), 1u);

    ExpectObjectsArePaired(inactive_buffer_token_clients[0].channel(),
                           token1_endpoints->server.channel().borrow());
  }
}

}  // namespace
