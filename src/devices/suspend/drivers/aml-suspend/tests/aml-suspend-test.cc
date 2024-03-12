// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../aml-suspend.h"

#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <gtest/gtest.h>
#include <src/devices/bus/testing/fake-pdev/fake-pdev.h>

#include "fidl/fuchsia.hardware.suspend/cpp/markers.h"
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

class TestEnvironmentWrapper {
 public:
  fdf::DriverStartArgs Setup(fake_pdev::FakePDevFidl::Config pdev_config) {
    zx::result start_args_result = node_.CreateStartArgsAndServe();
    EXPECT_EQ(start_args_result.status_value(), ZX_OK);

    EXPECT_EQ(
        env_.Initialize(std::move(start_args_result->incoming_directory_server)).status_value(),
        ZX_OK);

    pdev_.SetConfig(std::move(pdev_config));

    async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();

    auto pdev_result =
        env_.incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
            pdev_.GetInstanceHandler(dispatcher));
    EXPECT_EQ(pdev_result.status_value(), ZX_OK);

    compat_server_.Init("default", "topo");
    zx_status_t status = compat_server_.Serve(dispatcher, &env_.incoming_directory());
    ZX_ASSERT(status == ZX_OK);

    return std::move(start_args_result->start_args);
  }

  fdf_testing::TestNode& Node() { return node_; }

 private:
  fdf_testing::TestNode node_{"root"};
  fdf_testing::TestEnvironment env_;
  fake_pdev::FakePDevFidl pdev_;
  compat::DeviceServer compat_server_;
};

class AmlSuspendTestFixture : public testing::Test {
 public:
  AmlSuspendTestFixture()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        driver_dispatcher_(runtime_.StartBackgroundDispatcher()),
        dut_(driver_dispatcher_->async_dispatcher(), std::in_place,
             AmlSuspendTest::GetDriverRegistration()),
        test_environment_(env_dispatcher_->async_dispatcher(), std::in_place) {}

 protected:
  void SetUp() override {
    fake_pdev::FakePDevFidl::Config config;
    fdf::DriverStartArgs start_args =
        test_environment_.SyncCall(&TestEnvironmentWrapper::Setup, std::move(config));
    zx::result start_result = runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::DriverUnderTest<AmlSuspendTest>::Start, std::move(start_args)));
    ASSERT_EQ(start_result.status_value(), ZX_OK);

    test_environment_.SyncCall([this](TestEnvironmentWrapper* env_wrapper) {
      auto client_channel =
          env_wrapper->Node().children().at("aml-suspend-device").ConnectToDevice();
      client_.Bind(
          fidl::ClientEnd<fuchsia_hardware_suspend::Suspender>(std::move(client_channel.value())));
      ASSERT_TRUE(client_.is_valid());
    });
  }

  void TearDown() override {
    zx::result run_result = runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::DriverUnderTest<AmlSuspendTest>::PrepareStop));
    ZX_ASSERT(run_result.is_ok());
  }

  fidl::WireSyncClient<fuchsia_hardware_suspend::Suspender> client_;

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_;
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<AmlSuspendTest>> dut_;
  async_patterns::TestDispatcherBound<TestEnvironmentWrapper> test_environment_;
};

TEST_F(AmlSuspendTestFixture, TrivialGetSuspendStates) {
  auto result = client_->GetSuspendStates();

  ASSERT_TRUE(result.ok());

  // The protocol mandates that at least one suspend state is returned.
  ASSERT_TRUE(result.value()->has_suspend_states());
  EXPECT_GT(result.value()->suspend_states().count(), 0ul);
}

}  // namespace suspend
