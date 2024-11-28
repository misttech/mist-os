// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/aml-canvas/aml-canvas-driver.h"

#include <fidl/fuchsia.hardware.amlogiccanvas/cpp/wire.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/compat/cpp/logging.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/driver_lifecycle.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <gtest/gtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/lib/testing/predicates/status.h"

namespace aml_canvas {

namespace {

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
class AmlCanvasDriverTest : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result create_start_args_zx_result =
        node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    ASSERT_TRUE(create_start_args_zx_result.is_ok());

    auto [start_args, incoming_directory_server, outgoing_directory_client] =
        std::move(create_start_args_zx_result).value();
    start_args_ = std::move(start_args);
    driver_outgoing_ = std::move(outgoing_directory_client);

    zx::result init_result = test_environment_.SyncCall(
        &fdf_testing::internal::TestEnvironment::Initialize, std::move(incoming_directory_server));
    ASSERT_OK(init_result);

    zx::vmo mmio_vmo;
    static constexpr uint64_t kMmioVmoSize = 0x2000;
    ASSERT_OK(zx::vmo::create(kMmioVmoSize, 0, &mmio_vmo));

    fake_pdev::FakePDevFidl::Config config;
    config.use_fake_bti = true;
    config.mmios[0] = fake_pdev::MmioInfo{
        .vmo = std::move(mmio_vmo),
        .offset = 0,
        .size = kMmioVmoSize,
    };
    fake_pdev_.SyncCall(&fake_pdev::FakePDevFidl::SetConfig, std::move(config));

    auto instance_handler = fake_pdev_.SyncCall(&fake_pdev::FakePDevFidl::GetInstanceHandler,
                                                async_patterns::PassDispatcher);
    test_environment_.SyncCall([&](fdf_testing::internal::TestEnvironment* env) {
      const zx::result result =
          env->incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
              std::move(instance_handler));
      ASSERT_TRUE(result.is_ok());
    });
  }

  void TearDown() override {
    driver_.reset();
    test_environment_.reset();
    fake_pdev_.reset();
    node_server_.reset();
  }

  fidl::ClientEnd<fuchsia_io::Directory> CreateDriverSvcClient() {
    // Open the svc directory in the driver's outgoing, and store a client to it.
    auto svc_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

    zx_status_t status = fdio_open3_at(
        driver_outgoing_.handle()->get(), "/svc",
        static_cast<uint64_t>(fuchsia_io::kPermReadable | fuchsia_io::Flags::kProtocolDirectory),
        svc_endpoints.server.TakeChannel().release());
    EXPECT_OK(status);
    return std::move(svc_endpoints.client);
  }

  void StartDriver() {
    zx::result start_result = runtime_.RunToCompletion(driver_.SyncCall(
        &fdf_testing::internal::DriverUnderTest<AmlCanvasDriver>::Start, (std::move(start_args_))));
    ASSERT_OK(start_result);
  }

  void StopDriver() {
    zx::result stop_result = runtime_.RunToCompletion(
        driver_.SyncCall(&fdf_testing::internal::DriverUnderTest<AmlCanvasDriver>::PrepareStop));
    ASSERT_OK(stop_result);
  }

  fdf_testing::DriverRuntime& runtime() { return runtime_; }
  async_patterns::TestDispatcherBound<fdf_testing::internal::DriverUnderTest<AmlCanvasDriver>>&
  driver() {
    return driver_;
  }

  async_dispatcher_t* driver_async_dispatcher() { return driver_dispatcher_->async_dispatcher(); }
  async_dispatcher_t* env_async_dispatcher() { return env_dispatcher_->async_dispatcher(); }

 private:
  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  // Env and driver dispatchers run in the background because we need to make
  // sync calls into them.
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_async_dispatcher(), std::in_place, std::string("root")};
  async_patterns::TestDispatcherBound<fake_pdev::FakePDevFidl> fake_pdev_{env_async_dispatcher(),
                                                                          std::in_place};
  async_patterns::TestDispatcherBound<fdf_testing::internal::TestEnvironment> test_environment_{
      env_async_dispatcher(), std::in_place};

  async_patterns::TestDispatcherBound<fdf_testing::internal::DriverUnderTest<AmlCanvasDriver>>
      driver_{driver_async_dispatcher(), std::in_place};

  fuchsia_driver_framework::DriverStartArgs start_args_;
  fidl::ClientEnd<fuchsia_io::Directory> driver_outgoing_;
};

TEST_F(AmlCanvasDriverTest, Lifecycle) {
  StartDriver();
  StopDriver();
}

TEST_F(AmlCanvasDriverTest, ServesAmlogicCanvasDeviceProtocol) {
  StartDriver();

  zx::result canvas_client_end =
      component::ConnectAtMember<fuchsia_hardware_amlogiccanvas::Service::Device>(
          CreateDriverSvcClient());
  ASSERT_OK(canvas_client_end);

  StopDriver();
}

}  // namespace

}  // namespace aml_canvas
