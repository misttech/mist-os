// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/test_base.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/power/cpp/testing/fake_current_level.h>
#include <lib/driver/power/cpp/testing/fake_element_control.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/fake-bti/bti.h>
#include <lib/fpromise/result.h>
#include <lib/fpromise/single_threaded_executor.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/inspect/cpp/reader.h>

#include <gtest/gtest.h>

#include "src/devices/hrtimer/drivers/aml-hrtimer/aml-hrtimer.h"
#include "src/devices/hrtimer/drivers/aml-hrtimer/aml_hrtimer_config.h"

namespace hrtimer {

using fdf_power::testing::FakeCurrentLevel;
using fdf_power::testing::FakeElementControl;

class FakePlatformDevice : public fidl::Server<fuchsia_hardware_platform_device::Device> {
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

  void TriggerIrq(size_t timer_index) {
    ASSERT_TRUE(timer_index < kNumberOfTimers);
    ASSERT_EQ(fake_interrupts_[*kTimerToIrqsIndexes[timer_index]].trigger(0, zx::clock::get_boot()),
              ZX_OK);
  }

  void TriggerAllIrqs() {
    for (size_t i = 0; i < AmlHrtimer::GetNumberOfIrqs(); ++i) {
      ASSERT_EQ(fake_interrupts_[i].trigger(0, zx::clock::get_boot()), ZX_OK);
    }
  }

 private:
  static constexpr size_t kMmioSize = 0x10000;
  std::optional<size_t> kTimerToIrqsIndexes[kNumberOfTimers] = {0, 1, 2, 3, std::nullopt,
                                                                4, 5, 6, 7};
  void GetMmioById(GetMmioByIdRequest& request, GetMmioByIdCompleter::Sync& completer) override {
    if (request.index() != 0) {
      return completer.Reply(zx::error(ZX_ERR_OUT_OF_RANGE));
    }

    zx::vmo vmo;
    if (zx_status_t status = mmio_.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo); status != ZX_OK) {
      return completer.Reply(zx::error(status));
    }

    completer.Reply(zx::ok(fuchsia_hardware_platform_device::Mmio{{
        .offset = 0,
        .size = kMmioSize,
        .vmo = std::move(vmo),
    }}));
  }

  void GetMmioByName(GetMmioByNameRequest& request,
                     GetMmioByNameCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetInterruptById(GetInterruptByIdRequest& request,
                        GetInterruptByIdCompleter::Sync& completer) override {
    if (request.index() >= AmlHrtimer::GetNumberOfIrqs()) {
      completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
      return;
    }
    zx::interrupt interrupt;
    ASSERT_EQ(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL,
                                    &fake_interrupts_[request.index()]),
              ZX_OK);
    zx_status_t status =
        fake_interrupts_[request.index()].duplicate(ZX_RIGHT_SAME_RIGHTS, &interrupt);
    if (status != ZX_OK) {
      completer.Reply(zx::error(status));
      return;
    }
    completer.Reply(zx::ok(std::move(interrupt)));
  }

  void GetInterruptByName(GetInterruptByNameRequest& request,
                          GetInterruptByNameCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetBtiById(GetBtiByIdRequest& request, GetBtiByIdCompleter::Sync& completer) override {
    zx::bti bti;
    if (zx_status_t status = bti_.duplicate(ZX_RIGHT_SAME_RIGHTS, &bti); status != ZX_OK) {
      return completer.Reply(zx::error(status));
    }
    completer.Reply(zx::ok((std::move(bti))));
  }

  void GetBtiByName(GetBtiByNameRequest& request, GetBtiByNameCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetSmcById(GetSmcByIdRequest& request, GetSmcByIdCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetNodeDeviceInfo(GetNodeDeviceInfoCompleter::Sync& completer) override {
    fuchsia_hardware_platform_device::NodeDeviceInfo info;
    info.vid(PDEV_VID_AMLOGIC).pid(PDEV_PID_AMLOGIC_A311D);
    completer.Reply(zx::ok(std::move(info)));
  }

  void GetSmcByName(GetSmcByNameRequest& request, GetSmcByNameCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetMetadata(GetMetadataRequest& request, GetMetadataCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetMetadata2(GetMetadata2Request& request, GetMetadata2Completer::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_platform_device::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  void MapMmio() { mapped_mmio_.Map(mmio_); }

  void GetPowerConfiguration(GetPowerConfigurationCompleter::Sync& completer) override {
    // fuchsia_hardware_power uses FIDL uint8 for power levels matching fuchsia_power_broker's.
    constexpr uint8_t kPowerLevelOff =
        static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOff);
    constexpr uint8_t kPowerLevelOn =
        static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOn);
    constexpr char kPowerElementName[] = "aml-hrtimer-wake";
    fuchsia_hardware_power::LevelTuple wake_handling_on = {{
        .child_level = kPowerLevelOn,
        .parent_level = static_cast<uint8_t>(fuchsia_power_system::WakeHandlingLevel::kActive),
    }};
    fuchsia_hardware_power::PowerDependency wake_handling = {{
        .child = kPowerElementName,
        .parent = fuchsia_hardware_power::ParentElement::WithSag(
            fuchsia_hardware_power::SagElement::kWakeHandling),
        .level_deps = {{std::move(wake_handling_on)}},
        .strength = fuchsia_hardware_power::RequirementType::kAssertive,
    }};
    fuchsia_hardware_power::PowerLevel off = {{.level = kPowerLevelOff, .name = "off"}};
    fuchsia_hardware_power::PowerLevel on = {{.level = kPowerLevelOn, .name = "on"}};
    fuchsia_hardware_power::PowerElement element = {
        {.name = kPowerElementName, .levels = {{std::move(off), std::move(on)}}}};
    fuchsia_hardware_power::PowerElementConfiguration wake_config = {
        {.element = std::move(element), .dependencies = {{std::move(wake_handling)}}}};

    completer.Reply(zx::ok(
        std::vector<fuchsia_hardware_power::PowerElementConfiguration>{{std::move(wake_config)}}));
  }

  zx::vmo mmio_;
  fzl::VmoMapper mapped_mmio_;
  zx::bti bti_;
  zx::interrupt fake_interrupts_[AmlHrtimer::GetNumberOfIrqs()];

  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> bindings_;
};

// Power Specific.
// TODO(https://fxbug.dev/342124966): Move to the Power Framework Testing Client
// at //src/power/testing/client.
class FakeSystemActivityGovernor
    : public fidl::testing::TestBase<fuchsia_power_system::ActivityGovernor> {
 public:
  FakeSystemActivityGovernor(zx::event wake_handling) : wake_handling_(std::move(wake_handling)) {}

  fidl::ProtocolHandler<fuchsia_power_system::ActivityGovernor> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override {
    fuchsia_power_system::PowerElements elements;
    zx::event duplicate;
    wake_handling_.duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate);

    fuchsia_power_system::WakeHandling wake_handling = {
        {.assertive_dependency_token = std::move(duplicate)}};

    elements = {{.wake_handling = std::move(wake_handling)}};

    completer.Reply({{std::move(elements)}});
  }

  void TakeWakeLease(TakeWakeLeaseRequest& request,
                     TakeWakeLeaseCompleter::Sync& completer) override {
    zx::eventpair wake_lease_remote, wake_lease_local;
    zx::eventpair::create(0, &wake_lease_local, &wake_lease_remote);
    wake_leases_.push_back(std::move(wake_lease_local));
    lease_requested_ = true;
    completer.Reply(std::move(wake_lease_remote));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    ADD_FAILURE() << name << " is not implemented";
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  bool GetLeaseRequested() { return lease_requested_; }

 private:
  bool lease_requested_ = false;
  zx::event wake_handling_;
  std::vector<zx::eventpair> wake_leases_;
  fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor> bindings_;
};

class TestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    platform_device_.InitResources();
    auto result = to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
        platform_device_.GetInstanceHandler());
    EXPECT_EQ(ZX_OK, result.status_value());

    // Power specific.
    zx::event::create(0, &wake_handling_);
    zx::event duplicate;
    EXPECT_EQ(wake_handling_.duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate), ZX_OK);
    system_activity_governor_.emplace(std::move(duplicate));
    auto result_sag =
        to_driver_vfs.component().AddUnmanagedProtocol<fuchsia_power_system::ActivityGovernor>(
            system_activity_governor_->CreateHandler());
    EXPECT_EQ(ZX_OK, result_sag.status_value());
    return zx::ok();
  }
  FakePlatformDevice& platform_device() { return platform_device_; }
  FakeSystemActivityGovernor& system_activity_governor() { return *system_activity_governor_; }

 private:
  FakePlatformDevice platform_device_;
  std::optional<FakeSystemActivityGovernor> system_activity_governor_;
  zx::event wake_handling_;
};

class FixtureConfig final {
 public:
  using DriverType = AmlHrtimer;
  using EnvironmentType = TestEnvironment;
};

class DriverTest : public ::testing::Test {
 public:
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  void SetUp() override {
    zx::result<> result =
        driver_test().StartDriverWithCustomStartArgs([](fdf::DriverStartArgs& start_args) mutable {
          aml_hrtimer_config::Config fake_config;
          fake_config.enable_suspend() = true;
          start_args.config(fake_config.ToVmo());
        });
    ASSERT_EQ(ZX_OK, result.status_value());
    zx::result device_result =
        driver_test().ConnectThroughDevfs<fuchsia_hardware_hrtimer::Device>("aml-hrtimer");
    ASSERT_EQ(ZX_OK, device_result.status_value());
    client_.Bind(std::move(device_result.value()));
  }

  void CheckLeaseRequested(size_t timer_id) {
    driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
      ASSERT_FALSE(env.system_activity_governor().GetLeaseRequested());
    });
    zx::eventpair lease;
    std::thread thread([this, timer_id, &lease]() {
      auto result_start = client_->StartAndWait(
          {timer_id, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
      ASSERT_FALSE(result_start.is_error());
      ASSERT_TRUE(result_start->keep_alive().is_valid());
      lease = std::move(result_start->keep_alive());
    });

    // Wait until the driver has acquired the timer wait completer before triggering the IRQ.
    bool has_wait_completer = false;
    while (!has_wait_completer) {
      driver_test().RunInDriverContext([timer_id, &has_wait_completer](AmlHrtimer& driver) {
        has_wait_completer = driver.HasWaitCompleter(timer_id);
      });
      zx::nanosleep(zx::deadline_after(zx::msec(1)));
    }
    driver_test().RunInEnvironmentTypeContext(
        [timer_id](TestEnvironment& env) { env.platform_device().TriggerIrq(timer_id); });
    thread.join();
    driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
      ASSERT_TRUE(env.system_activity_governor().GetLeaseRequested());
    });
  }

  void CheckInspect(const char* path, const char* type, uint64_t id, uint64_t data) {
    driver_test().RunInDriverContext([&](AmlHrtimer& driver) {
      auto& inspector = driver.inspect();
      fpromise::single_threaded_executor executor;
      executor.schedule_task(inspect::ReadFromInspector(inspector).then(
          [&](fpromise::result<inspect::Hierarchy>& hierarchy) {
            ASSERT_TRUE(hierarchy.is_ok());
            const inspect::Hierarchy* events =
                hierarchy.value().GetByPath({"hrtimer-trace", "events"});
            ASSERT_TRUE(events);
            const auto* event = events->GetByPath({path});
            auto local_id = event->node().get_property<inspect::UintPropertyValue>("id")->value();
            auto local_type =
                event->node().get_property<inspect::StringPropertyValue>("type")->value();
            auto local_data =
                event->node().get_property<inspect::UintPropertyValue>("data")->value();
            ASSERT_EQ(local_type.compare(type), 0);
            ASSERT_EQ(local_id, id);
            ASSERT_EQ(local_data, data);
          }));
      executor.run();
    });
  }

  fdf_testing::BackgroundDriverTest<FixtureConfig>& driver_test() { return driver_test_; }

  fdf_testing::BackgroundDriverTest<FixtureConfig> driver_test_;

  fidl::SyncClient<fuchsia_hardware_hrtimer::Device> client_;
};

TEST_F(DriverTest, Properties) {
  auto result = client_->GetProperties();
  ASSERT_FALSE(result.is_error());
  ASSERT_FALSE(result->properties().IsEmpty());
  ASSERT_TRUE(result->properties().timers_properties().has_value());
  ASSERT_EQ(result->properties().timers_properties()->size(), std::size_t{9});
  auto& timers = result->properties().timers_properties().value();

  // Resolutions and range for all timers, except timer id 4.
  for (auto& i : kTimersAll) {
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
    if (i >= 5 && i <= 8) {
      ASSERT_EQ(timers[i].max_ticks().value(), 0xffff'ffff'ffff'ffffULL);  // extended max ticks.
    } else {
      ASSERT_EQ(timers[i].max_ticks().value(), 0xffffULL);
    }
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
  // All timers are able to take a 0 ticks expiration request for durations 1, 10, and 100 usecs.
  for (auto& i : kTimersAll) {
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
  // All timers are able to take a up to 0xffff ticks expiration request for durations 1, 10, and
  // 100 usecs.
  for (auto& i : kTimersAll) {
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

  // Timers id 0 to 3 inclusive error on 0xffff+1 ticks.
  for (uint64_t i = 0; i < 4; ++i) {
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

  // Timers id 5 to 8 inclusive have no error on 0xffff+1 ticks.
  for (uint64_t i = 5; i < 9; ++i) {
    auto result =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0x1'0000});
    ASSERT_FALSE(result.is_error());
  }
}

TEST_F(DriverTest, StartStop) {
  // All timers support start/stop.
  for (auto& i : kTimersAll) {
    auto result_start =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 1});
    ASSERT_FALSE(result_start.is_error());
  }
  // Timers are started.
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    ASSERT_EQ(env.platform_device().mmio()[0x3c50], 0x000f'0100UL);  // Timers A, B, C and D.
    // Timer E is always started.
    ASSERT_EQ(env.platform_device().mmio()[0x3c64], 0x000f'0000UL);  // Timers F, G, H and I.
  });

  for (auto& i : kTimersAll) {
    auto result_stop = client_->Stop(i);
    ASSERT_FALSE(result_stop.is_error());
  }
  // Timers are stopped.
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    ASSERT_EQ(env.platform_device().mmio()[0x3c50], 0x0000'0100UL);  // Timers A, B, C and D.
    // Timer E can't actually be stopped.
    ASSERT_EQ(env.platform_device().mmio()[0x3c64], 0x0000'0000UL);  // Timers F, G, H and I.
  });

  CheckInspect("0", "Start", 0, 1);
  CheckInspect("1", "StartHardware", 0, 1);
  CheckInspect("2", "Start", 1, 1);
  CheckInspect("3", "StartHardware", 1, 1);
  CheckInspect("4", "Start", 2, 1);
  CheckInspect("5", "StartHardware", 2, 1);
  CheckInspect("6", "Start", 3, 1);
  CheckInspect("7", "StartHardware", 3, 1);
  CheckInspect("8", "Start", 4, 1);
  CheckInspect("9", "StartHardware", 4, 0);  // Timer 4 does not set ticks in the HW.
  CheckInspect("10", "Start", 5, 1);
  CheckInspect("11", "StartHardware", 5, 1);
  CheckInspect("12", "Start", 6, 1);
  CheckInspect("13", "StartHardware", 6, 1);
  CheckInspect("14", "Start", 7, 1);
  CheckInspect("15", "StartHardware", 7, 1);
  CheckInspect("16", "Start", 8, 1);
  CheckInspect("17", "StartHardware", 8, 1);
  CheckInspect("18", "Stop", 0, 0);
  CheckInspect("19", "Stop", 1, 0);
  CheckInspect("20", "Stop", 2, 0);
  CheckInspect("21", "Stop", 3, 0);
  CheckInspect("22", "Stop", 4, 0);
  CheckInspect("23", "Stop", 5, 0);
  CheckInspect("24", "Stop", 6, 0);
  CheckInspect("25", "Stop", 7, 0);
  CheckInspect("26", "Stop", 8, 0);
}

TEST_F(DriverTest, EventTriggering) {
  zx::event events[kNumberOfTimers];
  for (auto& i : kTimersSupportWait) {
    ASSERT_EQ(zx::event::create(0, &events[i]), ZX_OK);
    zx::event duplicate_event;
    events[i].duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate_event);
    auto result_event = client_->SetEvent({i, std::move(duplicate_event)});
    ASSERT_FALSE(result_event.is_error());
    auto result_start =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
    ASSERT_FALSE(result_start.is_error());
  }

  driver_test().RunInEnvironmentTypeContext(
      [](TestEnvironment& env) { env.platform_device().TriggerAllIrqs(); });

  for (auto& i : kTimersSupportWait) {
    zx_signals_t signals = {};
    ASSERT_EQ(events[i].wait_one(ZX_EVENT_SIGNALED, zx::time::infinite(), &signals), ZX_OK);
  }
}

TEST_F(DriverTest, GetTicksTimers0123) {
  // Can start up to 16 bits.
  constexpr uint64_t kArbitraryTicksRequest = 0xffff;

  constexpr uint32_t kArbitraryCount16bits0 = 0x1234;
  constexpr uint32_t kArbitraryCount16bits1 = 0x5678;
  constexpr uint32_t kArbitraryCount16bits2 = 0x90ab;
  constexpr uint32_t kArbitraryCount16bits3 = 0xcdef;
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.platform_device().mmio()[0x3c51] = kArbitraryCount16bits0 << 16;  // Timer A.
    env.platform_device().mmio()[0x3c52] = kArbitraryCount16bits1 << 16;  // Timer B.
    env.platform_device().mmio()[0x3c53] = kArbitraryCount16bits2 << 16;  // Timer C.
    env.platform_device().mmio()[0x3c54] = kArbitraryCount16bits3 << 16;  // Timer D.
  });
  for (uint64_t i = 0; i < 4; ++i) {
    auto result_start = client_->Start(
        {i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), kArbitraryTicksRequest});
    ASSERT_FALSE(result_start.is_error());
  }

  // Reads from the registers.
  {
    auto result = client_->GetTicksLeft(0);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryCount16bits0);
  }
  {
    auto result = client_->GetTicksLeft(1);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryCount16bits1);
  }
  {
    auto result = client_->GetTicksLeft(2);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryCount16bits2);
  }
  {
    auto result = client_->GetTicksLeft(3);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryCount16bits3);
  }
}

TEST_F(DriverTest, GetTicksTimer4) {
  // Can start up to 64 bits.
  constexpr uint64_t kArbitraryTicksRequest = 0x1234'5678'90ab'cdef;

  auto result_start = client_->Start(
      {4, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), kArbitraryTicksRequest});
  ASSERT_FALSE(result_start.is_error());
  // We set the amount read after starting the timer since starting the timer writes to the
  // register we read upon GetTicksLeft.
  constexpr uint64_t kArbitraryCount64bits = 0x1234'5678'0000'0000;
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.platform_device().mmio()[0x3c62] =
        static_cast<uint32_t>(kArbitraryCount64bits);  // Timer E.
    env.platform_device().mmio()[0x3c63] =
        static_cast<uint32_t>(kArbitraryCount64bits >> 32);  // Timer E High.
  });
  auto result_ticks = client_->GetTicksLeft(4);
  ASSERT_FALSE(result_ticks.is_error());
  // This timer counts up so the driver subtracts the read from the registers.
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

TEST_F(DriverTest, GetTicksTimers5678TicksStayAtRequested) {
  // Can start up to 64 bits because they support ticks extension.
  constexpr uint64_t kArbitraryTicksRequest = 0x1234'5678'90ab'cdef;

  // The count starts at max for the register since the request goes beyond the register max.
  constexpr uint64_t kMaxCount = 0xffff;
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.platform_device().mmio()[0x3c65] = kMaxCount << 16;  // Timer F.
    env.platform_device().mmio()[0x3c66] = kMaxCount << 16;  // Timer G.
    env.platform_device().mmio()[0x3c67] = kMaxCount << 16;  // Timer H.
    env.platform_device().mmio()[0x3c68] = kMaxCount << 16;  // Timer I.
  });
  for (uint64_t i = 5; i < 9; ++i) {
    auto result_start = client_->Start(
        {i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), kArbitraryTicksRequest});
    ASSERT_FALSE(result_start.is_error());

    // Ticks left stay at the ticks requested since the register reads 0xffff.
    auto result = client_->GetTicksLeft(i);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryTicksRequest);
  }
}

TEST_F(DriverTest, GetTicksTimers5678TicksDownBy0xffff) {
  // Can start up to 64 bits because they support ticks extension.
  constexpr uint64_t kArbitraryTicksRequest = 0x1234'5678'90ab'cdef;

  // The count has decreased by 0xffff to 0.
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.platform_device().mmio()[0x3c65] = 0 << 16;  // Timer F.
    env.platform_device().mmio()[0x3c66] = 0 << 16;  // Timer G.
    env.platform_device().mmio()[0x3c67] = 0 << 16;  // Timer H.
    env.platform_device().mmio()[0x3c68] = 0 << 16;  // Timer I.
  });
  for (uint64_t i = 5; i < 9; ++i) {
    auto result_start = client_->Start(
        {i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), kArbitraryTicksRequest});
    ASSERT_FALSE(result_start.is_error());

    // Ticks have decreased by 0xffff since the register reads 0.
    auto result = client_->GetTicksLeft(i);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryTicksRequest - 0xffff);
  }
}

TEST_F(DriverTest, GetTicksTimers5678ArbitraryCount) {
  // Can start up to 64 bits because they support ticks extension.
  constexpr uint64_t kArbitraryTicksRequest = 0x1234'5678'90ab'cdef;

  constexpr uint64_t kArbitraryCount5 = 0x1234;
  constexpr uint64_t kArbitraryCount6 = 0x5678;
  constexpr uint64_t kArbitraryCount7 = 0x90ab;
  constexpr uint64_t kArbitraryCount8 = 0xcdef;
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.platform_device().mmio()[0x3c65] = kArbitraryCount5 << 16;  // Timer F.
    env.platform_device().mmio()[0x3c66] = kArbitraryCount6 << 16;  // Timer G.
    env.platform_device().mmio()[0x3c67] = kArbitraryCount7 << 16;  // Timer H.
    env.platform_device().mmio()[0x3c68] = kArbitraryCount8 << 16;  // Timer I.
  });
  for (uint64_t i = 5; i < 9; ++i) {
    auto result_start = client_->Start(
        {i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), kArbitraryTicksRequest});
    ASSERT_FALSE(result_start.is_error());
  }

  // Ticks have decreased by 0xffff - kArbitraryCount (register read).
  {
    auto result = client_->GetTicksLeft(5);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryTicksRequest - (0xffff - kArbitraryCount5));
  }
  {
    auto result = client_->GetTicksLeft(6);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryTicksRequest - (0xffff - kArbitraryCount6));
  }
  {
    auto result = client_->GetTicksLeft(7);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryTicksRequest - (0xffff - kArbitraryCount7));
  }
  {
    auto result = client_->GetTicksLeft(8);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryTicksRequest - (0xffff - kArbitraryCount8));
  }
}

TEST_F(DriverTest, GetTicksTimers5678ArbitraryCountWithIrq) {
  // Can start up to 64 bits because they support ticks extension.
  constexpr uint64_t kTicksRequestEnoughFor2Irqs = 0x1'1235;

  constexpr uint64_t kArbitraryCount = 0x1234;
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.platform_device().mmio()[0x3c65] = kArbitraryCount << 16;  // Timer F.
    env.platform_device().mmio()[0x3c66] = kArbitraryCount << 16;  // Timer G.
    env.platform_device().mmio()[0x3c67] = kArbitraryCount << 16;  // Timer H.
    env.platform_device().mmio()[0x3c68] = kArbitraryCount << 16;  // Timer I.
  });
  for (uint64_t i = 5; i < 9; ++i) {
    auto result_start =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL),
                        kTicksRequestEnoughFor2Irqs});
    ASSERT_FALSE(result_start.is_error());

    // Because the requested ticks is biggger than 0xffff, before any IRQ triggers we'll get
    // a decrease of 0xffff - kArbitraryCount (register read).
    auto result = client_->GetTicksLeft(i);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kTicksRequestEnoughFor2Irqs - (0xffff - kArbitraryCount));
  }

  // Trigger IRQs, indicates that the first 0xffff passed.
  driver_test().RunInEnvironmentTypeContext(
      [](TestEnvironment& env) { env.platform_device().TriggerAllIrqs(); });

  for (uint64_t i = 5; i < 9; ++i) {
    // Wait until after the IRQ is handled and start ticks left fit in the hardware capabilities.
    bool start_ticks_left_fit = false;
    while (!start_ticks_left_fit) {
      driver_test().RunInDriverContext([i, &start_ticks_left_fit](AmlHrtimer& driver) {
        start_ticks_left_fit = driver.StartTicksLeftFitInHardware(i);
      });
      zx::nanosleep(zx::deadline_after(zx::msec(1)));
    }

    // Now that we have received at least an IRQ for the first 0xffff passed, GetTicksLeft starts
    // returning the read from the register.
    auto result = client_->GetTicksLeft(i);
    ASSERT_FALSE(result.is_error());
    ASSERT_EQ(result->ticks(), kArbitraryCount);
  }
}

TEST_F(DriverTest, StartAndWaitTriggering) {
  std::vector<std::thread> threads;
  for (auto& i : kTimersSupportWait) {
    threads.emplace_back([this, i]() {
      auto result_start = client_->StartAndWait(
          {i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
      ASSERT_FALSE(result_start.is_error());
      ASSERT_TRUE(result_start->keep_alive().is_valid());
    });

    // Wait until the driver has acquired the timer wait completer before triggering the IRQ.
    bool has_wait_completer = false;
    while (!has_wait_completer) {
      driver_test().RunInDriverContext([i, &has_wait_completer](AmlHrtimer& driver) {
        has_wait_completer = driver.HasWaitCompleter(i);
      });
      zx::nanosleep(zx::deadline_after(zx::msec(1)));
    }
  }
  driver_test().RunInEnvironmentTypeContext(
      [](TestEnvironment& env) { env.platform_device().TriggerAllIrqs(); });

  // Join the threads such that we check for timers triggered.
  for (auto& thread : threads) {
    thread.join();
  }

  CheckInspect("0", "StartAndWait", 0, 0);
  CheckInspect("1", "StartHardware", 0, 0);
  CheckInspect("2", "StartAndWait", 1, 0);
  CheckInspect("3", "StartHardware", 1, 0);
  CheckInspect("4", "StartAndWait", 2, 0);
  CheckInspect("5", "StartHardware", 2, 0);
  CheckInspect("6", "StartAndWait", 3, 0);
  CheckInspect("7", "StartHardware", 3, 0);
  CheckInspect("8", "StartAndWait", 5, 0);
  CheckInspect("9", "StartHardware", 5, 0);
  CheckInspect("10", "StartAndWait", 6, 0);
  CheckInspect("11", "StartHardware", 6, 0);
  CheckInspect("12", "StartAndWait", 7, 0);
  CheckInspect("13", "StartHardware", 7, 0);
  CheckInspect("14", "StartAndWait", 8, 0);
  CheckInspect("15", "StartHardware", 8, 0);
  // Not checking TriggerIrqWait since we are not ordering IRQ triggers.
}

TEST_F(DriverTest, StartAndWait2Triggering) {
  std::vector<std::thread> threads;
  for (auto& i : kTimersSupportWait) {
    threads.emplace_back([this, i]() {
      zx::eventpair local_wake_lease, remote_wake_lease;
      ASSERT_TRUE(fuchsia_power_system::LeaseToken::create(0, &local_wake_lease,
                                                           &remote_wake_lease) == ZX_OK);
      auto result_start =
          client_->StartAndWait2({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL),
                                  0, std::move(remote_wake_lease)});
      ASSERT_FALSE(result_start.is_error());
      ASSERT_TRUE(result_start->expiration_keep_alive().is_valid());
    });
    // Wait until the driver has acquired the timer wait completer before triggering the IRQ.
    bool has_wait_completer = false;
    while (!has_wait_completer) {
      driver_test().RunInDriverContext([i, &has_wait_completer](AmlHrtimer& driver) {
        has_wait_completer = driver.HasWaitCompleter(i);
      });
      zx::nanosleep(zx::deadline_after(zx::msec(1)));
    }
  }
  driver_test().RunInEnvironmentTypeContext(
      [](TestEnvironment& env) { env.platform_device().TriggerAllIrqs(); });

  // Join the threads such that we check for timers triggered.
  for (auto& thread : threads) {
    thread.join();
  }

  CheckInspect("0", "StartAndWait2", 0, 0);
  CheckInspect("1", "StartHardware", 0, 0);
  CheckInspect("2", "StartAndWait2", 1, 0);
  CheckInspect("3", "StartHardware", 1, 0);
  CheckInspect("4", "StartAndWait2", 2, 0);
  CheckInspect("5", "StartHardware", 2, 0);
  CheckInspect("6", "StartAndWait2", 3, 0);
  CheckInspect("7", "StartHardware", 3, 0);
  CheckInspect("8", "StartAndWait2", 5, 0);
  CheckInspect("9", "StartHardware", 5, 0);
  CheckInspect("10", "StartAndWait2", 6, 0);
  CheckInspect("11", "StartHardware", 6, 0);
  CheckInspect("12", "StartAndWait2", 7, 0);
  CheckInspect("13", "StartHardware", 7, 0);
  CheckInspect("14", "StartAndWait2", 8, 0);
  CheckInspect("15", "StartHardware", 8, 0);
  // Not checking TriggerIrqWait2 since we are not ordering IRQ triggers.
}

TEST_F(DriverTest, StartAndWaitStop) {
  for (auto& i : kTimersSupportWait) {
    std::thread thread([this, i]() {
      auto result_start = client_->StartAndWait(
          {i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
      ASSERT_TRUE(result_start.is_error());
      ASSERT_EQ(result_start.error_value().domain_error(),
                fuchsia_hardware_hrtimer::DriverError::kCanceled);
    });

    // Wait until the driver has acquired a wait completer such that we can cancel the timer.
    bool has_wait_completer = false;
    while (!has_wait_completer) {
      driver_test().RunInDriverContext([i, &has_wait_completer](AmlHrtimer& driver) {
        has_wait_completer = driver.HasWaitCompleter(i);
      });
      zx::nanosleep(zx::deadline_after(zx::msec(1)));
    }

    auto result_start_stop = client_->Stop(i);
    ASSERT_FALSE(result_start_stop.is_error());
    thread.join();
  }

  CheckInspect("0", "StartAndWait", 0, 0);
  CheckInspect("1", "StartHardware", 0, 0);
  CheckInspect("2", "StopWait", 0, 0);
  CheckInspect("3", "StartAndWait", 1, 0);
  CheckInspect("4", "StartHardware", 1, 0);
  CheckInspect("5", "StopWait", 1, 0);
  CheckInspect("6", "StartAndWait", 2, 0);
  CheckInspect("7", "StartHardware", 2, 0);
  CheckInspect("8", "StopWait", 2, 0);
  CheckInspect("9", "StartAndWait", 3, 0);
  CheckInspect("10", "StartHardware", 3, 0);
  CheckInspect("11", "StopWait", 3, 0);
  CheckInspect("12", "StartAndWait", 5, 0);
  CheckInspect("13", "StartHardware", 5, 0);
  CheckInspect("14", "StopWait", 5, 0);
  CheckInspect("15", "StartAndWait", 6, 0);
  CheckInspect("16", "StartHardware", 6, 0);
  CheckInspect("17", "StopWait", 6, 0);
  CheckInspect("18", "StartAndWait", 7, 0);
  CheckInspect("19", "StartHardware", 7, 0);
  CheckInspect("20", "StopWait", 7, 0);
  CheckInspect("21", "StartAndWait", 8, 0);
  CheckInspect("22", "StartHardware", 8, 0);
  CheckInspect("23", "StopWait", 8, 0);
}

TEST_F(DriverTest, StartAndWait2Stop) {
  for (auto& i : kTimersSupportWait) {
    std::thread thread([this, i]() {
      zx::eventpair local_wake_lease, remote_wake_lease;
      ASSERT_TRUE(fuchsia_power_system::LeaseToken::create(0, &local_wake_lease,
                                                           &remote_wake_lease) == ZX_OK);
      auto result_start =
          client_->StartAndWait2({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL),
                                  0, std::move(remote_wake_lease)});
      ASSERT_TRUE(result_start.is_error());
      ASSERT_EQ(result_start.error_value().domain_error(),
                fuchsia_hardware_hrtimer::DriverError::kCanceled);
    });

    // Wait until the driver has acquired a wait completer such that we can cancel the timer.
    bool has_wait_completer = false;
    while (!has_wait_completer) {
      driver_test().RunInDriverContext([i, &has_wait_completer](AmlHrtimer& driver) {
        has_wait_completer = driver.HasWaitCompleter(i);
      });
      zx::nanosleep(zx::deadline_after(zx::msec(1)));
    }

    auto result_start_stop = client_->Stop(i);
    ASSERT_FALSE(result_start_stop.is_error());
    thread.join();
  }

  CheckInspect("0", "StartAndWait2", 0, 0);
  CheckInspect("1", "StartHardware", 0, 0);
  CheckInspect("2", "StopWait2", 0, 0);
  CheckInspect("3", "StartAndWait2", 1, 0);
  CheckInspect("4", "StartHardware", 1, 0);
  CheckInspect("5", "StopWait2", 1, 0);
  CheckInspect("6", "StartAndWait2", 2, 0);
  CheckInspect("7", "StartHardware", 2, 0);
  CheckInspect("8", "StopWait2", 2, 0);
  CheckInspect("9", "StartAndWait2", 3, 0);
  CheckInspect("10", "StartHardware", 3, 0);
  CheckInspect("11", "StopWait2", 3, 0);
  CheckInspect("12", "StartAndWait2", 5, 0);
  CheckInspect("13", "StartHardware", 5, 0);
  CheckInspect("14", "StopWait2", 5, 0);
  CheckInspect("15", "StartAndWait2", 6, 0);
  CheckInspect("16", "StartHardware", 6, 0);
  CheckInspect("17", "StopWait2", 6, 0);
  CheckInspect("18", "StartAndWait2", 7, 0);
  CheckInspect("19", "StartHardware", 7, 0);
  CheckInspect("20", "StopWait2", 7, 0);
  CheckInspect("21", "StartAndWait2", 8, 0);
  CheckInspect("22", "StartHardware", 8, 0);
  CheckInspect("23", "StopWait2", 8, 0);
}

class DriverTestNoAutoStop : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result<> result =
        driver_test().StartDriverWithCustomStartArgs([](fdf::DriverStartArgs& start_args) mutable {
          aml_hrtimer_config::Config fake_config;
          fake_config.enable_suspend() = true;
          start_args.config(fake_config.ToVmo());
        });
    ASSERT_EQ(ZX_OK, result.status_value());
    zx::result device_result =
        driver_test().ConnectThroughDevfs<fuchsia_hardware_hrtimer::Device>("aml-hrtimer");
    ASSERT_EQ(ZX_OK, device_result.status_value());
    client_.Bind(std::move(device_result.value()));
  }
  fdf_testing::BackgroundDriverTest<FixtureConfig>& driver_test() { return driver_test_; }

  fdf_testing::BackgroundDriverTest<FixtureConfig> driver_test_;

  fidl::SyncClient<fuchsia_hardware_hrtimer::Device> client_;
};

TEST_F(DriverTestNoAutoStop, CancelOnDriverStop) {
  std::vector<std::thread> threads;
  zx::event events[kNumberOfTimers];
  for (auto& i : kTimersSupportWait) {
    ASSERT_EQ(zx::event::create(0, &events[i]), ZX_OK);
    zx::event duplicate_event;
    events[i].duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate_event);
    auto result_event = client_->SetEvent({i, std::move(duplicate_event)});
    ASSERT_FALSE(result_event.is_error());

    threads.emplace_back([this, i]() {
      auto result_start = client_->StartAndWait(
          {i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
      ASSERT_TRUE(result_start.is_error());
      // Check that we cancel on driver stop.
      ASSERT_EQ(result_start.error_value().domain_error(),
                fuchsia_hardware_hrtimer::DriverError::kCanceled);
    });

    // Wait until the driver has acquired a wait completer such that it can be canceled.
    bool has_wait_completer = false;
    while (!has_wait_completer) {
      driver_test().RunInDriverContext([i, &has_wait_completer](AmlHrtimer& driver) {
        has_wait_completer = driver.HasWaitCompleter(i);
      });
      zx::nanosleep(zx::deadline_after(zx::msec(1)));
    }
  }
  // Start timer 4 as well.
  auto result_start =
      client_->Start({4ULL, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL),
                      0xffff'ffff'ffff'ffffULL});
  ASSERT_FALSE(result_start.is_error());

  // Force driver stop.
  auto result_stop_driver = driver_test().StopDriver();
  ASSERT_FALSE(result_stop_driver.is_error());

  // Join the threads such that we check for timers canceled.
  for (auto& thread : threads) {
    thread.join();
  }
}

TEST_F(DriverTest, LeaseRequested0) { CheckLeaseRequested(0); }
TEST_F(DriverTest, LeaseRequested1) { CheckLeaseRequested(1); }
TEST_F(DriverTest, LeaseRequested2) { CheckLeaseRequested(2); }
TEST_F(DriverTest, LeaseRequested3) { CheckLeaseRequested(3); }

TEST_F(DriverTest, LeaseNotRequested4) {
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    ASSERT_FALSE(env.system_activity_governor().GetLeaseRequested());
  });
  auto result_start =
      client_->StartAndWait({4, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
  ASSERT_TRUE(result_start.is_error());
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    ASSERT_FALSE(env.system_activity_governor().GetLeaseRequested());
  });
}

TEST_F(DriverTest, LeaseRequested5) { CheckLeaseRequested(5); }
TEST_F(DriverTest, LeaseRequested6) { CheckLeaseRequested(6); }
TEST_F(DriverTest, LeaseRequested7) { CheckLeaseRequested(7); }
TEST_F(DriverTest, LeaseRequested8) { CheckLeaseRequested(8); }

class TestEnvironmentNoPower : public fdf_testing::Environment {
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

class FixtureConfigNoPower final {
 public:
  using DriverType = AmlHrtimer;
  using EnvironmentType = TestEnvironmentNoPower;
};

class DriverTestNoPower : public ::testing::Test {
 public:
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  void SetUp() override {
    zx::result<> result =
        driver_test().StartDriverWithCustomStartArgs([](fdf::DriverStartArgs& start_args) mutable {
          aml_hrtimer_config::Config fake_config;
          fake_config.enable_suspend() = false;
          start_args.config(fake_config.ToVmo());
        });
    ASSERT_EQ(ZX_OK, result.status_value());
    zx::result device_result =
        driver_test().ConnectThroughDevfs<fuchsia_hardware_hrtimer::Device>("aml-hrtimer");
    ASSERT_EQ(ZX_OK, device_result.status_value());
    client_.Bind(std::move(device_result.value()));
  }

  fdf_testing::BackgroundDriverTest<FixtureConfigNoPower>& driver_test() { return driver_test_; }

  fdf_testing::BackgroundDriverTest<FixtureConfigNoPower> driver_test_;

  fidl::SyncClient<fuchsia_hardware_hrtimer::Device> client_;
};

TEST_F(DriverTestNoPower, StartAndWaitTriggeringNoPower) {
  for (auto& i : kTimersSupportWait) {
    auto result_start =
        client_->StartAndWait({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
    ASSERT_TRUE(result_start.is_error());  // Must fail, no power configuration.
    ASSERT_EQ(result_start.error_value().domain_error(),
              fuchsia_hardware_hrtimer::DriverError::kBadState);
  }
}

TEST_F(DriverTestNoPower, StartAndWait2TriggeringNoPower) {
  for (auto& i : kTimersSupportWait) {
    zx::eventpair local_wake_lease, remote_wake_lease;
    ASSERT_TRUE(fuchsia_power_system::LeaseToken::create(0, &local_wake_lease,
                                                         &remote_wake_lease) == ZX_OK);
    auto result_start =
        client_->StartAndWait2({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0,
                                std::move(remote_wake_lease)});
    ASSERT_TRUE(result_start.is_error());  // Must fail, no power configuration.
    ASSERT_EQ(result_start.error_value().domain_error(),
              fuchsia_hardware_hrtimer::DriverError::kBadState);
  }
}

}  // namespace hrtimer
