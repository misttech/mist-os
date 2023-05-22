// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/runtime/testing/runtime/dispatcher.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime_env.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/zx/result.h>

#include <gtest/gtest.h>

#include "src/graphics/lib/magma/tests/mock/mock_msd_cc.h"
#include "sys_driver_cpp/magma_driver_base.h"

namespace {
class FakeTestDriver : public MagmaTestDriverBase {
 public:
  FakeTestDriver(fdf::DriverStartArgs start_args,
                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : MagmaTestDriverBase("fake_test_driver", std::move(start_args),
                            std::move(driver_dispatcher)) {}
  zx::result<> MagmaStart() override {
    std::lock_guard lock(magma_mutex());

    set_magma_driver(MagmaDriver::Create());
    if (!magma_driver()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    set_magma_system_device(magma_driver()->CreateDevice(nullptr));
    if (!magma_system_device()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    return zx::ok();
  }
};

class FakeDriver : public MagmaProductionDriverBase {
 public:
  FakeDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : MagmaProductionDriverBase("fake_driver", std::move(start_args),
                                  std::move(driver_dispatcher)) {}
  zx::result<> MagmaStart() override {
    std::lock_guard lock(magma_mutex());

    set_magma_driver(MagmaDriver::Create());
    if (!magma_driver()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    set_magma_system_device(magma_driver()->CreateDevice(nullptr));
    if (!magma_system_device()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    return zx::ok();
  }
};

// Check that the test driver class can be instantiated (not started).
TEST(MagmaDriver, CreateTestDriver) {
  fdf::TestSynchronizedDispatcher driver_dispatcher{fdf::kDispatcherDefault};
  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server{
      driver_dispatcher.dispatcher(), std::in_place, std::string("root")};

  zx::result start_args = node_server.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
  EXPECT_EQ(ZX_OK, start_args.status_value());

  EXPECT_EQ(ZX_OK, fdf::RunOnDispatcherSync(driver_dispatcher.dispatcher(), [&]() {
                     FakeDriver driver{std::move(start_args->start_args),
                                       driver_dispatcher.driver_dispatcher().borrow()};
                   }).status_value());
}

// Check that the driver class can be instantiated (not started).
TEST(MagmaDriver, CreateDriver) {
  fdf::TestSynchronizedDispatcher driver_dispatcher{fdf::kDispatcherDefault};
  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server{
      driver_dispatcher.dispatcher(), std::in_place, std::string("root")};

  zx::result start_args = node_server.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
  EXPECT_EQ(ZX_OK, start_args.status_value());
  EXPECT_EQ(ZX_OK, fdf::RunOnDispatcherSync(driver_dispatcher.dispatcher(), [&]() {
                     FakeDriver driver{std::move(start_args->start_args),
                                       driver_dispatcher.driver_dispatcher().borrow()};
                   }).status_value());
}

class MagmaDriverStarted : public testing::Test {
 public:
  void SetUp() override {
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    EXPECT_EQ(ZX_OK, start_args.status_value());

    ASSERT_TRUE(start_args.is_ok());

    EXPECT_EQ(ZX_OK, fdf::RunOnDispatcherSync(test_env_dispatcher_.dispatcher(), [&]() {
                       test_environment_.emplace();
                       EXPECT_EQ(ZX_OK,
                                 test_environment_
                                     ->Initialize(std::move(start_args->incoming_directory_server))
                                     .status_value());
                     }).status_value());

    auto driver = fdf_testing::StartDriver<FakeTestDriver>(std::move(start_args->start_args),
                                                           driver_dispatcher_, lifecycle_);

    ASSERT_TRUE(driver.is_ok());

    driver_ = std::move(*driver);
  }

  void TearDown() override {
    EXPECT_EQ(ZX_OK,
              fdf_testing::TeardownDriver(driver_, driver_dispatcher_, lifecycle_).status_value());
    EXPECT_EQ(ZX_OK, fdf::RunOnDispatcherSync(test_env_dispatcher_.dispatcher(), [&]() {
                       test_environment_.reset();
                     }).status_value());
  }

  async_patterns::TestDispatcherBound<fdf_testing::TestNode>& node_server() { return node_server_; }

  zx::result<zx::channel> ConnectToChild(const char* child_name) {
    return node_server().SyncCall([&child_name](fdf_testing::TestNode* root_node) {
      return root_node->children().at(child_name).ConnectToDevice();
    });
  }

 protected:
  using FakeLifecycle = fdf::Lifecycle<FakeTestDriver>;

  fdf::TestSynchronizedDispatcher driver_dispatcher_{fdf::kDispatcherNoDefaultAllowSync};

  // This dispatcher is used by the test environment, and hosts the incoming
  // directory.
  fdf::TestSynchronizedDispatcher test_env_dispatcher_{{
      .is_default_dispatcher = true,
      .options = {},
      .dispatcher_name = "test-env-dispatcher",
  }};
  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      test_env_dispatcher_.dispatcher(), std::in_place, std::string("root")};
  DriverLifecycle lifecycle_{.version = DRIVER_LIFECYCLE_VERSION_3,
                             .v1 = {.start = nullptr, .stop = FakeLifecycle::Stop},
                             .v2 = {FakeLifecycle::PrepareStop},
                             .v3 = {FakeLifecycle::Start}};
  std::optional<fdf_testing::TestEnvironment> test_environment_;

  FakeTestDriver* driver_{};
};

TEST_F(MagmaDriverStarted, TestDriver) {}

TEST_F(MagmaDriverStarted, Query) {
  zx::result device_result = ConnectToChild("magma_gpu");

  ASSERT_EQ(ZX_OK, device_result.status_value());
  fidl::ClientEnd<fuchsia_gpu_magma::Device> device_client_end(std::move(device_result.value()));
  fidl::WireSyncClient client(std::move(device_client_end));
  auto result = client->Query(fuchsia_gpu_magma::wire::QueryId::kDeviceId);
  ASSERT_EQ(ZX_OK, result.status());
  ASSERT_TRUE(result->is_ok()) << result->error_value();
  ASSERT_TRUE(result->value()->is_simple_result());
  EXPECT_EQ(0u, result->value()->simple_result());
}

TEST_F(MagmaDriverStarted, PerformanceCounters) {
  zx::result device_result = ConnectToChild("gpu-performance-counters");

  ASSERT_EQ(ZX_OK, device_result.status_value());
  fidl::ClientEnd<fuchsia_gpu_magma::PerformanceCounterAccess> device_client_end(
      std::move(device_result.value()));
  fidl::WireSyncClient client(std::move(device_client_end));
  auto result = client->GetPerformanceCountToken();

  ASSERT_EQ(ZX_OK, result.status());

  zx_info_handle_basic_t handle_info{};
  ASSERT_EQ(result->access_token.get_info(ZX_INFO_HANDLE_BASIC, &handle_info, sizeof(handle_info),
                                          nullptr, nullptr),
            ZX_OK);
  EXPECT_EQ(ZX_OBJ_TYPE_EVENT, handle_info.type);
}

class MemoryPressureProviderServer : public fidl::WireServer<fuchsia_memorypressure::Provider> {
 public:
  void RegisterWatcher(fuchsia_memorypressure::wire::ProviderRegisterWatcherRequest* request,
                       RegisterWatcherCompleter::Sync& completer) override {
    auto client = fidl::WireSyncClient(std::move(request->watcher));
    EXPECT_EQ(ZX_OK,
              client->OnLevelChanged(fuchsia_memorypressure::wire::Level::kWarning).status());
  }
};

TEST_F(MagmaDriverStarted, DependencyInjection) {
  zx::result device_result = ConnectToChild("gpu-dependency-injection");

  ASSERT_EQ(ZX_OK, device_result.status_value());
  fidl::ClientEnd<fuchsia_gpu_magma::DependencyInjection> device_client_end(
      std::move(device_result.value()));
  fidl::WireSyncClient client(std::move(device_client_end));

  auto memory_pressure_endpoints = fidl::CreateEndpoints<fuchsia_memorypressure::Provider>();
  ASSERT_EQ(ZX_OK, memory_pressure_endpoints.status_value());

  auto result = client->SetMemoryPressureProvider(std::move(memory_pressure_endpoints->client));
  ASSERT_EQ(ZX_OK, result.status());

  EXPECT_EQ(ZX_OK, fdf::RunOnDispatcherSync(test_env_dispatcher_.dispatcher(), [&]() {
                     auto server = std::make_unique<MemoryPressureProviderServer>();
                     fidl::BindServer(test_env_dispatcher_.dispatcher(),
                                      std::move(memory_pressure_endpoints->server),
                                      std::move(server));
                   }).status_value());

  MsdMockDevice* mock_device;
  {
    std::lock_guard magma_lock(driver_->magma_mutex());
    mock_device = static_cast<MsdMockDevice*>(driver_->magma_system_device()->msd_dev());
  }
  mock_device->WaitForMemoryPressureSignal();
  EXPECT_EQ(MAGMA_MEMORY_PRESSURE_LEVEL_WARNING, mock_device->memory_pressure_level());
}

}  // namespace
