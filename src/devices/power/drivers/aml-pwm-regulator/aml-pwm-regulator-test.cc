// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/aml-pwm-regulator/aml-pwm-regulator.h"

#include <fidl/fuchsia.hardware.pwm/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.vreg/cpp/fidl.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/driver_lifecycle.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/sync/cpp/completion.h>

#include <list>

#include <zxtest/zxtest.h>
bool operator==(const fuchsia_hardware_pwm::wire::PwmConfig& lhs,
                const fuchsia_hardware_pwm::wire::PwmConfig& rhs) {
  return (lhs.polarity == rhs.polarity) && (lhs.period_ns == rhs.period_ns) &&
         (lhs.duty_cycle == rhs.duty_cycle) &&
         (lhs.mode_config.count() == rhs.mode_config.count()) &&
         (reinterpret_cast<aml_pwm::mode_config*>(lhs.mode_config.data())->mode ==
          reinterpret_cast<aml_pwm::mode_config*>(rhs.mode_config.data())->mode);
}

namespace aml_pwm_regulator {

void set_and_expect_voltage_step(fidl::WireSyncClient<fuchsia_hardware_vreg::Vreg>& vreg_client,
                                 uint32_t value) {
  fidl::WireResult result = vreg_client->SetVoltageStep(value);
  EXPECT_OK(result.status());
  EXPECT_TRUE(result->is_ok());

  fidl::WireResult voltage_step = vreg_client->GetVoltageStep();
  EXPECT_TRUE(voltage_step.ok());
  EXPECT_EQ(voltage_step->result, value);
}

class MockPwmServer final : public fidl::testing::WireTestBase<fuchsia_hardware_pwm::Pwm> {
 public:
  explicit MockPwmServer()
      : dispatcher_(fdf::Dispatcher::GetCurrent()->async_dispatcher()),
        outgoing_(fdf::Dispatcher::GetCurrent()->async_dispatcher()) {}

  void SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) override {
    ASSERT_TRUE(expect_configs_.size() > 0);
    auto expect_config = expect_configs_.front();

    ASSERT_EQ(request->config, expect_config);

    expect_configs_.pop_front();
    mode_config_buffers_.pop_front();
    completer.ReplySuccess();
  }

  void GetConfig(GetConfigCompleter::Sync& completer) override {
    ASSERT_TRUE(expect_default_configs_.size() > 0);
    auto default_config = expect_default_configs_.front();

    expect_default_configs_.pop_front();
    completer.ReplySuccess(default_config);
  }

  void Enable(EnableCompleter::Sync& completer) override {
    ASSERT_TRUE(expect_enable_);
    expect_enable_ = false;
    completer.ReplySuccess();
  }

  void ExpectEnable() { expect_enable_ = true; }

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

  void ExpectGetConfig(fuchsia_hardware_pwm::wire::PwmConfig config) {
    expect_default_configs_.push_back(std::move(config));
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

    auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
    ZX_ASSERT(outgoing_.Serve(std::move(endpoints.server)).is_ok());

    return std::move(endpoints.client);
  }

  void VerifyAndClear() {
    ASSERT_EQ(expect_configs_.size(), 0);
    ASSERT_EQ(mode_config_buffers_.size(), 0);
    ASSERT_FALSE(expect_enable_);
  }

  fuchsia_hardware_pwm::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_pwm::Service::InstanceHandler({
        .pwm = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                       fidl::kIgnoreBindingClosure),
    });
  }

 private:
  async_dispatcher_t* dispatcher_;
  component::OutgoingDirectory outgoing_;

  fidl::ServerBindingGroup<fuchsia_hardware_pwm::Pwm> bindings_;

  std::list<fuchsia_hardware_pwm::wire::PwmConfig> expect_configs_;
  std::list<fuchsia_hardware_pwm::wire::PwmConfig> expect_default_configs_;
  std::list<std::unique_ptr<uint8_t[]>> mode_config_buffers_;

  bool expect_enable_ = false;
};

struct IncomingNamespace {
  fdf_testing::TestNode test_node{std::string("root")};
  fdf_testing::internal::TestEnvironment test_env{fdf::Dispatcher::GetCurrent()->get()};
  compat::DeviceServer compat_server;

  MockPwmServer mock_pwm_server;
};

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
class AmlPwmRegulatorTest : public zxtest::Test {
 public:
  void SetUp() override {
    fuchsia_driver_framework::DriverStartArgs start_args;
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      auto start_args_result = incoming->test_node.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());
      start_args = std::move(start_args_result->start_args);
      outgoing_directory_client_ = std::move(start_args_result->outgoing_directory_client);

      auto init_result =
          incoming->test_env.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());

      incoming->compat_server.Initialize("pdev");

      // Setup metadata.
      fuchsia_hardware_vreg::VregMetadata metadata_vreg = {};
      metadata_vreg.name() = "pwm-0-regulator";
      metadata_vreg.min_voltage_uv() = 690000;
      metadata_vreg.voltage_step_uv() = 1000;
      metadata_vreg.num_steps() = 11;
      auto encoded_vreg = fidl::Persist(metadata_vreg);
      ASSERT_TRUE(encoded_vreg.is_ok());
      incoming->compat_server.AddMetadata(DEVICE_METADATA_VREG, encoded_vreg.value().data(),
                                          encoded_vreg.value().size());

      zx_status_t status =
          incoming->compat_server.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                        &incoming->test_env.incoming_directory());
      ASSERT_OK(status);

      // Serve mock pwm server.
      auto result =
          incoming->test_env.incoming_directory().AddService<fuchsia_hardware_pwm::Service>(
              incoming->mock_pwm_server.GetInstanceHandler(), "pwm");
      ASSERT_TRUE(result.is_ok());

      incoming->mock_pwm_server.ExpectEnable();
    });

    // Start dut_.
    auto result = runtime_.RunToCompletion(dut_.SyncCall(
        &fdf_testing::internal::DriverUnderTest<aml_pwm_regulator::AmlPwmRegulator>::Start,
        std::move(start_args)));
    ASSERT_TRUE(result.is_ok());
  }

  fidl::ClientEnd<fuchsia_hardware_vreg::Vreg> GetClient(std::string_view name) {
    auto svc_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

    zx_status_t status =
        fdio_open3_at(outgoing_directory_client_.handle()->get(), "/svc",
                      static_cast<uint64_t>(fuchsia_io::wire::Flags::kProtocolDirectory),
                      svc_endpoints.server.TakeChannel().release());
    EXPECT_EQ(ZX_OK, status);

    auto connect_result = component::ConnectAtMember<fuchsia_hardware_vreg::Service::Vreg>(
        svc_endpoints.client, name);
    EXPECT_TRUE(connect_result.is_ok());
    return std::move(connect_result.value());
  }

 private:
  fdf_testing::DriverRuntime runtime_;

  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client_;

 protected:
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{
      env_dispatcher_->async_dispatcher(), std::in_place};
  async_patterns::TestDispatcherBound<
      fdf_testing::internal::DriverUnderTest<aml_pwm_regulator::AmlPwmRegulator>>
      dut_{driver_dispatcher_->async_dispatcher(), std::in_place};
};

TEST_F(AmlPwmRegulatorTest, RegulatorTest) {
  fidl::WireSyncClient<fuchsia_hardware_vreg::Vreg> vreg_client(GetClient("pwm-0-regulator"));

  fidl::WireResult params = vreg_client->GetRegulatorParams();
  EXPECT_TRUE(params.ok());
  EXPECT_EQ(params->min_uv, 690'000);
  EXPECT_EQ(params->num_steps, 11);
  EXPECT_EQ(params->step_size_uv, 1'000);

  fidl::WireResult voltage_step = vreg_client->GetVoltageStep();
  EXPECT_TRUE(voltage_step.ok());
  EXPECT_EQ(voltage_step->result, 11);

  aml_pwm::mode_config mode = {
      .mode = aml_pwm::Mode::kOn,
      .regular = {},
  };
  fuchsia_hardware_pwm::wire::PwmConfig cfg = {
      .polarity = false,
      .period_ns = 1250,
  };

  incoming_.SyncCall(
      [&](IncomingNamespace* incoming) { incoming->mock_pwm_server.ExpectGetConfig(cfg); });

  cfg.duty_cycle = 70;
  cfg.mode_config =
      fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&mode), sizeof(mode));

  incoming_.SyncCall(
      [&](IncomingNamespace* incoming) { incoming->mock_pwm_server.ExpectSetConfig(cfg); });

  set_and_expect_voltage_step(vreg_client, 3);

  incoming_.SyncCall(
      [&](IncomingNamespace* incoming) { incoming->mock_pwm_server.ExpectGetConfig(cfg); });

  cfg.duty_cycle = 10;
  incoming_.SyncCall(
      [&](IncomingNamespace* incoming) { incoming->mock_pwm_server.ExpectSetConfig(cfg); });
  set_and_expect_voltage_step(vreg_client, 9);

  fidl::WireResult result = vreg_client->SetVoltageStep(14);
  EXPECT_TRUE(result.ok());
  EXPECT_TRUE(result->is_error());
  EXPECT_EQ(result->error_value(), ZX_ERR_INVALID_ARGS);

  incoming_.SyncCall(
      [&](IncomingNamespace* incoming) { incoming->mock_pwm_server.VerifyAndClear(); });
}

}  // namespace aml_pwm_regulator
