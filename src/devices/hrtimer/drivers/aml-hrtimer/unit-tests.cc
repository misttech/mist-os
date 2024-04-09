// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>
#include <lib/fake-bti/bti.h>
#include <lib/fzl/vmo-mapper.h>

#include "src/devices/hrtimer/drivers/aml-hrtimer/aml-hrtimer.h"

namespace hrtimer {

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

  void TriggerAllIrqs() {
    for (size_t i = 0; i < AmlHrtimer::GetNumberOfIrqs(); ++i) {
      ASSERT_EQ(fake_interrupts_[i].trigger(0, zx::clock::get_monotonic()), ZX_OK);
    }
  }

 private:
  static constexpr size_t kMmioSize = 0x10000;

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
        .strength = fuchsia_hardware_power::RequirementType::kActive,
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
class FakeSystemActivityGovernor : public fidl::Server<fuchsia_power_system::ActivityGovernor> {
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
        {.active_dependency_token = std::move(duplicate)}};

    elements = {{.wake_handling = std::move(wake_handling)}};

    completer.Reply({{std::move(elements)}});
  }

  void RegisterListener(RegisterListenerRequest& request,
                        RegisterListenerCompleter::Sync& completer) override {}

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  zx::event wake_handling_;
  fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor> bindings_;
};

class FakeLeaseControl : public fidl::Server<fuchsia_power_broker::LeaseControl> {
 public:
  void WatchStatus(fuchsia_power_broker::LeaseControlWatchStatusRequest& request,
                   WatchStatusCompleter::Sync& completer) override {
    completer.Reply(lease_status_);
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::LeaseControl> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  fuchsia_power_broker::LeaseStatus lease_status_ = fuchsia_power_broker::LeaseStatus::kSatisfied;
};

class FakeLessor : public fidl::Server<fuchsia_power_broker::Lessor> {
 public:
  void Lease(fuchsia_power_broker::LessorLeaseRequest& request,
             LeaseCompleter::Sync& completer) override {
    auto lease_control = fidl::CreateEndpoints<fuchsia_power_broker::LeaseControl>();
    auto lease_control_impl = std::make_unique<FakeLeaseControl>();
    lease_control_ = lease_control_impl.get();
    lease_control_binding_ = fidl::BindServer<fuchsia_power_broker::LeaseControl>(
        fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(lease_control->server),
        std::move(lease_control_impl),
        [this](FakeLeaseControl* impl, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_power_broker::LeaseControl> server_end) mutable {
          lease_requested_ = false;
        });

    lease_requested_ = true;
    completer.Reply(fit::success(std::move(lease_control->client)));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Lessor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  bool GetLeaseRequested() { return lease_requested_; }

 private:
  bool lease_requested_ = false;
  FakeLeaseControl* lease_control_;
  std::optional<fidl::ServerBindingRef<fuchsia_power_broker::LeaseControl>> lease_control_binding_;
};

class FakePowerBroker : public fidl::Server<fuchsia_power_broker::Topology> {
 public:
  fidl::ProtocolHandler<fuchsia_power_broker::Topology> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

  void AddElement(fuchsia_power_broker::ElementSchema& request,
                  AddElementCompleter::Sync& completer) override {
    auto element_control = fidl::CreateEndpoints<fuchsia_power_broker::ElementControl>();
    element_control_server_ = std::move(element_control->server);
    if (request.lessor_channel()) {
      auto lessor_impl = std::make_unique<FakeLessor>();
      wake_lessor_ = lessor_impl.get();
      fidl::ServerBindingRef<fuchsia_power_broker::Lessor> lessor_binding =
          fidl::BindServer<fuchsia_power_broker::Lessor>(
              fdf::Dispatcher::GetCurrent()->async_dispatcher(),
              std::move(*request.lessor_channel()), std::move(lessor_impl),
              [](FakeLessor* impl, fidl::UnbindInfo info,
                 fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) mutable {});
    }

    fuchsia_power_broker::TopologyAddElementResponse result{
        {.element_control_channel = std::move(element_control->client)},
    };

    completer.Reply(fit::success(std::move(result)));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Topology> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  fidl::ServerEnd<fuchsia_power_broker::ElementControl>& element_control_server() {
    return element_control_server_;
  }
  bool GetLeaseRequested() { return wake_lessor_ && wake_lessor_->GetLeaseRequested(); }

 private:
  FakeLessor* wake_lessor_ = nullptr;
  fidl::ServerEnd<fuchsia_power_broker::ElementControl> element_control_server_;
  fidl::ServerBindingGroup<fuchsia_power_broker::Topology> bindings_;
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
    auto result_broker =
        to_driver_vfs.component().AddUnmanagedProtocol<fuchsia_power_broker::Topology>(
            power_broker_.CreateHandler());
    EXPECT_EQ(ZX_OK, result_broker.status_value());
    return zx::ok();
  }
  FakePlatformDevice& platform_device() { return platform_device_; }
  FakePowerBroker& power_broker() { return power_broker_; }

 private:
  FakePlatformDevice platform_device_;
  std::optional<FakeSystemActivityGovernor> system_activity_governor_;
  FakePowerBroker power_broker_;
  zx::event wake_handling_;
};

class FixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = false;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = false;

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

  void CheckLeaseRequested(size_t timer_id) {
    RunInEnvironmentTypeContext([](TestEnvironment& env) {
      ASSERT_FALSE(env.power_broker().GetLeaseRequested());
      env.platform_device().TriggerAllIrqs();
    });
    auto result_start = client_->StartAndWait(
        {timer_id, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
    ASSERT_FALSE(result_start.is_error());
    ASSERT_TRUE(result_start->keep_alive().is_valid());
    RunInEnvironmentTypeContext(
        [](TestEnvironment& env) { ASSERT_TRUE(env.power_broker().GetLeaseRequested()); });
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
  // Timers id 0 to 8 inclusive but not 4 support events (via IRQ notification).
  zx::event events[9];
  for (uint64_t i = 0; i < 9; ++i) {
    if (i == 4) {
      continue;
    }
    ASSERT_EQ(zx::event::create(0, &events[i]), ZX_OK);
    zx::event duplicate_event;
    events[i].duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate_event);
    auto result_event = client_->SetEvent({i, std::move(duplicate_event)});
    ASSERT_FALSE(result_event.is_error());
    auto result_start =
        client_->Start({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
    ASSERT_FALSE(result_start.is_error());
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

TEST_F(DriverTest, PowerLeaseControl) {
  // Element control server in the driver is the same as provided by the fake SAG.
  zx_info_handle_basic_t broker_element_control, driver_element_control;
  RunInDriverContext([&](AmlHrtimer& driver) {
    zx_status_t status = driver.element_control()->channel().get_info(
        ZX_INFO_HANDLE_BASIC, &driver_element_control, sizeof(zx_info_handle_basic_t), nullptr,
        nullptr);
    ASSERT_EQ(status, ZX_OK);
  });
  RunInEnvironmentTypeContext([&](TestEnvironment& env) {
    zx_status_t status = env.power_broker().element_control_server().channel().get_info(
        ZX_INFO_HANDLE_BASIC, &broker_element_control, sizeof(zx_info_handle_basic_t), nullptr,
        nullptr);
    ASSERT_EQ(status, ZX_OK);
  });
  ASSERT_EQ(broker_element_control.koid, driver_element_control.related_koid);
}

TEST_F(DriverTest, WaitTriggering) {
  RunInEnvironmentTypeContext([](TestEnvironment& env) { env.platform_device().TriggerAllIrqs(); });

  // Timers id 0 to 8 inclusive but not 4 support wait (via IRQ notification).
  for (uint64_t i = 0; i < 9; ++i) {
    if (i == 4) {
      continue;
    }
    auto result_start =
        client_->StartAndWait({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
    ASSERT_FALSE(result_start.is_error());
    ASSERT_TRUE(result_start->keep_alive().is_valid());
  }
}

TEST_F(DriverTest, WaitStop) {
  // Timers id 0 to 8 inclusive but not 4 support wait (via IRQ notification).
  for (uint64_t i = 0; i < 9; ++i) {
    if (i == 4) {
      continue;
    }
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
      RunInDriverContext([i, &has_wait_completer](AmlHrtimer& driver) {
        has_wait_completer = driver.HasWaitCompleter(i);
      });
      zx::nanosleep(zx::deadline_after(zx::msec(1)));
    }

    auto result_start_stop = client_->Stop(i);
    ASSERT_FALSE(result_start_stop.is_error());
    thread.join();
  }
}

TEST_F(DriverTest, CancelOnDriverStop) {
  std::vector<std::thread> threads;
  zx::event events[9];
  // Timers id 0 to 8 inclusive but not 4 support events and wait (via IRQ notification).
  for (uint64_t i = 0; i < 9; ++i) {
    if (i == 4) {
      continue;
    }
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
      RunInDriverContext([i, &has_wait_completer](AmlHrtimer& driver) {
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
  auto result_stop_driver = StopDriver();
  ASSERT_FALSE(result_stop_driver.is_error());

  // Join the threads such that we check for timers canceled.
  for (auto& thread : threads) {
    thread.join();
  }
}

// TODO(https://fxbug.dev/332975913): deflake and reenable.
TEST_F(DriverTest, DISABLED_LeaseRequested0) { CheckLeaseRequested(0); }
TEST_F(DriverTest, DISABLED_LeaseRequested1) { CheckLeaseRequested(1); }
TEST_F(DriverTest, DISABLED_LeaseRequested2) { CheckLeaseRequested(2); }
TEST_F(DriverTest, DISABLED_LeaseRequested3) { CheckLeaseRequested(3); }

TEST_F(DriverTest, DISABLED_LeaseNotRequested4) {
  RunInEnvironmentTypeContext(
      [](TestEnvironment& env) { ASSERT_FALSE(env.power_broker().GetLeaseRequested()); });
  auto result_start =
      client_->StartAndWait({4, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
  ASSERT_TRUE(result_start.is_error());
  RunInEnvironmentTypeContext(
      [](TestEnvironment& env) { ASSERT_FALSE(env.power_broker().GetLeaseRequested()); });
}

TEST_F(DriverTest, DISABLED_LeaseRequested5) { CheckLeaseRequested(5); }
TEST_F(DriverTest, DISABLED_LeaseRequested6) { CheckLeaseRequested(6); }
TEST_F(DriverTest, DISABLED_LeaseRequested7) { CheckLeaseRequested(7); }
TEST_F(DriverTest, DISABLED_LeaseRequested8) { CheckLeaseRequested(8); }

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
  static constexpr bool kDriverOnForeground = false;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = false;

  using DriverType = AmlHrtimer;
  using EnvironmentType = TestEnvironmentNoPower;
};

class DriverTestNoPower : public fdf_testing::DriverTestFixture<FixtureConfigNoPower> {
 protected:
  void SetUp() override {
    zx::result device_result = ConnectThroughDevfs<fuchsia_hardware_hrtimer::Device>("aml-hrtimer");
    ASSERT_EQ(ZX_OK, device_result.status_value());
    client_.Bind(std::move(device_result.value()));
  }

  fidl::SyncClient<fuchsia_hardware_hrtimer::Device> client_;
};

TEST_F(DriverTestNoPower, WaitTriggeringNoPower) {
  RunInEnvironmentTypeContext(
      [](TestEnvironmentNoPower& env) { env.platform_device().TriggerAllIrqs(); });

  // Timers id 0 to 8 inclusive but not 4 support wait (via IRQ notification).
  for (uint64_t i = 0; i < 9; ++i) {
    if (i == 4) {
      continue;
    }
    auto result_start =
        client_->StartAndWait({i, fuchsia_hardware_hrtimer::Resolution::WithDuration(1'000ULL), 0});
    ASSERT_TRUE(result_start.is_error());  // Must fail, no power configuration.
    ASSERT_EQ(result_start.error_value().domain_error(),
              fuchsia_hardware_hrtimer::DriverError::kNotSupported);
  }
}

}  // namespace hrtimer
