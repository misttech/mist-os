// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/gpu.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/wire_test_base.h>
#include <fidl/fuchsia.sysmem/cpp/wire_test_base.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/testing/cpp/real_loop.h>
#include <lib/fake-bti/bti.h>
#include <lib/fidl-async/cpp/bind.h>
#include <zircon/compiler.h>

#include "fidl/fuchsia.hardware.sysmem/cpp/markers.h"
#include "lib/fidl/cpp/wire/channel.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"

#define USE_GTEST
#include <lib/virtio/backends/fake.h>
#undef USE_GTEST

#include <list>

#include <fbl/auto_lock.h>
#include <gtest/gtest.h>

#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/testing/predicates/status.h"

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
    display::DriverBufferCollectionId buffer_collection_id = next_buffer_collection_id_++;
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

  mutable fbl::Mutex lock_;
  std::unordered_map<display::DriverBufferCollectionId, BufferCollection> active_buffer_collections_
      __TA_GUARDED(lock_);
  std::vector<fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>>
      inactive_buffer_collection_tokens_ __TA_GUARDED(lock_);

  display::DriverBufferCollectionId next_buffer_collection_id_ =
      display::DriverBufferCollectionId(0);
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

class VirtioGpuTest : public testing::Test, public loop_fixture::RealLoop {
 public:
  VirtioGpuTest() = default;
  void SetUp() override {
    fake_sysmem_ = std::make_unique<FakeSysmem>(dispatcher());

    zx::bti bti;
    fake_bti_create(bti.reset_and_get_address());
    device_ = std::make_unique<virtio::GpuDevice>(nullptr, std::move(bti),
                                                  std::make_unique<FakeGpuBackend>());

    zx::result<fidl::Endpoints<fuchsia_hardware_sysmem::Sysmem>> sysmem_endpoints =
        fidl::CreateEndpoints<fuchsia_hardware_sysmem::Sysmem>();
    ASSERT_OK(sysmem_endpoints.status_value());
    auto& [sysmem_client, sysmem_server] = sysmem_endpoints.value();
    fidl::BindSingleInFlightOnly(dispatcher(), std::move(sysmem_server), fake_sysmem_.get());

    ASSERT_OK(device_->SetAndInitSysmemForTesting(fidl::WireSyncClient(std::move(sysmem_client))));

    RunLoopUntilIdle();
  }

  void TearDown() override {
    // Ensure the loop processes all queued FIDL messages.
    loop().Shutdown();
  }

  void ImportBufferCollection(display::DriverBufferCollectionId buffer_collection_id) {
    zx::result token_endpoints = fidl::CreateEndpoints<sysmem::BufferCollectionToken>();
    ASSERT_TRUE(token_endpoints.is_ok());
    EXPECT_OK(device_->DisplayControllerImplImportBufferCollection(
        display::ToBanjoDriverBufferCollectionId(buffer_collection_id),
        token_endpoints->client.TakeChannel()));
  }

 protected:
  std::unique_ptr<FakeSysmem> fake_sysmem_;
  std::unique_ptr<virtio::GpuDevice> device_;
};

TEST_F(VirtioGpuTest, ImportVmo) {
  display_controller_impl_protocol_t proto;
  EXPECT_OK(device_->DdkGetProtocol(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL,
                                    reinterpret_cast<void*>(&proto)));

  // Import buffer collection.
  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  ImportBufferCollection(kBufferCollectionId);

  // Set buffer collection constraints.
  const image_t kDefaultImage = {
      .width = 4,
      .height = 4,
      .type = IMAGE_TYPE_SIMPLE,
      .handle = 0,
  };
  EXPECT_OK(proto.ops->set_buffer_collection_constraints(device_.get(), &kDefaultImage,
                                                         kBanjoBufferCollectionId));
  RunLoopUntilIdle();

  PerformBlockingWork([&] {
    zx::result<virtio::GpuDevice::BufferInfo> buffer_info_result =
        device_->GetAllocatedBufferInfoForImage(kBufferCollectionId, /*index=*/0, &kDefaultImage);
    ASSERT_OK(buffer_info_result.status_value());

    const auto& buffer_info = buffer_info_result.value();
    EXPECT_EQ(4u, buffer_info.bytes_per_pixel);
    EXPECT_EQ(16u, buffer_info.bytes_per_row);
  });
}

TEST_F(VirtioGpuTest, SetConstraints) {
  display_controller_impl_protocol_t proto;
  EXPECT_OK(device_->DdkGetProtocol(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL,
                                    reinterpret_cast<void*>(&proto)));

  // Import buffer collection.
  zx::result token_endpoints = fidl::CreateEndpoints<sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());
  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  EXPECT_OK(proto.ops->import_buffer_collection(device_.get(), kBanjoBufferCollectionId,
                                                token_endpoints->client.handle()->get()));
  RunLoopUntilIdle();

  // Set buffer collection constraints.
  const image_t kDefaultImage = {
      .width = 4,
      .height = 4,
      .type = IMAGE_TYPE_SIMPLE,
      .handle = 0,
  };
  EXPECT_OK(proto.ops->set_buffer_collection_constraints(device_.get(), &kDefaultImage,
                                                         kBanjoBufferCollectionId));
  RunLoopUntilIdle();
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
  // This allocator is expected to be alive as long as the `device_` is
  // available, so it should outlive the test body.
  const MockAllocator* allocator = fake_sysmem_->GetLatestMockAllocator();

  zx::result token1_endpoints = fidl::CreateEndpoints<sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token1_endpoints.is_ok());
  zx::result token2_endpoints = fidl::CreateEndpoints<sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token2_endpoints.is_ok());

  display_controller_impl_protocol_t proto;
  EXPECT_OK(device_->DdkGetProtocol(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL,
                                    reinterpret_cast<void*>(&proto)));

  // Test ImportBufferCollection().
  constexpr display::DriverBufferCollectionId kValidBufferCollectionId(1);
  constexpr uint64_t kBanjoValidBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kValidBufferCollectionId);
  EXPECT_OK(proto.ops->import_buffer_collection(device_.get(), kBanjoValidBufferCollectionId,
                                                token1_endpoints->client.handle()->get()));

  // `collection_id` must be unused.
  EXPECT_EQ(proto.ops->import_buffer_collection(device_.get(), kBanjoValidBufferCollectionId,
                                                token2_endpoints->client.handle()->get()),
            ZX_ERR_ALREADY_EXISTS);

  RunLoopUntilIdle();
  EXPECT_TRUE(!allocator->GetActiveBufferCollectionTokenClients().empty());

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
  constexpr display::DriverBufferCollectionId kInvalidBufferCollectionId(2);
  constexpr uint64_t kBanjoInvalidBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kInvalidBufferCollectionId);
  EXPECT_EQ(proto.ops->release_buffer_collection(device_.get(), kBanjoInvalidBufferCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(proto.ops->release_buffer_collection(device_.get(), kBanjoValidBufferCollectionId));

  RunLoopUntilIdle();
  EXPECT_TRUE(allocator->GetActiveBufferCollectionTokenClients().empty());

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

TEST_F(VirtioGpuTest, ImportImage) {
  // This allocator is expected to be alive as long as the `device_` is
  // available, so it should outlive the test body.
  const MockAllocator* allocator = fake_sysmem_->GetLatestMockAllocator();

  zx::result token1_endpoints = fidl::CreateEndpoints<sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token1_endpoints.is_ok());

  display_controller_impl_protocol_t proto;
  EXPECT_OK(device_->DdkGetProtocol(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL,
                                    reinterpret_cast<void*>(&proto)));

  // Import buffer collection.
  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  EXPECT_OK(proto.ops->import_buffer_collection(device_.get(), kBanjoBufferCollectionId,
                                                token1_endpoints->client.handle()->get()));

  RunLoopUntilIdle();
  EXPECT_TRUE(!allocator->GetActiveBufferCollectionTokenClients().empty());

  // Set buffer collection constraints.
  const image_t kDefaultImage = {
      .width = 800,
      .height = 600,
      .type = IMAGE_TYPE_SIMPLE,
      .handle = 0,
  };
  EXPECT_OK(proto.ops->set_buffer_collection_constraints(device_.get(), &kDefaultImage,
                                                         kBanjoBufferCollectionId));
  RunLoopUntilIdle();

  // Invalid import: bad collection id
  image_t invalid_image = kDefaultImage;
  constexpr display::DriverBufferCollectionId kInvalidCollectionId(100);
  constexpr uint64_t kBanjoInvalidCollectionId =
      display::ToBanjoDriverBufferCollectionId(kInvalidCollectionId);
  PerformBlockingWork([&] {
    EXPECT_EQ(proto.ops->import_image(device_.get(), &invalid_image, kBanjoInvalidCollectionId,
                                      /*index=*/0),
              ZX_ERR_NOT_FOUND);
  });

  // Invalid import: bad index
  invalid_image = kDefaultImage;
  uint32_t kInvalidIndex = 100;
  PerformBlockingWork([&] {
    EXPECT_EQ(proto.ops->import_image(device_.get(), &invalid_image, kBanjoBufferCollectionId,
                                      kInvalidIndex),
              ZX_ERR_OUT_OF_RANGE);
  });

  // TODO(fxbug.dev/122727): Implement fake ring-buffer based tests so that we
  // can test the valid import case.

  // Release buffer collection.
  EXPECT_OK(proto.ops->release_buffer_collection(device_.get(), kBanjoBufferCollectionId));

  RunLoopUntilIdle();
  EXPECT_TRUE(allocator->GetActiveBufferCollectionTokenClients().empty());
}

}  // namespace
