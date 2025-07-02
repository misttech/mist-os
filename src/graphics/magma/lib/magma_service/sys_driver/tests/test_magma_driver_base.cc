// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/driver/testing/cpp/minimal_compat_environment.h>
#include <lib/magma_service/mock/mock_msd.h>
#include <lib/magma_service/sys_driver/magma_driver_base.h>
#include <lib/zx/result.h>

#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

namespace msd {

class FakeTestDriver : public MagmaDriverBase {
 public:
  FakeTestDriver(fdf::DriverStartArgs start_args,
                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : MagmaDriverBase("fake_test_driver", std::move(start_args), std::move(driver_dispatcher)) {}
  zx::result<> MagmaStart() override {
    std::lock_guard lock(magma_mutex());

    set_magma_driver(msd::Driver::MsdCreate());
    if (!magma_driver()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    test_server_.set_unit_test_status(ZX_OK);
    zx::result result = CreateTestService(test_server_);
    if (result.is_error()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    set_magma_system_device(
        MagmaSystemDevice::Create(magma_driver(), magma_driver()->MsdCreateDevice(nullptr)));
    if (!magma_system_device()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    return zx::ok();
  }

 private:
  msd::MagmaTestServer test_server_;
};

namespace {

class FakeDriver : public MagmaDriverBase {
 public:
  FakeDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : MagmaDriverBase("fake_driver", std::move(start_args), std::move(driver_dispatcher)) {}
  zx::result<> MagmaStart() override {
    std::lock_guard lock(magma_mutex());

    set_magma_driver(msd::Driver::MsdCreate());
    if (!magma_driver()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    set_magma_system_device(
        MagmaSystemDevice::Create(magma_driver(), magma_driver()->MsdCreateDevice(nullptr)));
    if (!magma_system_device()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    return zx::ok();
  }
};

// Check that the test driver class can be instantiated (not started).
TEST(MagmaDriver, CreateTestDriver) {
  fdf_testing::DriverRuntime runtime;
  fdf_testing::TestNode node_server("root");

  zx::result start_args = node_server.CreateStartArgsAndServe();
  EXPECT_EQ(ZX_OK, start_args.status_value());
  FakeTestDriver driver{std::move(start_args->start_args),
                        fdf::UnownedSynchronizedDispatcher(fdf::Dispatcher::GetCurrent()->get())};
}

// Check that the driver class can be instantiated (not started).
TEST(MagmaDriver, CreateDriver) {
  fdf_testing::DriverRuntime runtime;
  fdf_testing::TestNode node_server("root");

  zx::result start_args = node_server.CreateStartArgsAndServe();
  EXPECT_EQ(ZX_OK, start_args.status_value());
  FakeDriver driver{std::move(start_args->start_args),
                    fdf::UnownedSynchronizedDispatcher(fdf::Dispatcher::GetCurrent()->get())};
}

class FixtureConfig final {
 public:
  using DriverType = FakeTestDriver;
  using EnvironmentType = fdf_testing::MinimalCompatEnvironment;
};

class MagmaDriverStarted : public testing::Test {
 public:
  void SetUp() override { ASSERT_OK(driver_test_.StartDriver()); }

  void TearDown() override { ASSERT_OK(driver_test_.StopDriver()); }

 protected:
  fdf_testing::BackgroundDriverTest<FixtureConfig>& driver_test() { return driver_test_; }

 private:
  fdf_testing::BackgroundDriverTest<FixtureConfig> driver_test_;
};

TEST_F(MagmaDriverStarted, TestDriver) {}

TEST_F(MagmaDriverStarted, Query) {
  zx::result client_end = driver_test().ConnectThroughDevfs<fuchsia_gpu_magma::Device>("magma_gpu");
  ASSERT_OK(client_end);
  fidl::WireSyncClient<fuchsia_gpu_magma::Device> client{std::move(client_end.value())};

  auto result = client->Query(fuchsia_gpu_magma::wire::QueryId::kDeviceId);
  ASSERT_EQ(ZX_OK, result.status());
  ASSERT_TRUE(result->is_ok()) << result->error_value();
  ASSERT_TRUE(result->value()->is_simple_result());
  EXPECT_EQ(0u, result->value()->simple_result());
}

TEST_F(MagmaDriverStarted, PerformanceCounters) {
  zx::result client_end =
      driver_test().ConnectThroughDevfs<fuchsia_gpu_magma::PerformanceCounterAccess>(
          "gpu-performance-counters");
  ASSERT_OK(client_end);
  fidl::WireSyncClient<fuchsia_gpu_magma::PerformanceCounterAccess> client{
      std::move(client_end.value())};

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
  zx::result client_end = driver_test().ConnectThroughDevfs<fuchsia_gpu_magma::DependencyInjection>(
      "gpu-dependency-injection");
  ASSERT_OK(client_end);
  fidl::WireSyncClient<fuchsia_gpu_magma::DependencyInjection> client{
      std::move(client_end.value())};

  auto memory_pressure_endpoints = fidl::Endpoints<fuchsia_memorypressure::Provider>::Create();

  auto result = client->SetMemoryPressureProvider(std::move(memory_pressure_endpoints.client));
  ASSERT_EQ(ZX_OK, result.status());

  driver_test().RunInEnvironmentTypeContext([&](auto& env) {
    auto server = std::make_unique<MemoryPressureProviderServer>();
    auto* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    fidl::BindServer(dispatcher, std::move(memory_pressure_endpoints.server), std::move(server));
  });

  MsdMockDevice* mock_device;
  driver_test().RunInDriverContext([&mock_device](auto& driver) mutable {
    std::lock_guard magma_lock(driver.magma_mutex());
    mock_device = static_cast<MsdMockDevice*>(driver.magma_system_device()->msd_dev());
  });
  mock_device->WaitForMemoryPressureSignal();
  EXPECT_EQ(msd::MAGMA_MEMORY_PRESSURE_LEVEL_WARNING, mock_device->memory_pressure_level());
}

}  // namespace

}  // namespace msd

// Export the |FakeTestDriver| for the |fdf_testing::internal::DriverUnderTest<FakeTestDriver>| to
// use.
FUCHSIA_DRIVER_EXPORT(msd::FakeTestDriver);
