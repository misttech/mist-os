// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-power.h"

#include <fidl/fuchsia.hardware.pwm/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.vreg/cpp/wire_test_base.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/fake-platform-device/cpp/fake-pdev.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <memory>
#include <optional>
#include <utility>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <gtest/gtest.h>
#include <soc/aml-common/aml-pwm-regs.h>

#include "src/lib/testing/predicates/status.h"

namespace {

bool operator==(const fuchsia_hardware_pwm::wire::PwmConfig& lhs,
                const fuchsia_hardware_pwm::wire::PwmConfig& rhs) {
  return (lhs.polarity == rhs.polarity) && (lhs.period_ns == rhs.period_ns) &&
         (lhs.duty_cycle == rhs.duty_cycle) && (lhs.mode_config.size() == rhs.mode_config.size()) &&
         (reinterpret_cast<aml_pwm::mode_config*>(lhs.mode_config.data())->mode ==
          reinterpret_cast<aml_pwm::mode_config*>(rhs.mode_config.data())->mode);
}

}  // namespace

namespace power::test {

const std::vector<aml_voltage_table_t> kTestVoltageTable = {
    {.microvolt = 1'050'000, .duty_cycle = 0},  {.microvolt = 1'040'000, .duty_cycle = 3},
    {.microvolt = 1'030'000, .duty_cycle = 6},  {.microvolt = 1'020'000, .duty_cycle = 8},
    {.microvolt = 1'010'000, .duty_cycle = 11}, {.microvolt = 1'000'000, .duty_cycle = 14},
    {.microvolt = 990'000, .duty_cycle = 17},   {.microvolt = 980'000, .duty_cycle = 20},
    {.microvolt = 970'000, .duty_cycle = 23},   {.microvolt = 960'000, .duty_cycle = 26},
    {.microvolt = 950'000, .duty_cycle = 29},   {.microvolt = 940'000, .duty_cycle = 31},
    {.microvolt = 930'000, .duty_cycle = 34},   {.microvolt = 920'000, .duty_cycle = 37},
    {.microvolt = 910'000, .duty_cycle = 40},   {.microvolt = 900'000, .duty_cycle = 43},
    {.microvolt = 890'000, .duty_cycle = 45},   {.microvolt = 880'000, .duty_cycle = 48},
    {.microvolt = 870'000, .duty_cycle = 51},   {.microvolt = 860'000, .duty_cycle = 54},
    {.microvolt = 850'000, .duty_cycle = 56},   {.microvolt = 840'000, .duty_cycle = 59},
    {.microvolt = 830'000, .duty_cycle = 62},   {.microvolt = 820'000, .duty_cycle = 65},
    {.microvolt = 810'000, .duty_cycle = 68},   {.microvolt = 800'000, .duty_cycle = 70},
    {.microvolt = 790'000, .duty_cycle = 73},   {.microvolt = 780'000, .duty_cycle = 76},
    {.microvolt = 770'000, .duty_cycle = 79},   {.microvolt = 760'000, .duty_cycle = 81},
    {.microvolt = 750'000, .duty_cycle = 84},   {.microvolt = 740'000, .duty_cycle = 87},
    {.microvolt = 730'000, .duty_cycle = 89},   {.microvolt = 720'000, .duty_cycle = 92},
    {.microvolt = 710'000, .duty_cycle = 95},   {.microvolt = 700'000, .duty_cycle = 98},
    {.microvolt = 690'000, .duty_cycle = 100},
};

constexpr voltage_pwm_period_ns_t kTestPwmPeriodNs = 1250;

class FakePwmServer final : public fidl::testing::WireTestBase<fuchsia_hardware_pwm::Pwm> {
 public:
  void SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) override {
    ASSERT_TRUE(expect_configs_.size() > 0);
    const auto& expect_config = expect_configs_.front();

    ASSERT_TRUE(request->config == expect_config);

    expect_configs_.pop_front();
    mode_config_buffers_.pop_front();
    completer.ReplySuccess();
  }
  void Enable(EnableCompleter::Sync& completer) override { completer.ReplySuccess(); }

  void ExpectSetConfig(fuchsia_hardware_pwm::wire::PwmConfig config) {
    std::unique_ptr<uint8_t[]> mode_config = std::make_unique<uint8_t[]>(config.mode_config.size());
    memcpy(mode_config.get(), config.mode_config.data(), config.mode_config.size());
    auto copy = config;
    copy.mode_config =
        fidl::VectorView<uint8_t>::FromExternal(mode_config.get(), config.mode_config.size());
    expect_configs_.push_back(std::move(copy));
    mode_config_buffers_.push_back(std::move(mode_config));
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  fuchsia_hardware_pwm::Service::InstanceHandler CreateInstanceHandler() {
    return fuchsia_hardware_pwm::Service::InstanceHandler{{
        .pwm = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                       fidl::kIgnoreBindingClosure),
    }};
  }

  void VerifyAndClear() {
    ASSERT_TRUE(expect_configs_.size() == 0);
    ASSERT_TRUE(mode_config_buffers_.size() == 0);
  }

 private:
  std::list<fuchsia_hardware_pwm::wire::PwmConfig> expect_configs_;
  std::list<std::unique_ptr<uint8_t[]>> mode_config_buffers_;
  fidl::ServerBindingGroup<fuchsia_hardware_pwm::Pwm> bindings_;
};

class FakeVregServer final : public fidl::testing::WireTestBase<fuchsia_hardware_vreg::Vreg> {
 public:
  void SetRegulatorParams(uint32_t min_uv, uint32_t step_size_uv, uint32_t num_steps) {
    min_uv_ = min_uv;
    step_size_uv_ = step_size_uv;
    num_steps_ = num_steps;
  }

  void GetRegulatorParams(GetRegulatorParamsCompleter::Sync& completer) override {
    completer.ReplySuccess(min_uv_, step_size_uv_, num_steps_);
  }

  void SetVoltageStep(::fuchsia_hardware_vreg::wire::VregSetVoltageStepRequest* request,
                      SetVoltageStepCompleter::Sync& completer) override {
    voltage_step_ = request->step;
    completer.Reply(fit::success());
  }

  void GetVoltageStep(GetVoltageStepCompleter::Sync& completer) override {
    completer.ReplySuccess(voltage_step_);
  }

  void Enable(EnableCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void Disable(DisableCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  uint32_t voltage_step() const { return voltage_step_; }

  fuchsia_hardware_vreg::Service::InstanceHandler CreateInstanceHandler() {
    return fuchsia_hardware_vreg::Service::InstanceHandler{{
        .vreg = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                        fidl::kIgnoreBindingClosure),
    }};
  }

 private:
  uint32_t min_uv_;
  uint32_t step_size_uv_;
  uint32_t num_steps_;
  uint32_t voltage_step_;
  fidl::ServerBindingGroup<fuchsia_hardware_vreg::Vreg> bindings_;
};

class AmlPowerTestEnvironment : public fdf_testing::Environment {
 public:
  enum class Mode : uint8_t {
    kAstro,
    kVim3,
  };

  void InitAstro(std::span<aml_voltage_table_t> voltage_table,
                 const voltage_pwm_period_ns_t& pwm_period) {
    device_server_.AddMetadata(DEVICE_METADATA_AML_VOLTAGE_TABLE, voltage_table.data(),
                               voltage_table.size() * sizeof(aml_voltage_table_t));
    device_server_.AddMetadata(DEVICE_METADATA_AML_PWM_PERIOD_NS, &pwm_period, sizeof(pwm_period));
    mode_ = Mode::kAstro;
  }

  void InitVim3() { mode_ = Mode::kVim3; }

  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    auto* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();

    device_server_.Initialize(component::kDefaultInstance, std::nullopt);
    zx_status_t status = device_server_.Serve(dispatcher, &to_driver_vfs);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    {
      zx::result result = to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
          pdev_.GetInstanceHandler(dispatcher));
      if (result.is_error()) {
        return result.take_error();
      }
    }

    switch (mode_) {
      case Mode::kAstro:
        return ServeAstro(to_driver_vfs);
      case Mode::kVim3:
        return ServeVim3(to_driver_vfs);
    }
  }

  void Shutdown() { primary_cluster_pwm_.VerifyAndClear(); }

  FakePwmServer& primary_cluster_pwm() { return primary_cluster_pwm_; }
  FakeVregServer& big_cluster_vreg() { return big_cluster_vreg_; }
  FakeVregServer& little_cluster_vreg() { return little_cluster_vreg_; }

 private:
  zx::result<> ServeAstro(fdf::OutgoingDirectory& to_driver_vfs) {
    zx::result result = to_driver_vfs.AddService<fuchsia_hardware_pwm::Service>(
        primary_cluster_pwm_.CreateInstanceHandler(), AmlPower::kPwmPrimaryParentName);
    if (result.is_error()) {
      return result.take_error();
    }

    return zx::ok();
  }

  zx::result<> ServeVim3(fdf::OutgoingDirectory& to_driver_vfs) {
    zx::result result = to_driver_vfs.AddService<fuchsia_hardware_vreg::Service>(
        big_cluster_vreg_.CreateInstanceHandler(), AmlPower::kVregPwmBigParentName);
    if (result.is_error()) {
      return result.take_error();
    }

    result = to_driver_vfs.AddService<fuchsia_hardware_vreg::Service>(
        little_cluster_vreg_.CreateInstanceHandler(), AmlPower::kVregPwmLittleParentName);
    if (result.is_error()) {
      return result.take_error();
    }

    return zx::ok();
  }

  Mode mode_;

  // Mmio Regs and Regions
  FakePwmServer primary_cluster_pwm_;
  FakeVregServer big_cluster_vreg_;
  FakeVregServer little_cluster_vreg_;

  compat::DeviceServer device_server_;
  fdf_fake::FakePDev pdev_;
};

class FixtureConfig final {
 public:
  using DriverType = AmlPower;
  using EnvironmentType = AmlPowerTestEnvironment;
};

class AmlPowerTest : public ::testing::Test {
 public:
  void TearDown() override {
    ASSERT_OK(driver_test_.StopDriver());
    driver_test_.RunInEnvironmentTypeContext([](auto& env) { env.Shutdown(); });
  }

 protected:
  void StartDriverAstro(std::vector<aml_voltage_table_t> voltage_table = kTestVoltageTable,
                        voltage_pwm_period_ns_t pwm_period = kTestPwmPeriodNs) {
    driver_test_.RunInEnvironmentTypeContext(
        [&](auto& env) { env.InitAstro(voltage_table, pwm_period); });
    StartDriver(
        std::vector{MakeOffer<fuchsia_hardware_pwm::Service>(AmlPower::kPwmPrimaryParentName)});
  }

  void StartDriverVim3() {
    driver_test_.RunInEnvironmentTypeContext([&](auto& env) { env.InitVim3(); });
    StartDriver(
        std::vector{MakeOffer<fuchsia_hardware_vreg::Service>(AmlPower::kVregPwmBigParentName),
                    MakeOffer<fuchsia_hardware_vreg::Service>(AmlPower::kVregPwmLittleParentName)});
  }

  void PrimaryClusterPwmExpectSetConfig(fuchsia_hardware_pwm::wire::PwmConfig cfg) {
    driver_test_.RunInEnvironmentTypeContext(
        [cfg](auto& env) { env.primary_cluster_pwm().ExpectSetConfig(cfg); });
  }

  template <typename T>
  T WithBigClusterVregContext(fit::callback<T(FakeVregServer&)> callback) {
    return driver_test_.RunInEnvironmentTypeContext<T>(
        [callback = std::move(callback)](auto& env) mutable {
          return callback(env.big_cluster_vreg());
        });
  }

  template <typename T>
  T WithLittleClusterVregContext(fit::callback<T(FakeVregServer&)> callback) {
    return driver_test_.RunInEnvironmentTypeContext<T>(
        [callback = std::move(callback)](auto& env) mutable {
          return callback(env.little_cluster_vreg());
        });
  }

  AmlPower& driver() { return *driver_test_.driver(); }

 private:
  // Different from `fdf::MakeOffer2()`. This function correctly maps the service's instance name so
  // that the driver's service validator works correctly. In a real environment, this mapping
  // correction is performed by the driver framework.
  template <typename Service>
  static fuchsia_driver_framework::Offer MakeOffer(
      std::string_view instance_name = fdf::kDefaultInstance) {
    static_assert(fidl::IsServiceV<Service>, "Service must be a fidl Service");

    auto mapping = fuchsia_component_decl::NameMapping{{
        .source_name = std::string{instance_name},
        // `fdf::MakeOffer2()` would set `target_name` to `fdf::kDefaultInstance`.
        .target_name = std::string{instance_name},
    }};

    // `fdf::MakeOffer2()` would set this to `fdf::kDefaultInstance`.
    auto includes = std::string{instance_name};

    auto service = fuchsia_component_decl::OfferService{{
        .source_name = std::string(Service::Name),
        .target_name = std::string(Service::Name),
        .source_instance_filter = std::vector{std::string(includes)},
        .renamed_instances = std::vector{std::move(mapping)},
    }};

    return fuchsia_driver_framework::Offer::WithZirconTransport(
        fuchsia_component_decl::Offer::WithService(service));
  }

  // `node_offers` are appended to the node offers provided to `AmlPower::Start()`.
  void StartDriver(std::vector<fuchsia_driver_framework::Offer> node_offers) {
    ASSERT_OK(driver_test_.StartDriverWithCustomStartArgs(
        [additional_node_offers = std::move(node_offers)](fdf::DriverStartArgs& start_args) {
          // Enable service-connect validation.
          if (!start_args.program().has_value()) {
            start_args.program().emplace();
          }
          auto& program = start_args.program().value();
          if (!program.entries().has_value()) {
            program.entries().emplace(std::vector<fuchsia_data::DictionaryEntry>{});
          }
          auto& entries = program.entries().value();
          // TOOD(b/413437623): Possibly replace manual creation of program entry for the
          // service-correct validation once driver test framework provides a way to easily enable
          // service-connect validation.
          entries.emplace_back("service_connect_validation",
                               std::make_unique<fuchsia_data::DictionaryValue>(
                                   fuchsia_data::DictionaryValue::WithStr("true")));

          // Add node offers so that service-connect validation performs correctly.
          // TOOD(b/413437623): Don't manually create node offers once the driver test framework
          // correctly creates node offers from the test environment.
          if (!start_args.node_offers().has_value()) {
            start_args.node_offers().emplace(std::vector<fuchsia_driver_framework::Offer>{});
          }
          auto& node_offers = start_args.node_offers().value();
          node_offers.insert(node_offers.end(), additional_node_offers.begin(),
                             additional_node_offers.end());
          node_offers.emplace_back(MakeOffer<fuchsia_driver_compat::Service>());
          node_offers.emplace_back(MakeOffer<fuchsia_hardware_platform_device::Service>());
        }));
  }

  fdf_testing::ForegroundDriverTest<FixtureConfig> driver_test_;
};

TEST_F(AmlPowerTest, SetVoltage) {
  StartDriverAstro();
  zx_status_t st;
  constexpr uint32_t kTestVoltageInitial = 690'000;
  constexpr uint32_t kTestVoltageFinal = 1'040'000;

  aml_pwm::mode_config on = {aml_pwm::Mode::kOn, {}};

  // Initialize to 0.69V
  fuchsia_hardware_pwm::wire::PwmConfig cfg{.polarity = false,
                                            .period_ns = 1250,
                                            .duty_cycle = 100,
                                            .mode_config = fidl::VectorView<uint8_t>::FromExternal(
                                                reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);

  // Scale up to 1.05V
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 92,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 84,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 76,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 68,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 59,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 51,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 43,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 34,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 26,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 17,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 8,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 3,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);

  uint32_t actual;
  st = driver().PowerImplRequestVoltage(bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG,
                                        kTestVoltageInitial, &actual);
  EXPECT_EQ(kTestVoltageInitial, actual);
  EXPECT_OK(st);

  st = driver().PowerImplRequestVoltage(bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG,
                                        kTestVoltageFinal, &actual);
  EXPECT_EQ(kTestVoltageFinal, actual);
  EXPECT_OK(st);
}

TEST_F(AmlPowerTest, ClusterIndexOutOfRange) {
  constexpr uint32_t kTestVoltage = 690'000;

  StartDriverAstro();

  uint32_t actual;
  zx_status_t st = driver().PowerImplRequestVoltage(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE, kTestVoltage, &actual);
  EXPECT_NE(st, ZX_OK);
}

TEST_F(AmlPowerTest, GetVoltageUnset) {
  // Get the voltage before it's been set. Should return ZX_ERR_BAD_STATE.
  StartDriverAstro();

  uint32_t voltage;
  zx_status_t st = driver().PowerImplGetCurrentVoltage(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE, &voltage);
  EXPECT_NE(st, ZX_OK);
}

TEST_F(AmlPowerTest, GetVoltage) {
  // Get the voltage before it's been set. Should return ZX_ERR_BAD_STATE.
  constexpr uint32_t kTestVoltage = 690'000;
  zx_status_t st;

  StartDriverAstro();

  // Initialize to 0.69V
  aml_pwm::mode_config on = {aml_pwm::Mode::kOn, {}};
  fuchsia_hardware_pwm::wire::PwmConfig cfg = {
      false, 1250, 100,
      fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);

  uint32_t requested_voltage, actual_voltage;
  st = driver().PowerImplRequestVoltage(bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG,
                                        kTestVoltage, &requested_voltage);
  EXPECT_OK(st);
  EXPECT_EQ(requested_voltage, kTestVoltage);

  st = driver().PowerImplGetCurrentVoltage(bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG,
                                           &actual_voltage);
  EXPECT_OK(st);
  EXPECT_EQ(requested_voltage, actual_voltage);
}

TEST_F(AmlPowerTest, GetVoltageOutOfRange) {
  StartDriverAstro();

  uint32_t voltage;
  zx_status_t st = driver().PowerImplGetCurrentVoltage(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG, &voltage);
  EXPECT_NE(st, ZX_OK);
}

TEST_F(AmlPowerTest, SetVoltageRoundDown) {
  // Set a voltage that's not exactly supported and let the driver round down to the nearest
  // voltage.
  StartDriverAstro();
  constexpr uint32_t kTestVoltageInitial = 830'000;

  // We expect the driver to give us the highest voltage that does not exceed the requested voltage.
  constexpr uint32_t kTestVoltageFinalRequest = 935'000;
  constexpr uint32_t kTestVoltageFinalActual = 930'000;

  aml_pwm::mode_config on = {aml_pwm::Mode::kOn, {}};
  fuchsia_hardware_pwm::wire::PwmConfig cfg{.polarity = false,
                                            .period_ns = 1250,
                                            .duty_cycle = 62,
                                            .mode_config = fidl::VectorView<uint8_t>::FromExternal(
                                                reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 54,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 45,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 37,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 34,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);

  uint32_t actual;
  zx_status_t st;

  st = driver().PowerImplRequestVoltage(bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG,
                                        kTestVoltageInitial, &actual);
  EXPECT_OK(st);
  EXPECT_EQ(actual, kTestVoltageInitial);

  st = driver().PowerImplRequestVoltage(bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG,
                                        kTestVoltageFinalRequest, &actual);
  EXPECT_OK(st);
  EXPECT_EQ(actual, kTestVoltageFinalActual);
}

TEST_F(AmlPowerTest, SetVoltageLittleCluster) {
  // Set a voltage that's not exactly supported and let the driver round down to the nearest
  // voltage.
  StartDriverAstro();
  constexpr uint32_t kTestVoltageInitial = 730'000;
  constexpr uint32_t kTestVoltageFinal = 930'000;

  aml_pwm::mode_config on = {aml_pwm::Mode::kOn, {}};
  fuchsia_hardware_pwm::wire::PwmConfig cfg{.polarity = false,
                                            .period_ns = 1250,
                                            .duty_cycle = 89,
                                            .mode_config = fidl::VectorView<uint8_t>::FromExternal(
                                                reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 81,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 73,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 65,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 56,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 48,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 40,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);
  cfg = {.polarity = false,
         .period_ns = 1250,
         .duty_cycle = 34,
         .mode_config =
             fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&on), sizeof(on))};
  PrimaryClusterPwmExpectSetConfig(cfg);

  uint32_t actual;
  zx_status_t st;

  st = driver().PowerImplRequestVoltage(bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG,
                                        kTestVoltageInitial, &actual);
  EXPECT_OK(st);
  EXPECT_EQ(actual, kTestVoltageInitial);

  st = driver().PowerImplRequestVoltage(bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG,
                                        kTestVoltageFinal, &actual);
  EXPECT_OK(st);
  EXPECT_EQ(actual, kTestVoltageFinal);
}

TEST_F(AmlPowerTest, DomainEnableDisable) {
  StartDriverVim3();

  // Enable.
  EXPECT_OK(driver().PowerImplEnablePowerDomain(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE));
  EXPECT_OK(driver().PowerImplEnablePowerDomain(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG));

  // Disable.
  EXPECT_OK(driver().PowerImplDisablePowerDomain(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE));
  EXPECT_OK(driver().PowerImplDisablePowerDomain(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG));

  // Out of bounds.
  EXPECT_NE(driver().PowerImplDisablePowerDomain(
                bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE + 1),
            ZX_OK);
  EXPECT_NE(driver().PowerImplEnablePowerDomain(
                bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE + 1),
            ZX_OK);
}

TEST_F(AmlPowerTest, GetDomainStatus) {
  StartDriverAstro();

  // Happy case.
  power_domain_status_t result;
  EXPECT_OK(driver().PowerImplGetPowerDomainStatus(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG, &result));
  EXPECT_EQ(result, POWER_DOMAIN_STATUS_ENABLED);

  // Out of bounds.
  EXPECT_NE(driver().PowerImplGetPowerDomainStatus(
                bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE, &result),
            ZX_OK);
}

TEST_F(AmlPowerTest, Vim3SetBigCluster) {
  StartDriverVim3();

  WithBigClusterVregContext<void>(
      [](FakeVregServer& cluster_vreg) { cluster_vreg.SetRegulatorParams(100, 10, 10); });
  const uint32_t kTestVoltage = 155;
  uint32_t actual;
  EXPECT_OK(driver().PowerImplRequestVoltage(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG, kTestVoltage, &actual));
  EXPECT_EQ(actual, 150u);
  uint32_t voltage_step = WithBigClusterVregContext<uint32_t>(
      [](FakeVregServer& cluster_vreg) { return cluster_vreg.voltage_step(); });
  EXPECT_EQ(voltage_step, 5u);

  // Voltage is too low.
  EXPECT_NE(driver().PowerImplRequestVoltage(
                bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG, 99, &actual),
            ZX_OK);

  // Set voltage to the threshold.
  EXPECT_OK(driver().PowerImplRequestVoltage(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG, 200, &actual));
  EXPECT_EQ(actual, 200u);
  voltage_step = WithBigClusterVregContext<uint32_t>(
      [](FakeVregServer& cluster_vreg) { return cluster_vreg.voltage_step(); });
  EXPECT_EQ(voltage_step, 10u);

  // Set voltage beyond the threshold.
  EXPECT_NE(driver().PowerImplRequestVoltage(
                bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG, 300, &actual),
            ZX_OK);
}

TEST_F(AmlPowerTest, Vim3SetLittleCluster) {
  StartDriverVim3();

  WithLittleClusterVregContext<void>(
      [](FakeVregServer& cluster_vreg) { cluster_vreg.SetRegulatorParams(100, 10, 10); });
  const uint32_t kTestVoltage = 155;
  uint32_t actual;
  EXPECT_OK(driver().PowerImplRequestVoltage(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE, kTestVoltage, &actual));
  EXPECT_EQ(actual, 150u);
  uint32_t voltage_step = WithLittleClusterVregContext<uint32_t>(
      [](FakeVregServer& cluster_vreg) { return cluster_vreg.voltage_step(); });
  EXPECT_EQ(voltage_step, 5u);

  // Voltage is too low.
  EXPECT_NE(driver().PowerImplRequestVoltage(
                bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE, 99, &actual),
            ZX_OK);

  // Set voltage to the threshold.
  EXPECT_OK(driver().PowerImplRequestVoltage(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE, 200, &actual));
  EXPECT_EQ(actual, 200u);
  voltage_step = WithLittleClusterVregContext<uint32_t>(
      [](FakeVregServer& cluster_vreg) { return cluster_vreg.voltage_step(); });
  EXPECT_EQ(voltage_step, 10u);

  // Set voltage beyond the threshold.
  EXPECT_NE(driver().PowerImplRequestVoltage(
                bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE, 300, &actual),
            ZX_OK);
}

TEST_F(AmlPowerTest, Vim3GetSupportedVoltageRange) {
  StartDriverVim3();

  WithBigClusterVregContext<void>(
      [](FakeVregServer& cluster_vreg) { cluster_vreg.SetRegulatorParams(100, 10, 10); });

  uint32_t max, min;
  EXPECT_OK(driver().PowerImplGetSupportedVoltageRange(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_BIG, &min, &max));
  EXPECT_EQ(max, 200u);
  EXPECT_EQ(min, 100u);

  WithLittleClusterVregContext<void>(
      [](FakeVregServer& cluster_vreg) { cluster_vreg.SetRegulatorParams(100, 20, 5); });

  EXPECT_OK(driver().PowerImplGetSupportedVoltageRange(
      bind_fuchsia_amlogic_platform::POWER_DOMAIN_ARM_CORE_LITTLE, &min, &max));
  EXPECT_EQ(max, 200u);
  EXPECT_EQ(min, 100u);
}

}  // namespace power::test
