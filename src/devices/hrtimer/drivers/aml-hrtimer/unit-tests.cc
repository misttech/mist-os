// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>
#include <lib/fake-bti/bti.h>
#include <lib/fzl/vmo-mapper.h>

#include "src/devices/hrtimer/drivers/aml-hrtimer/aml-hrtimer.h"

namespace hrtimer {

class FakePlatformDevice : public fidl::WireServer<fuchsia_hardware_platform_device::Device> {
 public:
  fuchsia_hardware_platform_device::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_platform_device::Service::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                          fidl::kIgnoreBindingClosure),
    });
  }

  void InitResources() {
    zx::vmo::create(kMmioSize, 0, &mmio_);
    fake_bti_create(bti_.reset_and_get_address());
  }

  cpp20::span<uint32_t> mmio() {
    // The test has to wait for the driver to set the MMIO cache policy before mapping.
    if (!mapped_mmio_.start()) {
      MapMmio();
    }

    return {reinterpret_cast<uint32_t*>(mapped_mmio_.start()), kMmioSize / sizeof(uint32_t)};
  }

  void TriggerAllIrqs() {
    for (size_t i = 0; i < AmlHrtimer::GetNumberOfIrqs(); ++i) {
      ASSERT_EQ(fake_interrupts_[i].trigger(0, zx::clock::get_monotonic()), ZX_OK);
    }
  }

 private:
  static constexpr size_t kMmioSize = 0x10000;

  void GetMmioById(fuchsia_hardware_platform_device::wire::DeviceGetMmioByIdRequest* request,
                   GetMmioByIdCompleter::Sync& completer) override {
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

  void GetMmioByName(fuchsia_hardware_platform_device::wire::DeviceGetMmioByNameRequest* request,
                     GetMmioByNameCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetInterruptById(
      fuchsia_hardware_platform_device::wire::DeviceGetInterruptByIdRequest* request,
      GetInterruptByIdCompleter::Sync& completer) override {
    if (request->index >= AmlHrtimer::GetNumberOfIrqs()) {
      completer.ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }
    zx::interrupt interrupt;
    ASSERT_EQ(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL,
                                    &fake_interrupts_[request->index]),
              ZX_OK);
    zx_status_t status =
        fake_interrupts_[request->index].duplicate(ZX_RIGHT_SAME_RIGHTS, &interrupt);
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    completer.ReplySuccess(std::move(interrupt));
  }

  void GetInterruptByName(
      fuchsia_hardware_platform_device::wire::DeviceGetInterruptByNameRequest* request,
      GetInterruptByNameCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetBtiById(fuchsia_hardware_platform_device::wire::DeviceGetBtiByIdRequest* request,
                  GetBtiByIdCompleter::Sync& completer) override {
    zx::bti bti;
    if (zx_status_t status = bti_.duplicate(ZX_RIGHT_SAME_RIGHTS, &bti); status != ZX_OK) {
      return completer.ReplyError(status);
    }
    completer.ReplySuccess(std::move(bti));
  }

  void GetBtiByName(fuchsia_hardware_platform_device::wire::DeviceGetBtiByNameRequest* request,
                    GetBtiByNameCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetSmcById(fuchsia_hardware_platform_device::wire::DeviceGetSmcByIdRequest* request,
                  GetSmcByIdCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetNodeDeviceInfo(GetNodeDeviceInfoCompleter::Sync& completer) override {
    fidl::Arena arena;
    auto info = fuchsia_hardware_platform_device::wire::NodeDeviceInfo::Builder(arena);
    completer.ReplySuccess(info.vid(PDEV_VID_AMLOGIC).pid(PDEV_PID_AMLOGIC_A311D).Build());
  }

  void GetSmcByName(fuchsia_hardware_platform_device::wire::DeviceGetSmcByNameRequest* request,
                    GetSmcByNameCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_platform_device::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  void MapMmio() { mapped_mmio_.Map(mmio_); }

  void GetPowerConfiguration(GetPowerConfigurationCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  zx::vmo mmio_;
  fzl::VmoMapper mapped_mmio_;
  zx::bti bti_;
  zx::interrupt fake_interrupts_[AmlHrtimer::GetNumberOfIrqs()];

  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> bindings_;
};

class TestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    platform_device_.InitResources();
    auto result = to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
        platform_device_.GetInstanceHandler());
    EXPECT_EQ(ZX_OK, result.status_value());
    return zx::ok();
  }
  FakePlatformDevice& platform_device() { return platform_device_; }

 private:
  FakePlatformDevice platform_device_;
};

class FixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = false;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = AmlHrtimer;
  using EnvironmentType = TestEnvironment;
};

class DriverTest : public fdf_testing::DriverTestFixture<FixtureConfig> {
 protected:
  void SetUp() override {
    zx::result device_result = ConnectThroughDevfs<fuchsia_hardware_hrtimer::Device>("aml-hrtimer");
    ASSERT_EQ(ZX_OK, device_result.status_value());
    client_.Bind(std::move(device_result.value()));
  }

  fidl::SyncClient<fuchsia_hardware_hrtimer::Device> client_;
};

TEST_F(DriverTest, Properties) {
  auto result = client_->GetProperties();
  ASSERT_FALSE(result.is_error());
  ASSERT_FALSE(result->properties().IsEmpty());
  ASSERT_TRUE(result->properties().timers_properties().has_value());
  ASSERT_EQ(result->properties().timers_properties()->size(), std::size_t{9});
  auto& timers = result->properties().timers_properties().value();

  // Timers id 0 to 8 inclusive, except timer id 4.
  for (uint64_t i = 0; i < 9; ++i) {
    ASSERT_TRUE(timers[i].id());
    if (timers[i].id().value() == 4) {
      continue;
    }
    ASSERT_EQ(timers[i].id().value(), static_cast<uint64_t>(i));
    ASSERT_EQ(timers[i].supported_resolutions()->size(), 4ULL);
    auto& resolutions = timers[i].supported_resolutions().value();
    ASSERT_EQ(resolutions[0].duration().value(), 1'000);
    ASSERT_EQ(resolutions[1].duration().value(), 10'000);
    ASSERT_EQ(resolutions[2].duration().value(), 100'000);
    ASSERT_EQ(resolutions[3].duration().value(), 1'000'000);
    ASSERT_EQ(timers[i].max_ticks().value(), 0xffffULL);
    ASSERT_TRUE(timers[i].supports_event().value());
  }

  /// Timer id 4 has no IRQ and higher max_range.
  ASSERT_EQ(timers[4].id().value(), static_cast<uint64_t>(4));
  ASSERT_EQ(timers[4].supported_resolutions()->size(), 3ULL);
  auto& resolutions = timers[4].supported_resolutions().value();
  ASSERT_EQ(resolutions[0].duration().value(), 1'000);
  ASSERT_EQ(resolutions[1].duration().value(), 10'000);
  ASSERT_EQ(resolutions[2].duration().value(), 100'000);
  ASSERT_EQ(timers[4].max_ticks().value(), 0xffff'ffff'ffff'ffffULL);
  ASSERT_FALSE(timers[4].supports_event().value());
}

TEST_F(DriverTest, StartTimerNoticks) {
  // Timers id 0 to 8 inclusive are able to take a 0 ticks expiration request for durations 1, 10,
  // and 100 usecs.
  for (uint64_t i = 0; i < 9; ++i) {
    auto result0 =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
    ASSERT_FALSE(result0.is_error());
    auto result1 =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(10'000ULL), 0});
    ASSERT_FALSE(result1.is_error());
    auto result2 =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(100'000ULL), 0});
    ASSERT_FALSE(result2.is_error());
  }

  /// Timer id 4 does not support 1 msec.
  auto result =
      client_->Start({4ULL, fuchsia_hardware_hrtimer::Resolution::WithDuration(1000'000ULL), 0});
  ASSERT_TRUE(result.is_error());
  ASSERT_EQ(result.error_value().domain_error(),
            fuchsia_hardware_hrtimer::DriverError::kInvalidArgs);

  // Timers id 0 to 8 inclusive but not 4 support 1 msec.
  for (uint64_t i = 0; i < 9; ++i) {
    if (i == 4) {
      continue;
    }
    auto result =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000'000ULL), 0});
    ASSERT_FALSE(result.is_error());
  }
}

TEST_F(DriverTest, StartTimerMaxTicks) {
  // Timers id 0 to 8 inclusive are able to take a up to 0xffff ticks expiration request for
  // durations 1, 10, and 100 usecs.
  for (uint64_t i = 0; i < 9; ++i) {
    auto result0 =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0xffff});
    ASSERT_FALSE(result0.is_error());
    auto result1 =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(10'000ULL), 0xffff});
    ASSERT_FALSE(result1.is_error());
    auto result2 =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(100'000ULL), 0xffff});
    ASSERT_FALSE(result2.is_error());
  }

  // Timers id 0 to 8 inclusive, but not 4 error on 0xffff+1 ticks.
  for (uint64_t i = 0; i < 9; ++i) {
    if (i == 4) {
      continue;
    }
    auto result =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0x1'0000});
    ASSERT_TRUE(result.is_error());
  }

  // Timer id 4 supports 64 bits of ticks for 1, 10 and 100 usecs.
  auto result0 = client_->Start({4ULL, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL),
                                 0xffff'ffff'ffff'ffffULL});
  ASSERT_FALSE(result0.is_error());
  auto result1 =
      client_->Start({4ULL, fuchsia_hardware_hrtimer::Resolution::WithDuration(10'000ULL),
                      0xffff'ffff'ffff'ffffULL});
  ASSERT_FALSE(result1.is_error());
  auto result2 =
      client_->Start({4ULL, fuchsia_hardware_hrtimer::Resolution::WithDuration(100'000ULL),
                      0xffff'ffff'ffff'ffffULL});
  ASSERT_FALSE(result2.is_error());
}

TEST_F(DriverTest, StartStop) {
  // Timers id 0 to 8 inclusive but not 4 support events.
  for (uint64_t i = 0; i < 9; ++i) {
    auto result_start =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 1});
    ASSERT_FALSE(result_start.is_error());
  }
  // Timers are started.
  RunInEnvironmentTypeContext([](TestEnvironment& env) {
    ASSERT_EQ(env.platform_device().mmio()[0x3c50], 0x000f'0100UL);  // Timers A, B, C and D.
    // Timer E is always started.
    ASSERT_EQ(env.platform_device().mmio()[0x3c64], 0x000f'0000UL);  // Timers F, G, H and I.
  });

  for (uint64_t i = 0; i < 9; ++i) {
    auto result_stop = client_->Stop(i);
    ASSERT_FALSE(result_stop.is_error());
  }
  // Timers are stopped.
  RunInEnvironmentTypeContext([](TestEnvironment& env) {
    ASSERT_EQ(env.platform_device().mmio()[0x3c50], 0x0000'0100UL);  // Timers A, B, C and D.
    // Timer E can't actually be stopped.
    ASSERT_EQ(env.platform_device().mmio()[0x3c64], 0x0000'0000UL);  // Timers F, G, H and I.
  });
}

TEST_F(DriverTest, GetTicks) {
  constexpr uint32_t kArbitraryCount16bits = 0x1234;
  RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.platform_device().mmio()[0x3c51] = kArbitraryCount16bits << 16;  // Timer A.
    env.platform_device().mmio()[0x3c52] = kArbitraryCount16bits << 16;  // Timer B.
    env.platform_device().mmio()[0x3c53] = kArbitraryCount16bits << 16;  // Timer C.
    env.platform_device().mmio()[0x3c54] = kArbitraryCount16bits << 16;  // Timer D.
    // Gap and no timer E.
    env.platform_device().mmio()[0x3c65] = kArbitraryCount16bits << 16;  // Timer F.
    env.platform_device().mmio()[0x3c66] = kArbitraryCount16bits << 16;  // Timer G.
    env.platform_device().mmio()[0x3c67] = kArbitraryCount16bits << 16;  // Timer H.
    env.platform_device().mmio()[0x3c68] = kArbitraryCount16bits << 16;  // Timer I.
  });
  // Timers id 0 to 8 inclusive but not 4 support only 16 bits.
  for (uint64_t i = 0; i < 9; ++i) {
    if (i == 4) {
      continue;
    }
    auto result_stop = client_->Stop(i);
    ASSERT_FALSE(result_stop.is_error());

    auto result = client_->GetTicksLeft(i);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryCount16bits);
  }

  // Timer id 4 support 64 bits.
  constexpr uint64_t kArbitraryTicksRequest = 0xffff'ffff'ffff'ffff;
  constexpr uint64_t kArbitraryCount64bits = 0xffff'ffff'ffff'fff0;
  // Request a number of ticks first since this timer counts up so the driver subtracts.
  auto result_start = client_->Start(
      {4, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), kArbitraryTicksRequest});
  ASSERT_FALSE(result_start.is_error());
  // We set the amount read after starting the timer since starting the timer writes to the register
  // we read upon GetTicksLeft.
  RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.platform_device().mmio()[0x3c62] =
        static_cast<uint32_t>(kArbitraryCount64bits);  // Timer E.
    env.platform_device().mmio()[0x3c63] =
        static_cast<uint32_t>(kArbitraryCount64bits >> 32);  // Timer E High.
  });
  auto result_ticks = client_->GetTicksLeft(4);
  ASSERT_FALSE(result_ticks.is_error());
  ASSERT_EQ(result_ticks->ticks(), kArbitraryTicksRequest - kArbitraryCount64bits);

  // Since we can't really stop the timer 4 from ticking after a Stop(), GetTicksLeft() starts
  // to return 0.
  auto result_stop = client_->Stop(4);
  ASSERT_FALSE(result_stop.is_error());
  {
    auto result_ticks = client_->GetTicksLeft(4);
    ASSERT_FALSE(result_ticks.is_error());
    ASSERT_EQ(result_ticks->ticks(), 0ULL);
  }
}

TEST_F(DriverTest, EventTriggering) {
  // Timers id 0 to 8 inclusive but not 4 support events.
  zx::event events[9];
  for (uint64_t i = 0; i < 9; ++i) {
    if (i == 4) {
      continue;
    }
    ASSERT_EQ(zx::event::create(0, &events[i]), ZX_OK);
    zx::event duplicate_event;
    events[i].duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate_event);
    auto result = client_->SetEvent({i, std::move(duplicate_event)});
    ASSERT_FALSE(result.is_error());
  }

  RunInEnvironmentTypeContext([](TestEnvironment& env) { env.platform_device().TriggerAllIrqs(); });

  for (uint64_t i = 0; i < 9; ++i) {
    if (i == 4) {
      continue;
    }
    zx_signals_t signals = {};
    ASSERT_EQ(events[i].wait_one(ZX_EVENT_SIGNALED, zx::time::infinite(), &signals), ZX_OK);
  }
}

}  // namespace hrtimer
