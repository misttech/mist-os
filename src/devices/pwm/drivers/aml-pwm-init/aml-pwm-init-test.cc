// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-pwm-init.h"

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.pwm/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>

#include <list>

#include <fbl/alloc_checker.h>
#include <zxtest/zxtest.h>

#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"

bool operator==(const fuchsia_hardware_pwm::wire::PwmConfig& lhs,
                const fuchsia_hardware_pwm::wire::PwmConfig& rhs) {
  return (lhs.polarity == rhs.polarity) && (lhs.period_ns == rhs.period_ns) &&
         (lhs.duty_cycle == rhs.duty_cycle) &&
         (lhs.mode_config.count() == rhs.mode_config.count()) &&
         (reinterpret_cast<aml_pwm::mode_config*>(lhs.mode_config.data())->mode ==
          reinterpret_cast<aml_pwm::mode_config*>(rhs.mode_config.data())->mode);
}

namespace pwm_init {

class MockPwmServer final : public fidl::testing::WireTestBase<fuchsia_hardware_pwm::Pwm> {
 public:
  void SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) override {
    ASSERT_GT(expect_configs_.size(), 0);
    auto expect_config = expect_configs_.front();

    ASSERT_EQ(request->config, expect_config);

    expect_configs_.pop_front();
    mode_config_buffers_.pop_front();
    completer.ReplySuccess();
  }

  void Enable(EnableCompleter::Sync& completer) override {
    ASSERT_TRUE(expect_enable_);
    expect_enable_ = false;
    completer.ReplySuccess();
  }

  void ExpectSetConfig(fuchsia_hardware_pwm::wire::PwmConfig config) {
    std::unique_ptr<uint8_t[]> mode_config =
        std::make_unique<uint8_t[]>(config.mode_config.count());
    memcpy(mode_config.get(), config.mode_config.data(), config.mode_config.count());
    auto copy = config;
    copy.mode_config =
        fidl::VectorView<uint8_t>::FromExternal(mode_config.get(), config.mode_config.count());
    expect_configs_.push_back(std::move(copy));
    mode_config_buffers_.push_back(std::move(mode_config));
  }

  void ExpectEnable() { expect_enable_ = true; }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> BindServer() {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_pwm::Pwm>();
    EXPECT_TRUE(endpoints.is_ok());
    fidl::BindServer(async_get_default_dispatcher(), std::move(endpoints->server), this);
    return fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm>(std::move(endpoints->client));
  }

  void VerifyAndClear() {
    ASSERT_EQ(expect_configs_.size(), 0);
    ASSERT_EQ(mode_config_buffers_.size(), 0);
    ASSERT_FALSE(expect_enable_);
  }

 private:
  std::list<fuchsia_hardware_pwm::wire::PwmConfig> expect_configs_;
  std::list<std::unique_ptr<uint8_t[]>> mode_config_buffers_;
  bool expect_enable_ = false;
};

class FakePwmInitDevice : public PwmInitDevice {
 public:
  static std::unique_ptr<FakePwmInitDevice> Create(
      fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm,
      fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> wifi_gpio,
      fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> bt_gpio) {
    fbl::AllocChecker ac;
    auto device = fbl::make_unique_checked<FakePwmInitDevice>(
        &ac, std::move(pwm), std::move(wifi_gpio), std::move(bt_gpio));
    if (!ac.check()) {
      zxlogf(ERROR, "%s: device object alloc failed", __func__);
      return nullptr;
    }
    EXPECT_OK(device->Init());

    return device;
  }

  explicit FakePwmInitDevice(fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm,
                             fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> wifi_gpio,
                             fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> bt_gpio)
      : PwmInitDevice(nullptr, std::move(pwm), std::move(wifi_gpio), std::move(bt_gpio)) {}
};

TEST(PwmInitDeviceTest, InitTest) {
  async::Loop fidl_loop{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<MockPwmServer> pwm{fidl_loop.dispatcher(), std::in_place};
  async_patterns::TestDispatcherBound<fake_gpio::FakeGpio> wifi_gpio{fidl_loop.dispatcher(),
                                                                     std::in_place};
  async_patterns::TestDispatcherBound<fake_gpio::FakeGpio> bt_gpio{fidl_loop.dispatcher(),
                                                                   std::in_place};
  EXPECT_OK(fidl_loop.StartThread("fidl-servers"));

  auto pwm_client = pwm.SyncCall(&MockPwmServer::BindServer);
  auto wifi_gpio_client = wifi_gpio.SyncCall(&fake_gpio::FakeGpio::Connect);
  auto bt_gpio_client = bt_gpio.SyncCall(&fake_gpio::FakeGpio::Connect);

  pwm.SyncCall(&MockPwmServer::ExpectEnable);
  aml_pwm::mode_config two_timer = {
      .mode = aml_pwm::Mode::kTwoTimer,
      .two_timer =
          {
              .period_ns2 = 30052,
              .duty_cycle2 = 50.0,
              .timer1 = 0x0a,
              .timer2 = 0x0a,
          },
  };
  fuchsia_hardware_pwm::wire::PwmConfig init_cfg = {
      .polarity = false,
      .period_ns = 30053,
      .duty_cycle = static_cast<float>(49.931787176),
      .mode_config = fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&two_timer),
                                                             sizeof(two_timer))};
  pwm.SyncCall(&MockPwmServer::ExpectSetConfig, init_cfg);

  std::unique_ptr<FakePwmInitDevice> dev = FakePwmInitDevice::Create(
      std::move(pwm_client), std::move(wifi_gpio_client), std::move(bt_gpio_client));
  ASSERT_NOT_NULL(dev);

  ASSERT_EQ(1, wifi_gpio.SyncCall(&fake_gpio::FakeGpio::GetAltFunction));
  std::vector states = bt_gpio.SyncCall(&fake_gpio::FakeGpio::GetStateLog);
  ASSERT_EQ(2, states.size());
  ASSERT_EQ(fake_gpio::WriteSubState{.value = 0}, states[0].sub_state);
  ASSERT_EQ(fake_gpio::WriteSubState{.value = 1}, states[1].sub_state);

  pwm.SyncCall(&MockPwmServer::VerifyAndClear);
}

}  // namespace pwm_init
