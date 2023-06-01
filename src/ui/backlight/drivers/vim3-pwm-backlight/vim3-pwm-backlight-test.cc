// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/backlight/drivers/vim3-pwm-backlight/vim3-pwm-backlight.h"

#include <fidl/fuchsia.hardware.backlight/cpp/wire.h>
#include <fidl/fuchsia.hardware.pwm/cpp/wire_test_base.h>
#include <fuchsia/hardware/gpio/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/async-testing/test_loop.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/reader.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/errors.h>

#include <fbl/auto_lock.h>
#include <sdk/lib/inspect/testing/cpp/zxtest/inspect.h>
#include <soc/aml-common/aml-pwm-regs.h>
#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

bool operator==(const fuchsia_hardware_pwm::wire::PwmConfig& lhs,
                const fuchsia_hardware_pwm::wire::PwmConfig& rhs) {
  return (lhs.polarity == rhs.polarity) && (lhs.period_ns == rhs.period_ns) &&
         (lhs.duty_cycle == rhs.duty_cycle) &&
         (lhs.mode_config.count() == rhs.mode_config.count()) &&
         (reinterpret_cast<aml_pwm::mode_config*>(lhs.mode_config.data())->mode ==
          reinterpret_cast<aml_pwm::mode_config*>(rhs.mode_config.data())->mode);
}

namespace vim3_pwm_backlight {

namespace {

class MockPwmServer final : public fidl::testing::WireTestBase<fuchsia_hardware_pwm::Pwm> {
 public:
  explicit MockPwmServer(async_dispatcher_t* dispatcher)
      : dispatcher_(dispatcher), outgoing_(dispatcher) {}

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

    EXPECT_TRUE(request->config.mode_config.data());
    EXPECT_EQ(request->config.mode_config.count(), sizeof(mode_config_));
    if (request->config.mode_config.data() == nullptr ||
        request->config.mode_config.count() != sizeof(mode_config_)) {
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

  fidl::ClientEnd<fuchsia_io::Directory> Connect() {
    auto device_handler = [this](fidl::ServerEnd<fuchsia_hardware_pwm::Pwm> request) {
      fidl::BindServer(dispatcher_, std::move(request), this);
    };
    fuchsia_hardware_pwm::Service::InstanceHandler handler({.pwm = std::move(device_handler)});

    auto service_result = outgoing_.AddService<fuchsia_hardware_pwm::Service>(std::move(handler));
    ZX_ASSERT(service_result.is_ok());

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(endpoints.is_ok());
    ZX_ASSERT(outgoing_.Serve(std::move(endpoints->server)).is_ok());

    return std::move(endpoints->client);
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

 private:
  async_dispatcher_t* dispatcher_;
  component::OutgoingDirectory outgoing_;

  fuchsia_hardware_pwm::wire::PwmConfig recent_config_ = {};
  aml_pwm::mode_config mode_config_ = {};

  fit::function<zx_status_t(fuchsia_hardware_pwm::wire::PwmConfig)> set_config_override_callback_ =
      nullptr;

  std::unordered_map<std::string, bool> calls_;
};

class MockGpio : public ddk::GpioProtocol<MockGpio> {
 public:
  MockGpio() : proto_{&gpio_protocol_ops_, this} {}

  zx_status_t Unsupported() {
    EXPECT_TRUE(false, "unexpected call");
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t GpioConfigIn(uint32_t) { return Unsupported(); }
  zx_status_t GpioSetAltFunction(uint64_t) { return Unsupported(); }
  zx_status_t GpioRead(uint8_t*) { return Unsupported(); }
  zx_status_t GpioGetInterrupt(uint32_t, zx::interrupt*) { return Unsupported(); }
  zx_status_t GpioReleaseInterrupt() { return Unsupported(); }
  zx_status_t GpioSetPolarity(gpio_polarity_t) { return Unsupported(); }
  zx_status_t GpioSetDriveStrength(uint64_t, uint64_t*) { return Unsupported(); }
  zx_status_t GpioGetDriveStrength(uint64_t*) { return Unsupported(); }

  zx_status_t GpioConfigOut(uint8_t initial_value) {
    calls_["ConfigOut"] = true;
    EXPECT_FALSE(write_configured_);
    EXPECT_EQ(initial_value, 1u);
    value_written_ = initial_value;
    write_configured_ = true;
    return ZX_OK;
  }
  zx_status_t GpioWrite(uint8_t value) {
    if (write_override_callback_ != nullptr) {
      return write_override_callback_(value);
    }

    calls_["Write"] = true;
    EXPECT_TRUE(write_configured_);
    value_written_ = value;
    return ZX_OK;
  }

  uint8_t GetMostRecentValueWritten() const { return value_written_; }
  const gpio_protocol_t* GetProto() const { return &proto_; }

  bool IsCalled(const std::string& op) const { return calls_.find(op) != calls_.end(); }
  void ClearCallMap() { calls_.clear(); }
  void SetWriteOverrideCallback(fit::function<zx_status_t(uint8_t)> callback) {
    write_override_callback_ = std::move(callback);
  }

 private:
  fit::function<zx_status_t(uint8_t)> write_override_callback_ = nullptr;
  std::unordered_map<std::string, bool> calls_;
  uint8_t value_written_;
  bool write_configured_ = false;

  const gpio_protocol_t proto_;
};

class Vim3PwmBacklightDeviceTest : public zxtest::Test, public inspect::InspectTestHelper {
 public:
  Vim3PwmBacklightDeviceTest()
      : fake_parent_(MockDevice::FakeRootParent()),
        pwm_loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        loop_(&kAsyncLoopConfigNeverAttachToThread) {}
  ~Vim3PwmBacklightDeviceTest() override = default;

  void SetUp() override {
    pwm_loop_.StartThread("pwm-thread");
    fake_parent_->AddFidlService(fuchsia_hardware_pwm::Service::Name,
                                 mock_pwm_.SyncCall(&MockPwmServer::Connect), "pwm");
    fake_parent_->AddProtocol(ZX_PROTOCOL_GPIO, mock_gpio_.GetProto()->ops, &mock_gpio_,
                              "gpio-lcd-backlight-enable");
    fbl::AllocChecker ac;
    dev_ = fbl::make_unique_checked<Vim3PwmBacklight>(&ac, fake_parent_.get());
    ASSERT_TRUE(ac.check());

    zx::result server = fidl::CreateEndpoints(&client_);
    ASSERT_OK(server);
    fidl::BindServer(loop_.dispatcher(), std::move(server.value()), dev_.get());

    loop_.StartThread("fidl-dispatcher-thread");
  }

  const inspect::Hierarchy* GetInspectRoot() {
    const zx::vmo inspect_vmo = dev_->InspectVmo();
    if (!inspect_vmo.is_valid()) {
      return nullptr;
    }

    inspector_.ReadInspect(inspect_vmo);
    return inspector_.hierarchy().GetByPath({"vim3-pwm-backlight"});
  }

  void TearDown() override {
    loop_.Shutdown();
    loop_.JoinThreads();

    if (dev_) {
      dev_->DdkAsyncRemove();
      EXPECT_OK(
          mock_ddk::ReleaseFlaggedDevices(fake_parent_.get()));  // Calls DdkRelease() on dev_.
      [[maybe_unused]] auto ptr = dev_.release();
    }
  }

 protected:
  const fidl::ClientEnd<fuchsia_hardware_backlight::Device>& client() const { return client_; }

  MockGpio mock_gpio_;
  std::unique_ptr<Vim3PwmBacklight> dev_;
  std::shared_ptr<MockDevice> fake_parent_;
  inspect::InspectTestHelper inspector_;
  async::Loop pwm_loop_;
  async_patterns::TestDispatcherBound<MockPwmServer> mock_pwm_{
      pwm_loop_.dispatcher(), std::in_place, async_patterns::PassDispatcher};

  async::Loop loop_;

 private:
  fidl::ClientEnd<fuchsia_hardware_backlight::Device> client_;
};

TEST_F(Vim3PwmBacklightDeviceTest, TestLifeCycle) {
  EXPECT_OK(dev_->DdkAdd("vim3-pwm-backlight"));
  EXPECT_EQ(fake_parent_->child_count(), 1);
  dev_->DdkAsyncRemove();
  EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(fake_parent_.get()));  // Calls DdkRelease() on dev_.
  [[maybe_unused]] auto ptr = dev_.release();
  EXPECT_EQ(fake_parent_->child_count(), 0);
}

TEST_F(Vim3PwmBacklightDeviceTest, InitialState) {
  EXPECT_OK(dev_->Bind());

  EXPECT_TRUE(mock_pwm_.SyncCall(&MockPwmServer::IsCalled, std::string("SetConfig")));
  EXPECT_TRUE(mock_gpio_.IsCalled("ConfigOut"));

  EXPECT_EQ(mock_pwm_.SyncCall(&MockPwmServer::GetMostRecentConfig).duty_cycle, 100.0f);
  EXPECT_EQ(mock_pwm_.SyncCall(&MockPwmServer::GetMostRecentModeConfig).mode, aml_pwm::Mode::kOn);
  EXPECT_EQ(mock_gpio_.GetMostRecentValueWritten(), 1u);

  fidl::WireResult result_get = fidl::WireCall(client())->GetStateNormalized();
  EXPECT_OK(result_get);
  EXPECT_TRUE(result_get.value().is_ok());

  // The stored state doesn't change.
  EXPECT_EQ(result_get.value()->state.backlight_on, true);
  EXPECT_EQ(result_get.value()->state.brightness, 1.0);
}

TEST_F(Vim3PwmBacklightDeviceTest, SetStateNormalizedTurnOff) {
  EXPECT_OK(dev_->Bind());
  mock_pwm_.SyncCall(&MockPwmServer::ClearCallMap);
  mock_gpio_.ClearCallMap();

  fidl::WireResult result =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = false, .brightness = 0.5});
  EXPECT_OK(result);
  EXPECT_TRUE(result.value().is_ok());

  EXPECT_TRUE(mock_pwm_.SyncCall(&MockPwmServer::IsCalled, std::string("SetConfig")));
  EXPECT_TRUE(mock_gpio_.IsCalled("Write"));

  EXPECT_EQ(mock_pwm_.SyncCall(&MockPwmServer::GetMostRecentModeConfig).mode, aml_pwm::Mode::kOff);
  EXPECT_EQ(mock_gpio_.GetMostRecentValueWritten(), 0u);

  fidl::WireResult result_get = fidl::WireCall(client())->GetStateNormalized();
  EXPECT_OK(result_get);
  EXPECT_TRUE(result_get.value().is_ok());

  EXPECT_EQ(result_get.value()->state.backlight_on, false);
  EXPECT_EQ(result_get.value()->state.brightness, 0.5);
}

TEST_F(Vim3PwmBacklightDeviceTest, SetStateNormalizedTurnOn) {
  EXPECT_OK(dev_->Bind());
  mock_pwm_.SyncCall(&MockPwmServer::ClearCallMap);
  mock_gpio_.ClearCallMap();

  fidl::WireResult result =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = true, .brightness = 0.5});
  EXPECT_OK(result);
  EXPECT_TRUE(result.value().is_ok());

  EXPECT_TRUE(mock_pwm_.SyncCall(&MockPwmServer::IsCalled, std::string("SetConfig")));
  EXPECT_TRUE(mock_gpio_.IsCalled("Write"));

  EXPECT_EQ(mock_pwm_.SyncCall(&MockPwmServer::GetMostRecentModeConfig).mode, aml_pwm::Mode::kOn);
  EXPECT_EQ(mock_pwm_.SyncCall(&MockPwmServer::GetMostRecentConfig).duty_cycle, 50.0f);
  EXPECT_EQ(mock_pwm_.SyncCall(&MockPwmServer::GetMostRecentConfig).period_ns, 5'555'555u);
  EXPECT_EQ(mock_pwm_.SyncCall(&MockPwmServer::GetMostRecentConfig).polarity, false);
  EXPECT_EQ(mock_gpio_.GetMostRecentValueWritten(), 1u);

  fidl::WireResult result_get = fidl::WireCall(client())->GetStateNormalized();
  EXPECT_OK(result_get);
  EXPECT_TRUE(result_get.value().is_ok());

  EXPECT_EQ(result_get.value()->state.backlight_on, true);
  EXPECT_EQ(result_get.value()->state.brightness, 0.5);
}

TEST_F(Vim3PwmBacklightDeviceTest, SetStateNormalizedInspect) {
  EXPECT_OK(dev_->Bind());

  fidl::WireResult result_success1 =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = true, .brightness = 0.75});
  EXPECT_OK(result_success1);
  EXPECT_TRUE(result_success1.value().is_ok());

  // Inspected value should match the request on command success.
  {
    const inspect::Hierarchy* root = GetInspectRoot();
    ASSERT_NOT_NULL(root);

    const auto* power = root->node().get_property<inspect::BoolPropertyValue>("power");
    ASSERT_NOT_NULL(power);
    EXPECT_EQ(power->value(), true);

    const auto* brightness = root->node().get_property<inspect::DoublePropertyValue>("brightness");
    ASSERT_NOT_NULL(brightness);
    EXPECT_EQ(brightness->value(), 0.75);
  }

  fidl::WireResult result_success2 =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = false, .brightness = 0.5});
  EXPECT_OK(result_success2);
  EXPECT_TRUE(result_success2.value().is_ok());

  // Inspected value should change on command success.
  // A new inspect VMO is needed; the children VMOs produced by inspect are
  // snapshots of the previous state.
  {
    const inspect::Hierarchy* root = GetInspectRoot();
    ASSERT_NOT_NULL(root);

    const auto* power = root->node().get_property<inspect::BoolPropertyValue>("power");
    ASSERT_NOT_NULL(power);
    EXPECT_EQ(power->value(), false);

    const auto* brightness = root->node().get_property<inspect::DoublePropertyValue>("brightness");
    ASSERT_NOT_NULL(brightness);
    EXPECT_EQ(brightness->value(), 0.5);
  }

  fidl::WireResult result_failure =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = true, .brightness = 1.75});
  EXPECT_OK(result_failure);
  EXPECT_TRUE(result_failure.value().is_error());

  // Inspected value shouldn't change on command failure.
  {
    const inspect::Hierarchy* root = GetInspectRoot();
    ASSERT_NOT_NULL(root);

    const auto* power = root->node().get_property<inspect::BoolPropertyValue>("power");
    ASSERT_NOT_NULL(power);
    EXPECT_EQ(power->value(), false);

    const auto* brightness = root->node().get_property<inspect::DoublePropertyValue>("brightness");
    ASSERT_NOT_NULL(brightness);
    EXPECT_EQ(brightness->value(), 0.5);
  }
}

TEST_F(Vim3PwmBacklightDeviceTest, SetStateNormalizedNoDuplicateConfigs) {
  EXPECT_OK(dev_->Bind());
  mock_pwm_.SyncCall(&MockPwmServer::ClearCallMap);
  mock_gpio_.ClearCallMap();

  fidl::WireResult result =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = true, .brightness = 0.5});
  EXPECT_OK(result);
  EXPECT_TRUE(result.value().is_ok());

  mock_pwm_.SyncCall(&MockPwmServer::ClearCallMap);
  mock_gpio_.ClearCallMap();

  fidl::WireResult result_same_call =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = true, .brightness = 0.5});
  EXPECT_OK(result_same_call);
  EXPECT_TRUE(result_same_call.value().is_ok());

  EXPECT_FALSE(mock_pwm_.SyncCall(&MockPwmServer::IsCalled, std::string("SetConfig")));
  EXPECT_FALSE(mock_gpio_.IsCalled("Write"));
}

TEST_F(Vim3PwmBacklightDeviceTest, SetStateNormalizedRejectInvalidValue) {
  EXPECT_OK(dev_->Bind());
  mock_pwm_.SyncCall(&MockPwmServer::ClearCallMap);
  mock_gpio_.ClearCallMap();

  fidl::WireResult result_too_large_brightness =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = true, .brightness = 1.02});
  EXPECT_OK(result_too_large_brightness);
  EXPECT_EQ(result_too_large_brightness.value().error_value(), ZX_ERR_INVALID_ARGS);

  EXPECT_FALSE(mock_pwm_.SyncCall(&MockPwmServer::IsCalled, std::string("SetConfig")));
  EXPECT_FALSE(mock_gpio_.IsCalled("Write"));

  mock_pwm_.SyncCall(&MockPwmServer::ClearCallMap);
  mock_gpio_.ClearCallMap();

  fidl::WireResult result_negative_brightness =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = true, .brightness = -0.05});
  EXPECT_OK(result_negative_brightness);
  EXPECT_EQ(result_negative_brightness.value().error_value(), ZX_ERR_INVALID_ARGS);

  EXPECT_FALSE(mock_pwm_.SyncCall(&MockPwmServer::IsCalled, std::string("SetConfig")));
  EXPECT_FALSE(mock_gpio_.IsCalled("Write"));
}

TEST_F(Vim3PwmBacklightDeviceTest, SetStateNormalizedBailoutGpioConfig) {
  EXPECT_OK(dev_->Bind());
  fidl::WireResult result =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = true, .brightness = 0.5});
  EXPECT_OK(result);
  EXPECT_TRUE(result.value().is_ok());

  mock_pwm_.SyncCall(&MockPwmServer::ClearCallMap);
  mock_gpio_.ClearCallMap();

  std::vector<uint8_t> gpio_values_written;
  mock_gpio_.SetWriteOverrideCallback([&gpio_values_written](uint8_t value) {
    gpio_values_written.push_back(value);
    return ZX_ERR_INTERNAL;
  });

  fidl::WireResult result_fail =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = false, .brightness = 0.5});
  EXPECT_OK(result_fail);
  EXPECT_TRUE(result_fail.value().is_error());
  EXPECT_EQ(result_fail.value().error_value(), ZX_ERR_INTERNAL);

  EXPECT_EQ(gpio_values_written.size(), 2u);
  EXPECT_EQ(gpio_values_written[0], 0u);
  EXPECT_EQ(gpio_values_written[1], 1u);

  fidl::WireResult result_get = fidl::WireCall(client())->GetStateNormalized();
  EXPECT_OK(result_get);
  EXPECT_TRUE(result_get.value().is_ok());

  // The stored state doesn't change.
  EXPECT_EQ(result_get.value()->state.backlight_on, true);
  EXPECT_EQ(result_get.value()->state.brightness, 0.5);
}

TEST_F(Vim3PwmBacklightDeviceTest, SetStateNormalizedBailoutPwmConfig) {
  EXPECT_OK(dev_->Bind());
  fidl::WireResult result =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = true, .brightness = 0.5});
  EXPECT_OK(result);
  EXPECT_TRUE(result.value().is_ok());

  mock_pwm_.SyncCall(&MockPwmServer::ClearCallMap);
  mock_gpio_.ClearCallMap();

  std::vector<fuchsia_hardware_pwm::wire::PwmConfig> pwm_configs_set;
  std::vector<aml_pwm::mode_config> mode_configs_set;
  mock_pwm_.SyncCall(
      &MockPwmServer::SetSetConfigOverrideCallback,
      [&pwm_configs_set, &mode_configs_set](fuchsia_hardware_pwm::wire::PwmConfig config) {
        pwm_configs_set.push_back(config);
        mode_configs_set.push_back(
            *reinterpret_cast<aml_pwm::mode_config*>(config.mode_config.data()));
        return ZX_ERR_INTERNAL;
      });

  fidl::WireResult result_fail =
      fidl::WireCall(client())->SetStateNormalized({.backlight_on = false, .brightness = 0.75});
  EXPECT_OK(result_fail);
  EXPECT_TRUE(result_fail.value().is_error());
  EXPECT_EQ(result_fail.value().error_value(), ZX_ERR_INTERNAL);

  EXPECT_EQ(pwm_configs_set.size(), 2u);
  EXPECT_EQ(pwm_configs_set[0].duty_cycle, 75.0f);
  EXPECT_EQ(pwm_configs_set[1].duty_cycle, 50.0f);

  EXPECT_EQ(mode_configs_set.size(), 2u);
  EXPECT_EQ(mode_configs_set[0].mode, aml_pwm::Mode::kOff);
  EXPECT_EQ(mode_configs_set[1].mode, aml_pwm::Mode::kOn);

  fidl::WireResult result_get = fidl::WireCall(client())->GetStateNormalized();
  EXPECT_OK(result_get);
  EXPECT_TRUE(result_get.value().is_ok());

  // The stored state doesn't change.
  EXPECT_EQ(result_get.value()->state.backlight_on, true);
  EXPECT_EQ(result_get.value()->state.brightness, 0.5);
}

}  // namespace

}  // namespace vim3_pwm_backlight
