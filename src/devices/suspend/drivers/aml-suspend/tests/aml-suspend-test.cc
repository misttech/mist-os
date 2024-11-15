// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../aml-suspend.h"

#include <fidl/fuchsia.hardware.suspend/cpp/markers.h>
#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <gtest/gtest.h>
#include <src/devices/bus/testing/fake-pdev/fake-pdev.h>

#include "lib/zx/handle.h"
#include "lib/zx/resource.h"
#include "lib/zx/vmo.h"

namespace suspend {

class AmlSuspendTest : public AmlSuspend {
 public:
  AmlSuspendTest(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : AmlSuspend(std::move(start_args), std::move(dispatcher)) {
    zx_status_t result = zx::vmo::create(1, 0, &fake_resource_);
    ZX_ASSERT(result == ZX_OK);
  }

  zx::result<zx::resource> GetCpuResource() override {
    zx::vmo dupe;
    zx_status_t st = fake_resource_.duplicate(ZX_RIGHT_SAME_RIGHTS, &dupe);
    if (st != ZX_OK) {
      return zx::error(st);
    }

    // Client is now the owner.
    zx::handle result(dupe.release());

    return zx::ok(std::move(result));
  }

  zx_status_t SystemSuspendEnter() override {
    // No-op override for testing.
    return ZX_OK;
  }

  static DriverRegistration GetDriverRegistration() {
    // Use a custom DriverRegistration to create the DUT. Without this, the non-test implementation
    // will be used by default.
    return FUCHSIA_DRIVER_REGISTRATION_V1(fdf_internal::DriverServer<AmlSuspendTest>::initialize,
                                          fdf_internal::DriverServer<AmlSuspendTest>::destroy);
  }

 private:
  // We just need any kernel handle here.
  zx::vmo fake_resource_;
};

class TestEnvironmentWrapper : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    pdev_.SetConfig(fake_pdev::FakePDevFidl::Config{});

    auto pdev_result = to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
        pdev_.GetInstanceHandler(fdf::Dispatcher::GetCurrent()->async_dispatcher()));
    if (pdev_result.is_error()) {
      return pdev_result.take_error();
    }

    compat_server_.Initialize("default");
    zx_status_t status =
        compat_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs);
    return zx::make_result(status);
  }

 private:
  fake_pdev::FakePDevFidl pdev_;
  compat::DeviceServer compat_server_;
};

class AmlSuspendTestConfiguration {
 public:
  using DriverType = AmlSuspendTest;
  using EnvironmentType = TestEnvironmentWrapper;
};

class AmlSuspendTestFixture : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result<> result = driver_test().StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
    zx::result connect_result =
        driver_test().Connect<fuchsia_hardware_suspend::SuspendService::Suspender>();
    EXPECT_EQ(ZX_OK, connect_result.status_value());
    client_.Bind(std::move(connect_result.value()));
  }
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  fidl::WireSyncClient<fuchsia_hardware_suspend::Suspender>& client() { return client_; }

  fdf_testing::BackgroundDriverTest<AmlSuspendTestConfiguration>& driver_test() {
    return driver_test_;
  }

 private:
  fdf_testing::BackgroundDriverTest<AmlSuspendTestConfiguration> driver_test_;
  fidl::WireSyncClient<fuchsia_hardware_suspend::Suspender> client_;
};

TEST_F(AmlSuspendTestFixture, TrivialGetSuspendStates) {
  auto result = client()->GetSuspendStates();

  ASSERT_TRUE(result.ok());

  // The protocol mandates that at least one suspend state is returned.
  ASSERT_TRUE(result.value()->has_suspend_states());
  EXPECT_GT(result.value()->suspend_states().count(), 0ul);
}

TEST_F(AmlSuspendTestFixture, TrivialSuspend) {
  fidl::Arena arena;
  auto request = fuchsia_hardware_suspend::wire::SuspenderSuspendRequest::Builder(arena)
                     .state_index(0)
                     .Build();
  auto result = client()->Suspend(request);
  EXPECT_TRUE(result.ok());
  EXPECT_TRUE(result->is_ok());
}

}  // namespace suspend
