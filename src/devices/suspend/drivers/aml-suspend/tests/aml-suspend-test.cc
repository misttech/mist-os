// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../aml-suspend.h"

#include <fidl/fuchsia.hardware.suspend/cpp/markers.h>
#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>

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

    compat_server_.Init("default", "topo");
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
  static constexpr bool kDriverOnForeground = false;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = AmlSuspendTest;
  using EnvironmentType = TestEnvironmentWrapper;
};

class AmlSuspendTestFixture : public fdf_testing::DriverTestFixture<AmlSuspendTestConfiguration> {
 protected:
  void SetUp() override {
    zx::result connect_result = Connect<fuchsia_hardware_suspend::SuspendService::Suspender>();
    EXPECT_EQ(ZX_OK, connect_result.status_value());
    client_.Bind(std::move(connect_result.value()));
  }

  fidl::WireSyncClient<fuchsia_hardware_suspend::Suspender>& client() { return client_; }

 private:
  fidl::WireSyncClient<fuchsia_hardware_suspend::Suspender> client_;
};

TEST_F(AmlSuspendTestFixture, TrivialGetSuspendStates) {
  auto result = client()->GetSuspendStates();

  ASSERT_TRUE(result.ok());

  // The protocol mandates that at least one suspend state is returned.
  ASSERT_TRUE(result.value()->has_suspend_states());
  EXPECT_GT(result.value()->suspend_states().count(), 0ul);
}

}  // namespace suspend
