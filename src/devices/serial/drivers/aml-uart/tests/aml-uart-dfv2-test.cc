// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart-dfv2.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/test_base.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/power/cpp/testing/fake_element_control.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/syslog/cpp/macros.h>

#include <bind/fuchsia/broadcom/platform/cpp/bind.h>
#include <gtest/gtest.h>

#include "fidl/fuchsia.power.broker/cpp/markers.h"
#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/serial/drivers/aml-uart/tests/device_state.h"
#include "src/devices/serial/drivers/aml-uart/tests/fake_timer.h"

namespace {

using fdf_power::testing::FakeElementControl;
using fuchsia_power_system::LeaseToken;

static constexpr fuchsia_hardware_serial::wire::SerialPortInfo kSerialInfo = {
    .serial_class = fuchsia_hardware_serial::Class::kBluetoothHci,
    .serial_vid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM,
    .serial_pid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458,
};

class FakeSystemActivityGovernor
    : public fidl::testing::TestBase<fuchsia_power_system::ActivityGovernor> {
 public:
  FakeSystemActivityGovernor() = default;

  fidl::ProtocolHandler<fuchsia_power_system::ActivityGovernor> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

  bool HasActiveWakeLease() const { return !active_wake_leases_.empty(); }

  void TakeWakeLease(TakeWakeLeaseRequest& /* ignored */,
                     TakeWakeLeaseCompleter::Sync& completer) override {
    LeaseToken client_token, server_token;
    LeaseToken::create(/*options=*/0u, &client_token, &server_token);

    // Start an async task to wait for EVENTPAIR_PEER_CLOSED signal on server_token.
    zx_handle_t token_handle = server_token.get();
    active_wake_leases_[token_handle] = std::move(server_token);
    auto wait = std::make_unique<async::WaitOnce>(token_handle, ZX_EVENTPAIR_PEER_CLOSED);
    wait->Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                [this, token_handle](async_dispatcher_t*, async::WaitOnce*, zx_status_t status,
                                     const zx_packet_signal_t*) {
                  if (status == ZX_ERR_CANCELED) {
                    return;
                  }
                  ZX_ASSERT(status == ZX_OK);
                  auto it = active_wake_leases_.find(token_handle);
                  ZX_ASSERT(it != active_wake_leases_.end());
                  ZX_ASSERT(token_handle == it->second.get());
                  active_wake_leases_.erase(it);
                });
    wait_once_tasks_.push_back(std::move(wait));
    completer.Reply(std::move(client_token));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    ADD_FAILURE() << name << " is not implemented";
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  zx::event wake_handling_;
  fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor> bindings_;
  std::unordered_map<zx_handle_t, LeaseToken> active_wake_leases_;
  std::vector<std::unique_ptr<async::WaitOnce>> wait_once_tasks_;
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
    auto lease_control_endpoints = fidl::CreateEndpoints<fuchsia_power_broker::LeaseControl>();
    lease_control_binding_.emplace(
        fdf::Dispatcher::GetCurrent()->async_dispatcher(),
        std::move(lease_control_endpoints->server), &lease_control_,
        [this](fidl::UnbindInfo info) mutable { lease_requested_ = false; });
    lease_requested_ = true;
    completer.Reply(fit::success(std::move(lease_control_endpoints->client)));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Lessor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  bool lease_requested_ = false;
  FakeLeaseControl lease_control_;
  std::optional<fidl::ServerBinding<fuchsia_power_broker::LeaseControl>> lease_control_binding_;
};

class Environment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    // Configure pdev.
    fake_pdev::FakePDevFidl::Config config;
    config.irqs[0] = {};
    EXPECT_EQ(ZX_OK,
              zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));
    state_.set_irq_signaller(config.irqs[0].borrow());
    config.mmios[0] = state_.GetMmio();
    pdev_server_.SetConfig(std::move(config));

    // Add pdev.
    async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    constexpr std::string_view kInstanceName = "pdev";
    zx::result add_service_result =
        to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
            pdev_server_.GetInstanceHandler(dispatcher), kInstanceName);
    ZX_ASSERT(add_service_result.is_ok());

    // Add power protocols.
    auto result_sag =
        to_driver_vfs.component().AddUnmanagedProtocol<fuchsia_power_system::ActivityGovernor>(
            system_activity_governor_.CreateHandler());
    EXPECT_EQ(ZX_OK, result_sag.status_value());

    // Configure and add compat.
    compat_server_.Initialize("default");

    fit::result encoded = fidl::Persist(kSerialInfo);
    ZX_ASSERT(encoded.is_ok());

    compat_server_.AddMetadata(DEVICE_METADATA_SERIAL_PORT_INFO, encoded->data(), encoded->size());
    return zx::make_result(compat_server_.Serve(dispatcher, &to_driver_vfs));
  }

  DeviceState& device_state() { return state_; }
  FakeSystemActivityGovernor& sag() { return system_activity_governor_; }

 private:
  DeviceState state_;
  fake_pdev::FakePDevFidl pdev_server_;
  FakeSystemActivityGovernor system_activity_governor_;
  compat::DeviceServer compat_server_;
};

class AmlUartTestConfig {
 public:
  using DriverType = serial::AmlUartV2;
  using EnvironmentType = Environment;
};

class AmlUartHarness : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result result =
        driver_test().StartDriverWithCustomStartArgs([&](fdf::DriverStartArgs& args) {
          aml_uart_dfv2_config::Config fake_config;
          fake_config.enable_suspend() = false;
          args.config(fake_config.ToVmo());
        });

    ASSERT_EQ(ZX_OK, result.status_value());
  }

  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  fdf::WireSyncClient<fuchsia_hardware_serialimpl::Device> CreateClient() {
    zx::result driver_connect_result =
        driver_test().Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
    if (driver_connect_result.is_error()) {
      return {};
    }
    return fdf::WireSyncClient(std::move(driver_connect_result.value()));
  }

  fdf_testing::BackgroundDriverTest<AmlUartTestConfig>& driver_test() { return driver_test_; }

 private:
  fdf_testing::BackgroundDriverTest<AmlUartTestConfig> driver_test_;
};

class AmlUartAsyncHarness : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result result =
        driver_test().StartDriverWithCustomStartArgs([&](fdf::DriverStartArgs& args) {
          aml_uart_dfv2_config::Config fake_config;
          fake_config.enable_suspend() = false;
          args.config(fake_config.ToVmo());
        });

    ASSERT_EQ(ZX_OK, result.status_value());
  }

  fdf::WireClient<fuchsia_hardware_serialimpl::Device> CreateClient() {
    zx::result driver_connect_result =
        driver_test().Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
    if (driver_connect_result.is_error()) {
      return {};
    }
    return fdf::WireClient(std::move(driver_connect_result.value()),
                           fdf::Dispatcher::GetCurrent()->get());
  }

  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  serial::AmlUart& Device() { return driver_test().driver()->aml_uart_for_testing(); }

  fdf_testing::ForegroundDriverTest<AmlUartTestConfig>& driver_test() { return driver_test_; }

 private:
  fdf_testing::ForegroundDriverTest<AmlUartTestConfig> driver_test_;
};

class AmlUartHarnessWithPower : public AmlUartHarness {
 public:
  void SetUp() override {
    zx::result result =
        driver_test().StartDriverWithCustomStartArgs([&](fdf::DriverStartArgs& args) {
          aml_uart_dfv2_config::Config fake_config;
          fake_config.enable_suspend() = true;
          args.config(fake_config.ToVmo());
        });

    ASSERT_EQ(ZX_OK, result.status_value());
  }
};

class AmlUartAsyncHarnessWithPower : public AmlUartAsyncHarness {
 public:
  void SetUp() override {
    zx::result result =
        driver_test().StartDriverWithCustomStartArgs([&](fdf::DriverStartArgs& args) {
          aml_uart_dfv2_config::Config fake_config;
          fake_config.enable_suspend() = true;
          args.config(fake_config.ToVmo());
        });

    ASSERT_EQ(ZX_OK, result.status_value());
  }
};

TEST_F(AmlUartHarness, SerialImplAsyncGetInfo) {
  auto client = CreateClient();

  fdf::Arena arena('TEST');
  auto result = client.buffer(arena)->GetInfo();
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());

  const auto& info = result->value()->info;
  ASSERT_EQ(info.serial_class, fuchsia_hardware_serial::Class::kBluetoothHci);
  ASSERT_EQ(info.serial_pid, bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458);
  ASSERT_EQ(info.serial_vid, bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM);
}

TEST_F(AmlUartHarness, SerialImplAsyncGetInfoFromDriverService) {
  zx::result driver_connect_result =
      driver_test().Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());
  fdf::Arena arena('INFO');
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> device_client(
      std::move(driver_connect_result.value()), fdf::Dispatcher::GetCurrent()->get());
  device_client.buffer(arena)->GetInfo().Then(
      [quit = driver_test().runtime().QuitClosure()](
          fdf::WireUnownedResult<fuchsia_hardware_serialimpl::Device::GetInfo>& result) {
        ASSERT_EQ(ZX_OK, result.status());
        ASSERT_TRUE(result.value().is_ok());

        auto res = result.value().value();
        ASSERT_EQ(res->info.serial_class, fuchsia_hardware_serial::Class::kBluetoothHci);
        ASSERT_EQ(res->info.serial_pid,
                  bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458);
        ASSERT_EQ(res->info.serial_vid,
                  bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM);
        quit();
      });
  driver_test().runtime().Run();
}

TEST_F(AmlUartHarness, SerialImplAsyncConfig) {
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  {
    auto result = client.buffer(arena)->Enable(false);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  }

  driver_test().RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().Control().tx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().rx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().inv_cts(), 0u);
  });

  static constexpr uint32_t serial_test_config = fuchsia_hardware_serialimpl::kSerialDataBits6 |
                                                 fuchsia_hardware_serialimpl::kSerialStopBits2 |
                                                 fuchsia_hardware_serialimpl::kSerialParityEven |
                                                 fuchsia_hardware_serialimpl::kSerialFlowCtrlCtsRts;
  {
    auto result = client.buffer(arena)->Config(20, serial_test_config);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  }

  driver_test().RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
    ASSERT_EQ(env.device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
    ASSERT_EQ(env.device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
    ASSERT_TRUE(env.device_state().FlowControl());
  });

  {
    auto result =
        client.buffer(arena)->Config(40, fuchsia_hardware_serialimpl::kSerialSetBaudRateOnly);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  }

  driver_test().RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
    ASSERT_EQ(env.device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
    ASSERT_EQ(env.device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
    ASSERT_TRUE(env.device_state().FlowControl());
  });

  {
    auto result = client.buffer(arena)->Config(0, serial_test_config);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  }

  {
    auto result = client.buffer(arena)->Config(UINT32_MAX, serial_test_config);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  }

  {
    auto result = client.buffer(arena)->Config(1, serial_test_config);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  }

  driver_test().RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
    ASSERT_EQ(env.device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
    ASSERT_EQ(env.device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
    ASSERT_TRUE(env.device_state().FlowControl());
  });

  {
    auto result =
        client.buffer(arena)->Config(40, fuchsia_hardware_serialimpl::kSerialSetBaudRateOnly);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  }

  driver_test().RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
    ASSERT_EQ(env.device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
    ASSERT_EQ(env.device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
    ASSERT_TRUE(env.device_state().FlowControl());
  });
}

TEST_F(AmlUartHarness, SerialImplAsyncEnable) {
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  {
    auto result = client.buffer(arena)->Enable(false);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  }

  driver_test().RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().Control().tx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().rx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().inv_cts(), 0u);
  });

  {
    auto result = client.buffer(arena)->Enable(true);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  }

  driver_test().RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().Control().tx_enable(), 1u);
    ASSERT_EQ(env.device_state().Control().rx_enable(), 1u);
    ASSERT_EQ(env.device_state().Control().inv_cts(), 0u);
    ASSERT_TRUE(env.device_state().PortResetRX());
    ASSERT_TRUE(env.device_state().PortResetTX());
    ASSERT_FALSE(env.device_state().Control().rst_rx());
    ASSERT_FALSE(env.device_state().Control().rst_tx());
    ASSERT_TRUE(env.device_state().Control().tx_interrupt_enable());
    ASSERT_TRUE(env.device_state().Control().rx_interrupt_enable());
  });
}

TEST_F(AmlUartHarness, SerialImplReadDriverService) {
  uint8_t data[kDataLen];
  for (size_t i = 0; i < kDataLen; i++) {
    data[i] = static_cast<uint8_t>(i);
  }

  zx::result driver_connect_result =
      driver_test().Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());
  fdf::Arena arena('READ');
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> device_client(
      std::move(driver_connect_result.value()), fdf::Dispatcher::GetCurrent()->get());

  device_client.buffer(arena)->Enable(true).Then(
      [quit = driver_test().runtime().QuitClosure()](auto& res) { quit(); });
  driver_test().runtime().Run();
  driver_test().runtime().ResetQuit();

  device_client.buffer(arena)->Read().Then(
      [quit = driver_test().runtime().QuitClosure(),
       data](fdf::WireUnownedResult<fuchsia_hardware_serialimpl::Device::Read>& result) {
        ASSERT_EQ(ZX_OK, result.status());
        ASSERT_TRUE(result.value().is_ok());

        auto res = result.value().value();
        EXPECT_EQ(res->data.count(), kDataLen);
        EXPECT_EQ(memcmp(data, res->data.data(), res->data.count()), 0);
        quit();
      });

  driver_test().RunInEnvironmentTypeContext(
      [&data](Environment& env) { env.device_state().Inject(data, kDataLen); });
  driver_test().runtime().Run();
}

TEST_F(AmlUartHarness, SerialImplWriteDriverService) {
  uint8_t data[kDataLen];
  for (size_t i = 0; i < kDataLen; i++) {
    data[i] = static_cast<uint8_t>(i);
  }

  zx::result driver_connect_result =
      driver_test().Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());
  fdf::Arena arena('WRIT');
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> device_client(
      std::move(driver_connect_result.value()), fdf::Dispatcher::GetCurrent()->get());

  device_client.buffer(arena)->Enable(true).Then(
      [quit = driver_test().runtime().QuitClosure()](auto& res) { quit(); });
  driver_test().runtime().Run();
  driver_test().runtime().ResetQuit();

  device_client.buffer(arena)
      ->Write(fidl::VectorView<uint8_t>::FromExternal(data, kDataLen))
      .Then([quit = driver_test().runtime().QuitClosure()](
                fdf::WireUnownedResult<fuchsia_hardware_serialimpl::Device::Write>& result) {
        ASSERT_EQ(ZX_OK, result.status());
        ASSERT_TRUE(result.value().is_ok());
        quit();
      });
  driver_test().runtime().Run();

  driver_test().RunInEnvironmentTypeContext([&data](Environment& env) {
    auto buf = env.device_state().TxBuf();
    ASSERT_EQ(buf.size(), kDataLen);
    ASSERT_EQ(memcmp(buf.data(), data, buf.size()), 0);
  });
}

TEST_F(AmlUartAsyncHarness, SerialImplAsyncWriteDoubleCallback) {
  // NOTE: we don't start the IRQ thread.  The Handle*RaceForTest() enable.
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  std::vector<uint8_t> expected_data;
  for (size_t i = 0; i < kDataLen; i++) {
    expected_data.push_back(static_cast<uint8_t>(i));
  }

  bool write_complete = false;
  client.buffer(arena)
      ->Write(fidl::VectorView<uint8_t>::FromExternal(expected_data.data(), kDataLen))
      .ThenExactlyOnce([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        write_complete = true;
      });
  driver_test().runtime().RunUntilIdle();
  Device().HandleTXRaceForTest();
  driver_test().runtime().RunUntil([&]() { return write_complete; });

  driver_test().RunInEnvironmentTypeContext(
      [expected_data = std::move(expected_data)](Environment& env) {
        EXPECT_EQ(expected_data, env.device_state().TxBuf());
      });
}

TEST_F(AmlUartAsyncHarness, SerialImplAsyncReadDoubleCallback) {
  // NOTE: we don't start the IRQ thread.  The Handle*RaceForTest() enable.
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  std::vector<uint8_t> expected_data;
  for (size_t i = 0; i < kDataLen; i++) {
    expected_data.push_back(static_cast<uint8_t>(i));
  }

  client.buffer(arena)->Read().ThenExactlyOnce([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    const std::vector actual_data(result->value()->data.cbegin(), result->value()->data.cend());
    EXPECT_EQ(expected_data, actual_data);
    driver_test().runtime().Quit();
  });
  driver_test().runtime().RunUntilIdle();

  driver_test().RunInEnvironmentTypeContext(
      [&](Environment& env) { env.device_state().Inject(expected_data.data(), kDataLen); });
  Device().HandleRXRaceForTest();
  driver_test().runtime().Run();
}

TEST_F(AmlUartHarnessWithPower, AcquireWakeLeaseWithRead) {
  FakeTimer fake_timer;

  // Inject the fake timer handle to driver. FakeTimer::timer_handle_ will replace the handle
  // under the driver timer.
  driver_test().RunInDriverContext([&](serial::AmlUartV2& driver) {
    driver.aml_uart_for_testing().InjectTimerForTest(FakeTimer::timer_handle_);
  });

  uint8_t data[kDataLen];
  for (size_t i = 0; i < kDataLen; i++) {
    data[i] = static_cast<uint8_t>(i);
  }

  zx::result driver_connect_result =
      driver_test().Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());
  fdf::Arena arena('READ');
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> device_client(
      std::move(driver_connect_result.value()), fdf::Dispatcher::GetCurrent()->get());

  device_client.buffer(arena)->Enable(true).Then(
      [quit = driver_test().runtime().QuitClosure()](auto& res) { quit(); });
  driver_test().runtime().Run();
  driver_test().runtime().ResetQuit();

  // Verify that no lease has been acquired. Trigger an interrupt.
  driver_test().RunInEnvironmentTypeContext([&data](Environment& env) {
    ASSERT_FALSE(env.sag().HasActiveWakeLease());
    env.device_state().Inject(data, kDataLen);
  });

  // Verify that the timer deadline has been set by the driver. Save the deadline for later
  // comparison.
  driver_test().runtime().RunUntil([&]() { return FakeTimer::current_deadline_ != 0; });
  zx_time_t last_deadline = FakeTimer::current_deadline_;

  driver_test().RunInEnvironmentTypeContext([&data](Environment& env) {
    ASSERT_TRUE(env.sag().HasActiveWakeLease());
    env.device_state().Inject(data, kDataLen);
  });

  // Verify that the timer was set again by driver, also the wake lease was not
  // dropped.
  driver_test().runtime().RunUntil([&]() {
    return FakeTimer::current_deadline_ != 0 && FakeTimer::current_deadline_ != last_deadline;
  });

  driver_test().RunInEnvironmentTypeContext(
      [](Environment& env) { ASSERT_TRUE(env.sag().HasActiveWakeLease()); });

  // Fire the timer and verify that the wake lease has been dropped.
  fake_timer.FireTimer();

  // Verify that the deadline has been cleared.
  EXPECT_EQ(FakeTimer::current_deadline_, 0);

  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInEnvironmentTypeContext<bool>(
               [](Environment& env) { return env.sag().HasActiveWakeLease(); }) == false;
  });

  driver_test().RunInEnvironmentTypeContext(
      [&data](Environment& env) { env.device_state().Inject(data, kDataLen); });

  // The driver is able to set the timer and acquire lease again.
  driver_test().runtime().RunUntil([&]() { return FakeTimer::current_deadline_ != 0; });
  driver_test().RunInEnvironmentTypeContext(
      [](Environment& env) { ASSERT_TRUE(env.sag().HasActiveWakeLease()); });
}

}  // namespace
