// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/backlight/drivers/vim3-pwm-backlight/vim3-pwm-backlight.h"

#include <fidl/fuchsia.hardware.backlight/cpp/wire.h>
#include <fidl/fuchsia.hardware.pwm/cpp/wire_test_base.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/inspect/testing/cpp/inspect.h>
#include <zircon/errors.h>

#include <fbl/auto_lock.h>
#include <gtest/gtest.h>
#include <soc/aml-common/aml-pwm-regs.h>

#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"
#include "src/lib/testing/predicates/status.h"

bool operator==(const fuchsia_hardware_pwm::wire::PwmConfig& lhs,
                const fuchsia_hardware_pwm::wire::PwmConfig& rhs) {
  return (lhs.polarity == rhs.polarity) && (lhs.period_ns == rhs.period_ns) &&
         (lhs.duty_cycle == rhs.duty_cycle) && (lhs.mode_config.size() == rhs.mode_config.size()) &&
         (reinterpret_cast<aml_pwm::mode_config*>(lhs.mode_config.data())->mode ==
          reinterpret_cast<aml_pwm::mode_config*>(rhs.mode_config.data())->mode);
}

namespace vim3_pwm_backlight {

namespace {

class TestVim3PwmBacklight : public Vim3PwmBacklight {
 public:
  TestVim3PwmBacklight(fdf::DriverStartArgs start_args,
                       fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : Vim3PwmBacklight(std::move(start_args), std::move(driver_dispatcher)) {}

  static DriverRegistration GetDriverRegistration() {
    return FUCHSIA_DRIVER_REGISTRATION_V1(
        fdf_internal::DriverServer<TestVim3PwmBacklight>::initialize,
        fdf_internal::DriverServer<TestVim3PwmBacklight>::destroy);
  }

  inspect::ComponentInspector& inspector() { return Vim3PwmBacklight::inspector(); }
};

class MockPwmServer final : public fidl::testing::WireTestBase<fuchsia_hardware_pwm::Pwm> {
 public:
  void SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) override {
    if (set_config_override_callback_ != nullptr) {
      zx_status_t status = set_config_override_callback_(request->config);
      if (status == ZX_OK) {
        completer.ReplySuccess();
      } else {
        completer.ReplyError(status);
      }
      return;
    }

    calls_["SetConfig"] = true;

    EXPECT_FALSE(request->config.mode_config.empty());
    EXPECT_EQ(request->config.mode_config.size(), sizeof(mode_config_));
    if (request->config.mode_config.data() == nullptr ||
        request->config.mode_config.size() != sizeof(mode_config_)) {
      return completer.ReplyError(ZX_ERR_INVALID_ARGS);
    }

    memcpy(&mode_config_, request->config.mode_config.data(), sizeof(mode_config_));
    recent_config_ = request->config;
    recent_config_.mode_config = fidl::VectorView<uint8_t>::FromExternal(
        reinterpret_cast<uint8_t*>(&mode_config_), sizeof(mode_config_));

    completer.ReplySuccess();
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  const fuchsia_hardware_pwm::wire::PwmConfig& GetMostRecentConfig() const {
    return recent_config_;
  }
  const aml_pwm::mode_config& GetMostRecentModeConfig() const { return mode_config_; }

  bool IsCalled(const std::string& op) const { return calls_.find(op) != calls_.end(); }
  void ClearCallMap() { calls_.clear(); }

  void SetSetConfigOverrideCallback(
      fit::function<zx_status_t(const fuchsia_hardware_pwm::wire::PwmConfig)> callback) {
    set_config_override_callback_ = std::move(callback);
  }

  fuchsia_hardware_pwm::Service::InstanceHandler CreateInstanceHandler(
      async_dispatcher_t* dispatcher) {
    Handler pwm_handler = [this, dispatcher = dispatcher](
                              fidl::ServerEnd<fuchsia_hardware_pwm::Pwm> request) {
      this->bindings_.AddBinding(dispatcher, std::move(request), this, fidl::kIgnoreBindingClosure);
    };

    return fuchsia_hardware_pwm::Service::InstanceHandler({.pwm = std::move(pwm_handler)});
  }

 private:
  fuchsia_hardware_pwm::wire::PwmConfig recent_config_ = {};
  aml_pwm::mode_config mode_config_ = {};

  fit::function<zx_status_t(fuchsia_hardware_pwm::wire::PwmConfig)> set_config_override_callback_ =
      nullptr;

  std::unordered_map<std::string, bool> calls_;

  fidl::ServerBindingGroup<fuchsia_hardware_pwm::Pwm> bindings_;
};

class Vim3PwmBacklightTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();

    {
      zx::result result = to_driver_vfs.AddService<fuchsia_hardware_pwm::Service>(
          pwm_.CreateInstanceHandler(dispatcher), "pwm");
      if (result.is_error()) {
        return result.take_error();
      }
    }

    {
      zx::result result = to_driver_vfs.AddService<fuchsia_hardware_gpio::Service>(
          gpio_.CreateInstanceHandler(), "gpio-lcd-backlight-enable");
      if (result.is_error()) {
        return result.take_error();
      }
    }

    return zx::ok();
  }

  MockPwmServer& pwm() { return pwm_; }
  fake_gpio::FakeGpio& gpio() { return gpio_; }

 private:
  MockPwmServer pwm_;
  fake_gpio::FakeGpio gpio_;
};

class FixtureConfig final {
 public:
  using DriverType = TestVim3PwmBacklight;
  using EnvironmentType = Vim3PwmBacklightTestEnvironment;
};

class Vim3PwmBacklightTest : public ::testing::Test {
 public:
  void SetUp() override {
    ASSERT_OK(driver_test_.StartDriver());
    zx::result client = driver_test_.Connect<fuchsia_hardware_backlight::Service::Backlight>();
    ASSERT_OK(client);
    client_.Bind(std::move(client.value()));
  }

  void WithInspectRoot(fit::callback<void(const inspect::Hierarchy*)> task) {
    driver_test_.RunInDriverContext([task = std::move(task)](auto& driver) mutable {
      fpromise::result hierarchy_result =
          inspect::ReadFromVmo(driver.inspector().inspector().DuplicateVmo());
      ASSERT_TRUE(hierarchy_result.is_ok());
      const auto* hierarchy = hierarchy_result.value().GetByPath({"vim3-pwm-backlight"});
      ASSERT_NE(hierarchy, nullptr);
      task(hierarchy);
    });
  }

  void TearDown() override { ASSERT_OK(driver_test_.StopDriver()); }

 protected:
  const fidl::WireSyncClient<fuchsia_hardware_backlight::Device>& client() const { return client_; }
  fdf_testing::BackgroundDriverTest<FixtureConfig>& driver_test() { return driver_test_; }

 private:
  fidl::WireSyncClient<fuchsia_hardware_backlight::Device> client_;
  fdf_testing::BackgroundDriverTest<FixtureConfig> driver_test_;
};

TEST_F(Vim3PwmBacklightTest, InitialState) {
  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    auto& pwm = env.pwm();
    auto& gpio = env.gpio();

    EXPECT_TRUE(pwm.IsCalled(std::string("SetConfig")));
    EXPECT_EQ(1u, gpio.GetStateLog().size());
    EXPECT_EQ(1, gpio.GetWriteValue());

    EXPECT_EQ(pwm.GetMostRecentConfig().duty_cycle, 100.0f);
    EXPECT_EQ(pwm.GetMostRecentModeConfig().mode, aml_pwm::Mode::kOn);
  });

  fidl::WireResult result_get = client()->GetStateNormalized();
  EXPECT_OK(result_get.status());
  EXPECT_TRUE(result_get.value().is_ok());

  // The stored state doesn't change.
  EXPECT_EQ(result_get.value()->state.backlight_on, true);
  EXPECT_EQ(result_get.value()->state.brightness, 1.0);
}

TEST_F(Vim3PwmBacklightTest, SetStateNormalizedTurnOff) {
  driver_test().RunInEnvironmentTypeContext([](auto& env) { env.pwm().ClearCallMap(); });

  fidl::WireResult result =
      client()->SetStateNormalized({.backlight_on = false, .brightness = 0.5});
  EXPECT_OK(result.status());
  EXPECT_TRUE(result.value().is_ok());

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    auto& pwm = env.pwm();
    auto& gpio = env.gpio();

    EXPECT_TRUE(pwm.IsCalled(std::string("SetConfig")));
    EXPECT_EQ(pwm.GetMostRecentModeConfig().mode, aml_pwm::Mode::kOff);
    EXPECT_EQ(2u, gpio.GetStateLog().size());
    EXPECT_EQ(0, gpio.GetWriteValue());
  });

  fidl::WireResult result_get = client()->GetStateNormalized();
  EXPECT_OK(result_get.status());
  EXPECT_TRUE(result_get.value().is_ok());

  EXPECT_EQ(result_get.value()->state.backlight_on, false);
  EXPECT_EQ(result_get.value()->state.brightness, 0.5);
}

TEST_F(Vim3PwmBacklightTest, SetStateNormalizedTurnOn) {
  driver_test().RunInEnvironmentTypeContext([](auto& env) { env.pwm().ClearCallMap(); });

  fidl::WireResult result = client()->SetStateNormalized({.backlight_on = true, .brightness = 0.5});
  EXPECT_OK(result.status());
  EXPECT_TRUE(result.value().is_ok());

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    auto& pwm = env.pwm();
    auto& gpio = env.gpio();

    EXPECT_TRUE(pwm.IsCalled(std::string("SetConfig")));
    EXPECT_EQ(pwm.GetMostRecentModeConfig().mode, aml_pwm::Mode::kOn);
    EXPECT_EQ(pwm.GetMostRecentConfig().duty_cycle, 50.0f);
    EXPECT_EQ(pwm.GetMostRecentConfig().period_ns, 5'555'555u);
    EXPECT_EQ(pwm.GetMostRecentConfig().polarity, false);
    EXPECT_EQ(2u, gpio.GetStateLog().size());
    EXPECT_EQ(1, gpio.GetWriteValue());
  });

  fidl::WireResult result_get = client()->GetStateNormalized();
  EXPECT_OK(result_get.status());
  EXPECT_TRUE(result_get.value().is_ok());

  EXPECT_EQ(result_get.value()->state.backlight_on, true);
  EXPECT_EQ(result_get.value()->state.brightness, 0.5);
}

TEST_F(Vim3PwmBacklightTest, SetStateNormalizedInspect) {
  fidl::WireResult result_success1 =
      client()->SetStateNormalized({.backlight_on = true, .brightness = 0.75});
  EXPECT_OK(result_success1.status());
  EXPECT_TRUE(result_success1.value().is_ok());

  // Inspected value should match the request on command success.
  WithInspectRoot([](const auto* root) {
    EXPECT_THAT(root->node(),
                inspect::testing::PropertyList(testing::AllOf(
                    testing::Contains(inspect::testing::BoolIs("power", true)),
                    testing::Contains(inspect::testing::DoubleIs("brightness", 0.75)))));
  });

  fidl::WireResult result_success2 =
      client()->SetStateNormalized({.backlight_on = false, .brightness = 0.5});
  EXPECT_OK(result_success2.status());
  EXPECT_TRUE(result_success2.value().is_ok());

  // Inspected value should change on command success.
  // A new inspect VMO is needed; the children VMOs produced by inspect are
  // snapshots of the previous state.
  WithInspectRoot([](const auto* root) {
    EXPECT_THAT(root->node(),
                inspect::testing::PropertyList(testing::AllOf(
                    testing::Contains(inspect::testing::BoolIs("power", false)),
                    testing::Contains(inspect::testing::DoubleIs("brightness", 0.5)))));
  });

  fidl::WireResult result_failure =
      client()->SetStateNormalized({.backlight_on = true, .brightness = 1.75});
  EXPECT_OK(result_failure.status());
  EXPECT_TRUE(result_failure.value().is_error());

  // Inspected value shouldn't change on command failure.
  WithInspectRoot([](const auto* root) {
    EXPECT_THAT(root->node(),
                inspect::testing::PropertyList(testing::AllOf(
                    testing::Contains(inspect::testing::BoolIs("power", false)),
                    testing::Contains(inspect::testing::DoubleIs("brightness", 0.5)))));
  });
}

TEST_F(Vim3PwmBacklightTest, SetStateNormalizedNoDuplicateConfigs) {
  driver_test().RunInEnvironmentTypeContext([](auto& env) { env.pwm().ClearCallMap(); });

  fidl::WireResult result = client()->SetStateNormalized({.backlight_on = true, .brightness = 0.5});
  EXPECT_OK(result.status());
  EXPECT_TRUE(result.value().is_ok());

  driver_test().RunInEnvironmentTypeContext([](auto& env) { env.pwm().ClearCallMap(); });

  fidl::WireResult result_same_call =
      client()->SetStateNormalized({.backlight_on = true, .brightness = 0.5});
  EXPECT_OK(result_same_call.status());
  EXPECT_TRUE(result_same_call.value().is_ok());

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    EXPECT_FALSE(env.pwm().IsCalled(std::string("SetConfig")));
    std::vector gpio_states = env.gpio().GetStateLog();
    ASSERT_EQ(2u, gpio_states.size());
    ASSERT_EQ(fake_gpio::WriteSubState{.value = 1}, gpio_states.back().sub_state);
  });
}

TEST_F(Vim3PwmBacklightTest, SetStateNormalizedRejectInvalidValue) {
  driver_test().RunInEnvironmentTypeContext([](auto& env) { env.pwm().ClearCallMap(); });

  fidl::WireResult result_too_large_brightness =
      client()->SetStateNormalized({.backlight_on = true, .brightness = 1.02});
  EXPECT_OK(result_too_large_brightness.status());
  EXPECT_EQ(result_too_large_brightness.value().error_value(), ZX_ERR_INVALID_ARGS);

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    auto& pwm = env.pwm();
    EXPECT_FALSE(pwm.IsCalled(std::string("SetConfig")));
    pwm.ClearCallMap();
  });

  fidl::WireResult result_negative_brightness =
      client()->SetStateNormalized({.backlight_on = true, .brightness = -0.05});
  EXPECT_OK(result_negative_brightness.status());
  EXPECT_EQ(result_negative_brightness.value().error_value(), ZX_ERR_INVALID_ARGS);

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    EXPECT_FALSE(env.pwm().IsCalled(std::string("SetConfig")));
    std::vector gpio_states = env.gpio().GetStateLog();
    ASSERT_EQ(1u, gpio_states.size());
    ASSERT_EQ(fake_gpio::WriteSubState{.value = 1}, gpio_states[0].sub_state);
  });
}

TEST_F(Vim3PwmBacklightTest, SetStateNormalizedBailoutGpioConfig) {
  fidl::WireResult result = client()->SetStateNormalized({.backlight_on = true, .brightness = 0.5});
  EXPECT_OK(result.status());
  EXPECT_TRUE(result.value().is_ok());

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    env.pwm().ClearCallMap();
    env.gpio().SetSetBufferModeCallback([](fake_gpio::FakeGpio& gpio) { return ZX_ERR_INTERNAL; });
  });

  fidl::WireResult result_fail =
      client()->SetStateNormalized({.backlight_on = false, .brightness = 0.5});
  EXPECT_OK(result_fail.status());
  EXPECT_TRUE(result_fail.value().is_error());
  EXPECT_EQ(result_fail.value().error_value(), ZX_ERR_INTERNAL);

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    std::vector gpio_states = env.gpio().GetStateLog();
    ASSERT_EQ(4u, gpio_states.size());
    ASSERT_EQ(fake_gpio::WriteSubState{.value = 0}, gpio_states[2].sub_state);
    ASSERT_EQ(fake_gpio::WriteSubState{.value = 1}, gpio_states[3].sub_state);
  });

  fidl::WireResult result_get = client()->GetStateNormalized();
  EXPECT_OK(result_get.status());
  EXPECT_TRUE(result_get.value().is_ok());

  // The stored state doesn't change.
  EXPECT_EQ(result_get.value()->state.backlight_on, true);
  EXPECT_EQ(result_get.value()->state.brightness, 0.5);
}

TEST_F(Vim3PwmBacklightTest, SetStateNormalizedBailoutPwmConfig) {
  fidl::WireResult result = client()->SetStateNormalized({.backlight_on = true, .brightness = 0.5});
  EXPECT_OK(result.status());
  EXPECT_TRUE(result.value().is_ok());

  std::vector<fuchsia_hardware_pwm::wire::PwmConfig> pwm_configs_set;
  std::vector<aml_pwm::mode_config> mode_configs_set;
  driver_test().RunInEnvironmentTypeContext([&](auto& env) {
    auto& pwm = env.pwm();
    pwm.ClearCallMap();

    pwm.SetSetConfigOverrideCallback(
        [&pwm_configs_set, &mode_configs_set](fuchsia_hardware_pwm::wire::PwmConfig config) {
          pwm_configs_set.push_back(config);
          mode_configs_set.push_back(
              *reinterpret_cast<aml_pwm::mode_config*>(config.mode_config.data()));
          return ZX_ERR_INTERNAL;
        });
  });

  fidl::WireResult result_fail =
      client()->SetStateNormalized({.backlight_on = false, .brightness = 0.75});
  EXPECT_OK(result_fail.status());
  EXPECT_TRUE(result_fail.value().is_error());
  EXPECT_EQ(result_fail.value().error_value(), ZX_ERR_INTERNAL);

  EXPECT_EQ(pwm_configs_set.size(), 2u);
  EXPECT_EQ(pwm_configs_set[0].duty_cycle, 75.0f);
  EXPECT_EQ(pwm_configs_set[1].duty_cycle, 50.0f);

  EXPECT_EQ(mode_configs_set.size(), 2u);
  EXPECT_EQ(mode_configs_set[0].mode, aml_pwm::Mode::kOff);
  EXPECT_EQ(mode_configs_set[1].mode, aml_pwm::Mode::kOn);

  fidl::WireResult result_get = client()->GetStateNormalized();
  EXPECT_OK(result_get.status());
  EXPECT_TRUE(result_get.value().is_ok());

  // The stored state doesn't change.
  EXPECT_EQ(result_get.value()->state.backlight_on, true);
  EXPECT_EQ(result_get.value()->state.brightness, 0.5);
}

}  // namespace

}  // namespace vim3_pwm_backlight
