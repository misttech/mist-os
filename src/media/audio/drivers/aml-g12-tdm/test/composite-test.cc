// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/aml-g12-tdm/composite.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdio/directory.h>
#include <lib/fzl/vmo-mapper.h>

#include <algorithm>

#include <zxtest/zxtest.h>

namespace audio::aml_g12 {

// TODO(fxbug.dev/132252): Complete audio-composite tests.
class FakePlatformDevice : public fidl::WireServer<fuchsia_hardware_platform_device::Device> {
 public:
  fuchsia_hardware_platform_device::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_platform_device::Service::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                          fidl::kIgnoreBindingClosure),
    });
  }

  void InitResources() { EXPECT_OK(zx::vmo::create(kMmioSize, 0, &mmio_)); }

  cpp20::span<uint32_t> mmio() {
    // The test has to wait for the driver to set the MMIO cache policy before mapping.
    if (!mapped_mmio_.start()) {
      MapMmio();
    }

    return {reinterpret_cast<uint32_t*>(mapped_mmio_.start()), kMmioSize / sizeof(uint32_t)};
  }

 private:
  static constexpr size_t kMmioSize = 0x1000;

  void GetMmio(fuchsia_hardware_platform_device::wire::DeviceGetMmioRequest* request,
               GetMmioCompleter::Sync& completer) override {
    if (request->index != 0) {
      return completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
    }

    zx::vmo vmo;
    if (zx_status_t status = mmio_.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo); status != ZX_OK) {
      return completer.ReplyError(status);
    }

    fidl::Arena arena;
    completer.ReplySuccess(fuchsia_hardware_platform_device::wire::Mmio::Builder(arena)
                               .offset(0)
                               .size(kMmioSize)
                               .vmo(std::move(vmo))
                               .Build());
  }

  void GetInterrupt(fuchsia_hardware_platform_device::wire::DeviceGetInterruptRequest* request,
                    GetInterruptCompleter::Sync& completer) override {}

  void GetBti(fuchsia_hardware_platform_device::wire::DeviceGetBtiRequest* request,
              GetBtiCompleter::Sync& completer) override {}

  void GetSmc(fuchsia_hardware_platform_device::wire::DeviceGetSmcRequest* request,
              GetSmcCompleter::Sync& completer) override {}

  void GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) override {}

  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override {}

  void MapMmio() { ASSERT_OK(mapped_mmio_.Map(mmio_)); }

  zx::vmo mmio_;
  fzl::VmoMapper mapped_mmio_;

  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> bindings_;
};

struct IncomingNamespace {
  fdf_testing::TestNode node_{std::string("root")};
  fdf_testing::TestEnvironment env_{fdf::Dispatcher::GetCurrent()->get()};
};

class AmlG12CompositeTest : public zxtest::Test {
 public:
  AmlG12CompositeTest()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        driver_dispatcher_(runtime_.StartBackgroundDispatcher()),
        incoming_(env_dispatcher(), std::in_place) {}

  void SetUp() override {
    fuchsia_driver_framework::DriverStartArgs driver_start_args;
    incoming_.SyncCall([&driver_start_args, this](IncomingNamespace* incoming) {
      auto start_args_result = incoming->node_.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());

      auto init_result =
          incoming->env_.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());

      platform_device_.InitResources();
      const zx::result add_platform_result =
          incoming->env_.incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
              platform_device_.GetInstanceHandler());
      EXPECT_TRUE(add_platform_result.is_ok());

      driver_start_args = std::move(start_args_result->start_args);
    });

    zx::result result = runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::DriverUnderTest<Driver>::Start, std::move(driver_start_args)));
    ASSERT_EQ(ZX_OK, result.status_value());

    incoming_.SyncCall([this](IncomingNamespace* incoming) {
      auto client_channel = incoming->node_.children().at(kDriverName).ConnectToDevice();
      client_.Bind(
          fidl::ClientEnd<fuchsia_hardware_audio::Composite>(std::move(client_channel.value())));
      ASSERT_TRUE(client_.is_valid());
    });
  }

  void TearDown() override {
    zx::result result =
        runtime_.RunToCompletion(dut_.SyncCall(&fdf_testing::DriverUnderTest<Driver>::PrepareStop));
    ASSERT_EQ(ZX_OK, result.status_value());
  }

 protected:
  fidl::SyncClient<fuchsia_hardware_audio::Composite> client_;
  FakePlatformDevice platform_device_;

 private:
  async_dispatcher_t* driver_dispatcher() { return driver_dispatcher_->async_dispatcher(); }
  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }

  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_;
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
  // Use dut_ instead of driver_ because driver_ is used by zxtest.
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<Driver>> dut_{
      driver_dispatcher(), std::in_place};
};

TEST_F(AmlG12CompositeTest, CompositeProperties) {
  fidl::Result result = client_->GetProperties();
  ASSERT_TRUE(result.is_ok());
  ASSERT_EQ(fuchsia_hardware_audio::kClockDomainMonotonic,
            result->properties().clock_domain().value());
}

TEST_F(AmlG12CompositeTest, Reset) {
  // TODO(fxbug.dev/132252): Add behavior to change state recovered to the configuration below.
  fidl::Result reset_result = client_->Reset();
  ASSERT_TRUE(reset_result.is_ok());

  // After reset we check we have configured all engines for TDM output and input.

  // Configure TDM OUT for I2S (default).
  // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 32 bits per slot is 0x3001002F.
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x500 / 4]);  // A output.
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x540 / 4]);  // B output.
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x580 / 4]);  // C output.

  // Configure TDM IN for I2S (default).
  // TDM IN CTRL0 config, PAD_TDMIN A(0), B(1) and C(2).
  ASSERT_EQ(0x7003'001F, platform_device_.mmio()[0x300 / 4]);  // A input.
  ASSERT_EQ(0x7013'001F, platform_device_.mmio()[0x340 / 4]);  // B input.
  ASSERT_EQ(0x7023'001F, platform_device_.mmio()[0x380 / 4]);  // C input.

  // Configure clocks.
  // SCLK CTRL, clk in/out enabled, 24 sdiv, 32 lrduty, 64 lrdiv.
  ASSERT_EQ(0xc180'7c3f, platform_device_.mmio()[0x040 / 4]);  // A.
  ASSERT_EQ(0xc180'7c3f, platform_device_.mmio()[0x048 / 4]);  // B.
  ASSERT_EQ(0xc180'7c3f, platform_device_.mmio()[0x050 / 4]);  // C.
}

}  // namespace audio::aml_g12
