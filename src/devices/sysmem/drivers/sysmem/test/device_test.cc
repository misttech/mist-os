// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem/cpp/natural_types.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/sync/completion.h>
#include <lib/zx/bti.h>
#include <stdlib.h>
#include <zircon/errors.h>

#include <ddktl/device.h>
#include <ddktl/unbind-txn.h>
#include <zxtest/zxtest.h>

#include "../buffer_collection.h"
#include "../device.h"
#include "../driver.h"
#include "../logical_buffer_collection.h"
#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace sysmem_driver {
namespace {

class FakeDdkSysmem : public zxtest::Test {
 public:
  FakeDdkSysmem() {}

  void SetUp() override {
    pdev_.SetConfig({
        .use_fake_bti = true,
    });
    EXPECT_OK(pdev_loop_.StartThread());

    auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

    RunSyncOnLoop(pdev_loop_, [this, server = std::move(endpoints.server)]() mutable {
      outgoing_.emplace(pdev_loop_.dispatcher());
      auto device_handler =
          [this](fidl::ServerEnd<fuchsia_hardware_platform_device::Device> request) {
            fidl::BindServer(pdev_loop_.dispatcher(), std::move(request), &pdev_);
          };
      fuchsia_hardware_platform_device::Service::InstanceHandler handler(
          {.device = std::move(device_handler)});
      auto service_result =
          outgoing_->AddService<fuchsia_hardware_platform_device::Service>(std::move(handler));
      ZX_ASSERT(service_result.is_ok());

      ZX_ASSERT(outgoing_->Serve(std::move(server)).is_ok());
    });

    fake_parent_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                                 std::move(endpoints.client));
    EXPECT_EQ(sysmem_->Bind(), ZX_OK);
  }

  void TearDown() override {
    ddk::UnbindTxn txn{sysmem_->zxdev()};
    sysmem_->DdkUnbind(std::move(txn));
    EXPECT_OK(sysmem_->zxdev()->WaitUntilUnbindReplyCalled());
    std::ignore = sysmem_.release();
    loop_.Shutdown();
    RunSyncOnLoop(pdev_loop_, [this] { outgoing_.reset(); });
  }

  fidl::ClientEnd<fuchsia_sysmem::Allocator> Connect() {
    auto [allocator_client_end, allocator_server_end] =
        fidl::Endpoints<fuchsia_sysmem::Allocator>::Create();

    auto [connector_client_end, connector_server_end] =
        fidl::Endpoints<fuchsia_hardware_sysmem::DriverConnector>::Create();

    fidl::BindServer(loop_.dispatcher(), std::move(connector_server_end), sysmem_.get());
    EXPECT_OK(loop_.StartThread());

    auto result = fidl::WireCall(connector_client_end)->ConnectV1(std::move(allocator_server_end));
    EXPECT_OK(result);

    return std::move(allocator_client_end);
  }

  fidl::ClientEnd<fuchsia_sysmem::BufferCollection> AllocateNonSharedCollection() {
    fidl::WireSyncClient<fuchsia_sysmem::Allocator> allocator(Connect());

    auto [collection_client_end, collection_server_end] =
        fidl::Endpoints<fuchsia_sysmem::BufferCollection>::Create();

    EXPECT_OK(allocator->AllocateNonSharedCollection(std::move(collection_server_end)));
    return std::move(collection_client_end);
  }

  void RunSyncOnLoop(async::Loop& loop, fit::closure to_run) {
    sync_completion_t done;
    ZX_ASSERT(ZX_OK ==
              async::PostTask(loop.dispatcher(), [&done, to_run = std::move(to_run)]() mutable {
                std::move(to_run)();
                sync_completion_signal(&done);
              }));
    ZX_ASSERT(ZX_OK == sync_completion_wait_deadline(&done, ZX_TIME_INFINITE));
  }

 protected:
  sysmem_driver::Driver sysmem_ctx_;
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  std::unique_ptr<sysmem_driver::Device> sysmem_{new Device{fake_parent_.get(), &sysmem_ctx_}};

  fake_pdev::FakePDevFidl pdev_;

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  // Separate loop so we can make sync FIDL calls from loop_ to pdev_loop_.
  async::Loop pdev_loop_{&kAsyncLoopConfigNeverAttachToThread};
  // std::optional<> because outgoing_ can only be created, used, deleted on pdev_loop_
  std::optional<component::OutgoingDirectory> outgoing_;
};

TEST_F(FakeDdkSysmem, TearDownLoop) {
  // Queue up something that would be processed on the FIDL thread, so we can try to detect a
  // use-after-free if the FidlServer outlives the sysmem device.
  AllocateNonSharedCollection();
}

// Test that creating and tearing down a SecureMem connection works correctly.
TEST_F(FakeDdkSysmem, DummySecureMem) {
  auto [client, server] = fidl::Endpoints<fuchsia_sysmem::SecureMem>::Create();
  ASSERT_OK(sysmem_->CommonSysmemRegisterSecureMem(std::move(client)));

  // This shouldn't deadlock waiting for a message on the channel.
  EXPECT_OK(sysmem_->CommonSysmemUnregisterSecureMem());

  // This shouldn't cause a panic due to receiving peer closed.
  client.reset();
}

TEST_F(FakeDdkSysmem, NamedToken) {
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> allocator(Connect());

  auto [token_client_end, token_server_end] =
      fidl::Endpoints<fuchsia_sysmem::BufferCollectionToken>::Create();

  EXPECT_OK(allocator->AllocateSharedCollection(std::move(token_server_end)));

  fidl::WireSyncClient<fuchsia_sysmem::BufferCollectionToken> token(std::move(token_client_end));

  // The buffer collection should end up with a name of "a" because that's the highest priority.
  EXPECT_OK(token->SetName(5u, "c"));
  EXPECT_OK(token->SetName(100u, "a"));
  EXPECT_OK(token->SetName(6u, "b"));

  auto [collection_client_end, collection_server_end] =
      fidl::Endpoints<fuchsia_sysmem::BufferCollection>::Create();

  EXPECT_OK(
      allocator->BindSharedCollection(token.TakeClientEnd(), std::move(collection_server_end)));

  // Poll until a matching buffer collection is found.
  while (true) {
    bool found_collection = false;
    sync_completion_t completion;
    async::PostTask(sysmem_->dispatcher(), [&] {
      if (sysmem_->logical_buffer_collections().size() == 1) {
        const auto* logical_collection = *sysmem_->logical_buffer_collections().begin();
        auto collection_views = logical_collection->collection_views();
        if (collection_views.size() == 1) {
          auto name = logical_collection->name();
          EXPECT_TRUE(name);
          EXPECT_EQ("a", *name);
          found_collection = true;
        }
      }
      sync_completion_signal(&completion);
    });

    sync_completion_wait(&completion, ZX_TIME_INFINITE);
    if (found_collection)
      break;
  }
}

TEST_F(FakeDdkSysmem, NamedClient) {
  auto collection_client_end = AllocateNonSharedCollection();

  fidl::WireSyncClient<fuchsia_sysmem::BufferCollection> collection(
      std::move(collection_client_end));
  EXPECT_OK(collection->SetDebugClientInfo("a", 5));

  // Poll until a matching buffer collection is found.
  while (true) {
    bool found_collection = false;
    sync_completion_t completion;
    async::PostTask(sysmem_->dispatcher(), [&] {
      if (sysmem_->logical_buffer_collections().size() == 1) {
        const auto* logical_collection = *sysmem_->logical_buffer_collections().begin();
        if (logical_collection->collection_views().size() == 1) {
          const BufferCollection* collection = logical_collection->collection_views().front();
          if (collection->node_properties().client_debug_info().name == "a") {
            EXPECT_EQ(5u, collection->node_properties().client_debug_info().id);
            found_collection = true;
          }
        }
      }
      sync_completion_signal(&completion);
    });

    sync_completion_wait(&completion, ZX_TIME_INFINITE);
    if (found_collection)
      break;
  }
}

// Check that the allocator name overrides the collection name.
TEST_F(FakeDdkSysmem, NamedAllocatorToken) {
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> allocator(Connect());

  auto [token_client_end, token_server_end] =
      fidl::Endpoints<fuchsia_sysmem::BufferCollectionToken>::Create();

  EXPECT_OK(allocator->AllocateSharedCollection(std::move(token_server_end)));

  fidl::WireSyncClient<fuchsia_sysmem::BufferCollectionToken> token(std::move(token_client_end));

  const char kAlphabetString[] = "abcdefghijklmnopqrstuvwxyz";
  EXPECT_OK(token->SetDebugClientInfo("bad", 6));
  EXPECT_OK(allocator->SetDebugClientInfo(kAlphabetString, 5));

  auto [collection_client_end, collection_server_end] =
      fidl::Endpoints<fuchsia_sysmem::BufferCollection>::Create();

  EXPECT_OK(
      allocator->BindSharedCollection(token.TakeClientEnd(), std::move(collection_server_end)));

  // Poll until a matching buffer collection is found.
  while (true) {
    bool found_collection = false;
    sync_completion_t completion;
    async::PostTask(sysmem_->dispatcher(), [&] {
      if (sysmem_->logical_buffer_collections().size() == 1) {
        const auto* logical_collection = *sysmem_->logical_buffer_collections().begin();
        auto collection_views = logical_collection->collection_views();
        if (collection_views.size() == 1) {
          const auto& collection = collection_views.front();
          if (collection->node_properties().client_debug_info().name.find(kAlphabetString) !=
              std::string::npos) {
            EXPECT_EQ(5u, collection->node_properties().client_debug_info().id);
            found_collection = true;
          }
        }
      }
      sync_completion_signal(&completion);
    });

    sync_completion_wait(&completion, ZX_TIME_INFINITE);
    if (found_collection)
      break;
  }
}

TEST_F(FakeDdkSysmem, MaxSize) {
  sysmem_->set_settings(sysmem_driver::Settings{.max_allocation_size = zx_system_get_page_size()});

  auto collection_client = AllocateNonSharedCollection();

  fuchsia_sysmem::BufferCollectionConstraints constraints;
  constraints.min_buffer_count() = 1;
  constraints.has_buffer_memory_constraints() = true;
  constraints.buffer_memory_constraints().min_size_bytes() = zx_system_get_page_size() * 2;
  constraints.buffer_memory_constraints().cpu_domain_supported() = true;
  constraints.usage().cpu() = fuchsia_sysmem::kCpuUsageRead;

  fidl::WireSyncClient<fuchsia_sysmem::BufferCollection> collection(std::move(collection_client));
  fidl::Arena arena;
  EXPECT_OK(collection->SetConstraints(true, fidl::ToWire(arena, std::move(constraints))));

  // Sysmem should fail the collection and return an error.
  fidl::WireResult result = collection->WaitForBuffersAllocated();
  EXPECT_NE(result.status(), ZX_OK);
}

// Check that teardown doesn't leak any memory (detected through LSAN).
TEST_F(FakeDdkSysmem, TeardownLeak) {
  auto collection_client = AllocateNonSharedCollection();

  fuchsia_sysmem::BufferCollectionConstraints constraints;
  constraints.min_buffer_count() = 1;
  constraints.has_buffer_memory_constraints() = true;
  constraints.buffer_memory_constraints().min_size_bytes() = zx_system_get_page_size();
  constraints.buffer_memory_constraints().cpu_domain_supported() = true;
  constraints.usage().cpu() = fuchsia_sysmem::kCpuUsageRead;

  fidl::WireSyncClient<fuchsia_sysmem::BufferCollection> collection(std::move(collection_client));
  fidl::Arena arena;
  EXPECT_OK(collection->SetConstraints(true, fidl::ToWire(arena, std::move(constraints))));

  fidl::WireResult result = collection->WaitForBuffersAllocated();

  EXPECT_OK(result);
  EXPECT_OK(result.value().status);

  for (uint32_t i = 0; i < result.value().buffer_collection_info.buffer_count; i++) {
    result.value().buffer_collection_info.buffers[i].vmo.reset();
  }
  collection = {};
}

// Check that there are no circular references from a VMO to the logical buffer collection.
TEST_F(FakeDdkSysmem, BufferLeak) {
  auto collection_client = AllocateNonSharedCollection();

  fuchsia_sysmem::BufferCollectionConstraints constraints;
  constraints.min_buffer_count() = 1;
  constraints.has_buffer_memory_constraints() = true;
  constraints.buffer_memory_constraints().min_size_bytes() = zx_system_get_page_size();
  constraints.buffer_memory_constraints().cpu_domain_supported() = true;
  constraints.usage().cpu() = fuchsia_sysmem::kCpuUsageRead;

  fidl::WireSyncClient<fuchsia_sysmem::BufferCollection> collection(std::move(collection_client));
  fidl::Arena arena;
  EXPECT_OK(collection->SetConstraints(true, fidl::ToWire(arena, std::move(constraints))));

  fidl::WireResult result = collection->WaitForBuffersAllocated();

  EXPECT_OK(result);
  EXPECT_OK(result.value().status);

  for (uint32_t i = 0; i < result.value().buffer_collection_info.buffer_count; i++) {
    result.value().buffer_collection_info.buffers[i].vmo.reset();
  }

  collection = {};

  // Poll until all buffer collections are deleted.
  while (true) {
    bool no_collections = false;
    sync_completion_t completion;
    async::PostTask(sysmem_->dispatcher(), [&] {
      no_collections = sysmem_->logical_buffer_collections().empty();
      sync_completion_signal(&completion);
    });

    sync_completion_wait(&completion, ZX_TIME_INFINITE);
    if (no_collections)
      break;
  }
}

}  // namespace
}  // namespace sysmem_driver
