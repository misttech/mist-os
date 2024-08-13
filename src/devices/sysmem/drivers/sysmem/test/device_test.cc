// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/sysmem/drivers/sysmem/device.h"

#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem/cpp/natural_types.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/sync/completion.h>
#include <lib/zx/bti.h>
#include <stdlib.h>
#include <zircon/errors.h>

#include <ddktl/device.h>
#include <ddktl/unbind-txn.h>
#include <gtest/gtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/sysmem/drivers/sysmem/buffer_collection.h"
#include "src/devices/sysmem/drivers/sysmem/device.h"
#include "src/devices/sysmem/drivers/sysmem/logical_buffer_collection.h"
#include "src/devices/sysmem/drivers/sysmem/sysmem_config.h"

namespace sysmem_driver {
namespace {

class FakeDdkSysmem : public ::testing::Test {
 public:
  FakeDdkSysmem() {}

  void SetUp() override {
    zx::result<fdf_testing::TestNode::CreateStartArgsResult> create_start_args_result =
        node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    ASSERT_TRUE(create_start_args_result.is_ok());

    auto [start_args, incoming_directory_server, outgoing_directory_client] =
        std::move(create_start_args_result).value();
    start_args.config() = sysmem_config::Config{}.ToVmo();
    start_args_ = std::move(start_args);
    driver_outgoing_ = std::move(outgoing_directory_client);

    auto init_result = test_environment_.SyncCall(&fdf_testing::TestEnvironment::Initialize,
                                                  std::move(incoming_directory_server));
    ASSERT_TRUE(init_result.is_ok());

    fake_pdev::FakePDevFidl::Config config;
    config.use_fake_bti = true;
    fake_pdev_.SyncCall(&fake_pdev::FakePDevFidl::SetConfig, std::move(config));

    auto pdev_instance_handler = fake_pdev_.SyncCall(&fake_pdev::FakePDevFidl::GetInstanceHandler,
                                                     async_patterns::PassDispatcher);
    test_environment_.SyncCall([&pdev_instance_handler](fdf_testing::TestEnvironment* env) {
      auto add_service_result =
          env->incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
              std::move(pdev_instance_handler));
      ASSERT_TRUE(add_service_result.is_ok());
    });

    StartDriver();
  }

  void TearDown() override {
    StopDriver();

    device_ = nullptr;
    wrapped_device_.reset();
    test_environment_.reset();
    fake_pdev_.reset();
    node_server_.reset();
  }

  fidl::ClientEnd<fuchsia_io::Directory> CreateDriverOutgoingDirClient() {
    // Open the svc directory in the driver's outgoing, and return a client end.
    auto svc_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

    zx_status_t status = fdio_open_at(driver_outgoing_.handle()->get(), "/svc",
                                      static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                      svc_endpoints.server.TakeChannel().release());
    EXPECT_EQ(status, ZX_OK);
    return std::move(svc_endpoints.client);
  }

  void StartDriver() {
    zx::result start_result = runtime_.RunToCompletion(wrapped_device_.SyncCall(
        &fdf_testing::DriverUnderTest<sysmem_driver::Device>::Start, std::move(start_args_)));
    ASSERT_EQ(start_result.status_value(), ZX_OK);
    wrapped_device_.SyncCall([this](fdf_testing::DriverUnderTest<sysmem_driver::Device>* device) {
      device_ = **device;
    });
  }

  void StopDriver() {
    zx::result stop_result = runtime_.RunToCompletion(wrapped_device_.SyncCall(
        &fdf_testing::DriverUnderTest<sysmem_driver::Device>::PrepareStop));
    ASSERT_EQ(stop_result.status_value(), ZX_OK);
  }

  fidl::ClientEnd<fuchsia_sysmem2::Allocator> Connect() {
    zx::result allocator_v2_result =
        component::ConnectAtMember<fuchsia_hardware_sysmem::Service::AllocatorV2>(
            CreateDriverOutgoingDirClient());
    EXPECT_TRUE(allocator_v2_result.is_ok());
    return std::move(allocator_v2_result).value();
  }

  fidl::ClientEnd<fuchsia_sysmem2::BufferCollection> AllocateNonSharedCollection() {
    fidl::SyncClient<fuchsia_sysmem2::Allocator> allocator(Connect());

    auto [collection_client_end, collection_server_end] =
        fidl::Endpoints<fuchsia_sysmem2::BufferCollection>::Create();

    fuchsia_sysmem2::AllocatorAllocateNonSharedCollectionRequest allocate_non_shared_request;
    allocate_non_shared_request.collection_request() = std::move(collection_server_end);
    EXPECT_TRUE(
        allocator->AllocateNonSharedCollection(std::move(allocate_non_shared_request)).is_ok());
    return std::move(collection_client_end);
  }

  fdf_testing::DriverRuntime& runtime() { return runtime_; }
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<sysmem_driver::Device>>&
  wrapped_device() {
    return wrapped_device_;
  }

  async_dispatcher_t* driver_async_dispatcher() { return driver_dispatcher_->async_dispatcher(); }
  async_dispatcher_t* env_async_dispatcher() { return env_dispatcher_->async_dispatcher(); }

 protected:
  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  // Env dispatcher and driver dispatchers run separately in the background because we need to make
  // sync calls from driver dispatcher to env dispatcher, and from test thread to driver dispatcher
  // (in Connect).
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_async_dispatcher(), std::in_place, std::string("root")};
  async_patterns::TestDispatcherBound<fake_pdev::FakePDevFidl> fake_pdev_{env_async_dispatcher(),
                                                                          std::in_place};
  async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment_{
      env_async_dispatcher(), std::in_place};

  fuchsia_driver_framework::DriverStartArgs start_args_;
  fidl::ClientEnd<fuchsia_io::Directory> driver_outgoing_;

  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<sysmem_driver::Device>>
      wrapped_device_{driver_async_dispatcher(), std::in_place};

  sysmem_driver::Device* device_ = nullptr;
};

TEST_F(FakeDdkSysmem, Lifecycle) {
  // Queue up something that would be processed on the FIDL thread, so we can try to detect a
  // use-after-free if the FidlServer outlives the sysmem device.
  AllocateNonSharedCollection();
}

// Test that creating and tearing down a SecureMem connection works correctly.
TEST_F(FakeDdkSysmem, DummySecureMem) {
  auto [client, server] = fidl::Endpoints<fuchsia_sysmem::SecureMem>::Create();
  ASSERT_EQ(device_->RegisterSecureMemInternal(std::move(client)), ZX_OK);

  // This shouldn't deadlock waiting for a message on the channel.
  ASSERT_EQ(device_->UnregisterSecureMemInternal(), ZX_OK);

  // This shouldn't cause a panic due to receiving peer closed.
  client.reset();
}

TEST_F(FakeDdkSysmem, NamedToken) {
  fidl::SyncClient<fuchsia_sysmem2::Allocator> allocator(Connect());

  auto [token_client_end, token_server_end] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();

  fuchsia_sysmem2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_end);
  EXPECT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());

  fidl::SyncClient<fuchsia_sysmem2::BufferCollectionToken> token(std::move(token_client_end));

  // The buffer collection should end up with a name of "a" because that's the highest priority.
  {
    fuchsia_sysmem2::NodeSetNameRequest set_name_request;
    set_name_request.priority() = 5u;
    set_name_request.name() = "c";
    EXPECT_TRUE(token->SetName(std::move(set_name_request)).is_ok());
  }
  {
    fuchsia_sysmem2::NodeSetNameRequest set_name_request;
    set_name_request.priority() = 100u;
    set_name_request.name() = "a";
    EXPECT_TRUE(token->SetName(std::move(set_name_request)).is_ok());
  }
  {
    fuchsia_sysmem2::NodeSetNameRequest set_name_request;
    set_name_request.priority() = 6u;
    set_name_request.name() = "b";
    EXPECT_TRUE(token->SetName(std::move(set_name_request)).is_ok());
  }

  auto [collection_client_end, collection_server_end] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollection>::Create();

  fuchsia_sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_end);
  EXPECT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  // Poll until a matching buffer collection is found.
  while (true) {
    bool found_collection = device_->SyncCall([&]() {
      if (device_->logical_buffer_collections().size() == 1) {
        const auto* logical_collection = *device_->logical_buffer_collections().begin();
        auto collection_views = logical_collection->collection_views();
        if (collection_views.size() == 1) {
          auto name = logical_collection->name();
          EXPECT_TRUE(name);
          EXPECT_EQ("a", *name);
          return true;
        }
      }
      return false;
    });
    if (found_collection) {
      break;
    }
  }
}

TEST_F(FakeDdkSysmem, NamedClient) {
  auto collection_client_end = AllocateNonSharedCollection();

  fidl::SyncClient<fuchsia_sysmem2::BufferCollection> collection(std::move(collection_client_end));
  fuchsia_sysmem2::NodeSetDebugClientInfoRequest set_debug_request;
  set_debug_request.name() = "a";
  set_debug_request.id() = 5;
  EXPECT_TRUE(collection->SetDebugClientInfo(std::move(set_debug_request)).is_ok());

  // Poll until a matching buffer collection is found.
  while (true) {
    bool found_collection = device_->SyncCall([&]() mutable {
      if (device_->logical_buffer_collections().size() == 1) {
        const auto* logical_collection = *device_->logical_buffer_collections().begin();
        auto collection_views = logical_collection->collection_views();
        if (collection_views.size() == 1) {
          const BufferCollection* collection = logical_collection->collection_views().front();
          if (collection->node_properties().client_debug_info().name == "a") {
            EXPECT_EQ(5u, collection->node_properties().client_debug_info().id);
            return true;
          }
        }
      }
      return false;
    });
    if (found_collection) {
      break;
    }
  }
}

// Check that the allocator name overrides the collection name.
TEST_F(FakeDdkSysmem, NamedAllocatorToken) {
  fidl::SyncClient<fuchsia_sysmem2::Allocator> allocator(Connect());

  auto [token_client_end, token_server_end] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();

  fuchsia_sysmem2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_end);
  EXPECT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());

  fidl::SyncClient<fuchsia_sysmem2::BufferCollectionToken> token(std::move(token_client_end));

  const char kAlphabetString[] = "abcdefghijklmnopqrstuvwxyz";
  {
    fuchsia_sysmem2::AllocatorSetDebugClientInfoRequest set_debug_request;
    set_debug_request.name() = kAlphabetString;
    set_debug_request.id() = 5;
    EXPECT_TRUE(allocator->SetDebugClientInfo(std::move(set_debug_request)).is_ok());
  }
  // Despite this message being sent after the above message, this message is not the "final word"
  // on the debug info, because the allocator will fence all token messages before transferring the
  // allocator's debug info to the BufferColllection.
  {
    fuchsia_sysmem2::NodeSetDebugClientInfoRequest set_debug_request;
    set_debug_request.name() = "bad";
    set_debug_request.id() = 6;
    EXPECT_TRUE(token->SetDebugClientInfo(std::move(set_debug_request)).is_ok());
  }

  auto [collection_client_end, collection_server_end] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollection>::Create();

  fuchsia_sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_end);
  EXPECT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  // Poll until a matching buffer collection is found. If this gets stuck, sysmem may be failing to
  // ensure that the allocator's debug info is the "last word" - may be failing to fence the token
  // messages before applyign the allocator's debug info to the token.
  while (true) {
    bool found_collection = device_->SyncCall([&]() {
      ZX_ASSERT(fdf::Dispatcher::GetCurrent()->async_dispatcher() == device_->loop_dispatcher());
      if (device_->logical_buffer_collections().size() == 1) {
        const auto* logical_collection = *device_->logical_buffer_collections().begin();
        auto collection_views = logical_collection->collection_views();
        if (collection_views.size() == 1) {
          const auto& collection = collection_views.front();
          // This needs to tell the difference between "abcdefghijklmnopqrstuvwxyz" and
          // "bad (was abcdefghijklmnopqrstuvwxyz)".
          if (collection->node_properties().client_debug_info().name.find(kAlphabetString) == 0) {
            EXPECT_EQ(5u, collection->node_properties().client_debug_info().id);
            return true;
          }
        }
      }
      return false;
    });
    if (found_collection) {
      break;
    }
  }
}

TEST_F(FakeDdkSysmem, MaxSize) {
  device_->set_settings(sysmem_driver::Settings{.max_allocation_size = zx_system_get_page_size()});

  auto collection_client = AllocateNonSharedCollection();

  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.min_buffer_count() = 1;
  auto& bmc = constraints.buffer_memory_constraints().emplace();
  bmc.min_size_bytes() = zx_system_get_page_size() * 2;
  bmc.cpu_domain_supported() = true;
  constraints.usage().emplace().cpu() = fuchsia_sysmem::kCpuUsageRead;

  fidl::SyncClient<fuchsia_sysmem2::BufferCollection> collection(std::move(collection_client));
  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  EXPECT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  // Sysmem should fail the collection and return an error.
  auto wait_result = collection->WaitForAllBuffersAllocated();
  EXPECT_TRUE(!wait_result.is_ok());
}

// Check that teardown doesn't leak any memory (detected through LSAN).
TEST_F(FakeDdkSysmem, TeardownLeak) {
  auto collection_client = AllocateNonSharedCollection();

  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.min_buffer_count() = 1;
  auto& bmc = constraints.buffer_memory_constraints().emplace();
  bmc.min_size_bytes() = zx_system_get_page_size();
  bmc.cpu_domain_supported() = true;
  constraints.usage().emplace().cpu() = fuchsia_sysmem::kCpuUsageRead;

  fidl::SyncClient<fuchsia_sysmem2::BufferCollection> collection(std::move(collection_client));
  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  EXPECT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto wait_result = collection->WaitForAllBuffersAllocated();
  EXPECT_TRUE(wait_result.is_ok());
  auto info = std::move(wait_result->buffer_collection_info());

  for (uint32_t i = 0; i < info->buffers()->size(); i++) {
    info->buffers()->at(i).vmo().reset();
  }
  collection = {};
}

// Check that there are no circular references from a VMO to the logical buffer collection.
TEST_F(FakeDdkSysmem, BufferLeak) {
  auto collection_client = AllocateNonSharedCollection();

  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.min_buffer_count() = 1;
  auto& bmc = constraints.buffer_memory_constraints().emplace();
  bmc.min_size_bytes() = zx_system_get_page_size();
  bmc.cpu_domain_supported() = true;
  constraints.usage().emplace().cpu() = fuchsia_sysmem::kCpuUsageRead;

  fidl::SyncClient<fuchsia_sysmem2::BufferCollection> collection(std::move(collection_client));
  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  EXPECT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto wait_result = collection->WaitForAllBuffersAllocated();
  EXPECT_TRUE(wait_result.is_ok());
  auto info = std::move(wait_result->buffer_collection_info());

  for (uint32_t i = 0; i < info->buffers()->size(); i++) {
    info->buffers()->at(i).vmo().reset();
  }

  collection = {};

  // Poll until all buffer collections are deleted.
  while (true) {
    bool no_collections = device_->SyncCall([&]() {
      no_collections = device_->logical_buffer_collections().empty();
      return no_collections;
    });
    if (no_collections) {
      break;
    }
  }
}

}  // namespace
}  // namespace sysmem_driver
