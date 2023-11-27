// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v2/gpu.h"

#include <fidl/fuchsia.sysmem/cpp/wire_test_base.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdf/env.h>
#include <lib/fdf/testing.h>
#include <lib/syslog/cpp/macros.h>

#include <fbl/auto_lock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
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
  zx_status_t Connect(fidl::ServerEnd<sysmem::Allocator> request) {
    binding_group_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(request),
                              this, fidl::kIgnoreBindingClosure);
    return ZX_OK;
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
        fdf::Dispatcher::GetCurrent()->async_dispatcher(),
        std::move(request->buffer_collection_request),
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
  fidl::ServerBindingGroup<sysmem::Allocator> binding_group_;

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
};

class VirtioGpuTest : public ::testing::Test {
 public:
  VirtioGpuTest() = default;
  void SetUp() override {
    // Create start args
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment
    zx::result init_result =
        test_environment_.SyncCall(&fdf_testing::TestEnvironment::Initialize,
                                   std::move(start_args->incoming_directory_server));
    EXPECT_EQ(ZX_OK, init_result.status_value());

    // TODO(fxbug.dev/134883): Connect Fake PCI Backend.

    // TODO(fxbug.dev/134883): Connect Fake Sysmem.

    // Start driver
    zx::result start_result = runtime_.RunToCompletion(
        driver_.SyncCall(&fdf_testing::DriverUnderTest<virtio::GpuDriver>::Start,
                         std::move(start_args->start_args)));

    // TODO(fxbug.dev/134883): This should be ZX_OK once all the mocks are in place.
    EXPECT_EQ(ZX_ERR_PEER_CLOSED, start_result.status_value());
  }

  void TearDown() override {
    zx::result stop_result = runtime_.RunToCompletion(
        driver_.SyncCall(&fdf_testing::DriverUnderTest<virtio::GpuDriver>::PrepareStop));
    EXPECT_EQ(ZX_OK, stop_result.status_value());
  }

  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }

 private:
  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  // Env dispatcher runs in the background because we need to make sync calls into it.
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};
  async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment_{
      env_dispatcher(), std::in_place};

  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<virtio::GpuDriver>> driver_{
      driver_dispatcher_->async_dispatcher(), std::in_place};
};

TEST_F(VirtioGpuTest, CreateAndStartDriver) {}

}  // namespace
