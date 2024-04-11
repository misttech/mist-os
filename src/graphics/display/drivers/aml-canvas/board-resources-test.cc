// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/aml-canvas/board-resources.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/async-loop/testing/cpp/real_loop.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/fake-bti/bti.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/bti.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <gtest/gtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/lib/testing/predicates/status.h"

namespace aml_canvas {

namespace {

class FakePdevTest : public ::testing::Test, public loop_fixture::RealLoop {
 public:
  FakePdevTest() = default;
  ~FakePdevTest() = default;

  void SetUp() override {
    logger_ =
        std::make_unique<fdf::Logger>("board-resources-pdev-test", FUCHSIA_LOG_INFO, zx::socket{},
                                      fidl::WireClient<fuchsia_logger::LogSink>{});
    fdf::Logger::SetGlobalInstance(logger_.get());
  }

  fidl::ClientEnd<fuchsia_hardware_platform_device::Device> ConnectToFakePdev() {
    auto [pdev_client, pdev_server] =
        fidl::Endpoints<fuchsia_hardware_platform_device::Device>::Create();
    fake_pdev_server_.Connect(std::move(pdev_server));
    return std::move(pdev_client);
  }

 protected:
  std::unique_ptr<fdf::Logger> logger_;
  fake_pdev::FakePDevFidl fake_pdev_server_;
};

TEST_F(FakePdevTest, MapMmioSuccess) {
  zx::vmo vmo;
  const uint64_t vmo_size = 0x2000;
  ASSERT_OK(zx::vmo::create(vmo_size, /*options=*/0, &vmo));

  fake_pdev::FakePDevFidl::Config config;
  config.mmios[0] = fake_pdev::MmioInfo{
      .vmo = std::move(vmo),
      .offset = 0,
      .size = vmo_size,
  };
  fake_pdev_server_.SetConfig(std::move(config));

  fidl::ClientEnd<fuchsia_hardware_platform_device::Device> pdev = ConnectToFakePdev();
  PerformBlockingWork([&] {
    zx::result<fdf::MmioBuffer> mmio_result = MapMmio(MmioResourceIndex::kDmc, pdev);
    ASSERT_OK(mmio_result.status_value());
  });
}

TEST_F(FakePdevTest, MapMmioPdevFailure) {
  fake_pdev::FakePDevFidl::Config config;
  fake_pdev_server_.SetConfig(std::move(config));

  fidl::ClientEnd<fuchsia_hardware_platform_device::Device> pdev = ConnectToFakePdev();
  PerformBlockingWork([&] {
    zx::result<fdf::MmioBuffer> mmio_result = MapMmio(MmioResourceIndex::kDmc, pdev);
    ASSERT_NE(mmio_result.status_value(), ZX_OK);
  });
}

TEST_F(FakePdevTest, GetBtiSuccess) {
  zx_handle_t fake_bti;
  zx_status_t status = fake_bti_create(&fake_bti);
  ASSERT_OK(status);

  fake_pdev::FakePDevFidl::Config config;
  config.btis[0] = zx::bti(fake_bti);
  fake_pdev_server_.SetConfig(std::move(config));

  fidl::ClientEnd<fuchsia_hardware_platform_device::Device> pdev = ConnectToFakePdev();
  PerformBlockingWork([&] {
    zx::result<zx::bti> bti_result = GetBti(BtiResourceIndex::kCanvas, pdev);
    ASSERT_OK(bti_result.status_value());
  });
}

TEST_F(FakePdevTest, GetBtiPdevFailure) {
  fake_pdev::FakePDevFidl::Config config;
  config.use_fake_bti = false;
  fake_pdev_server_.SetConfig(std::move(config));

  fidl::ClientEnd<fuchsia_hardware_platform_device::Device> pdev = ConnectToFakePdev();
  PerformBlockingWork([&] {
    zx::result<zx::bti> bti_result = GetBti(BtiResourceIndex::kCanvas, pdev);
    ASSERT_NE(bti_result.status_value(), ZX_OK);
  });
}

}  // namespace

}  // namespace aml_canvas
