// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/aml-pwm-regulator/aml-pwm-regulator.h"

#include <fidl/fuchsia.hardware.pwm/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/sync/cpp/completion.h>

#include <iterator>
#include <vector>

#include <zxtest/zxtest.h>

#include "fidl/fuchsia.hardware.vreg/cpp/wire_types.h"
#include "lib/fidl/cpp/wire/wire_messaging_declarations.h"
#include "lib/fidl/cpp/wire/wire_types.h"
#include "src/devices/lib/metadata/llcpp/vreg.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

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
  explicit MockPwmServer(async_dispatcher_t* dispatcher)
      : dispatcher_(dispatcher), outgoing_(dispatcher) {}

  void SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) override {
    ASSERT_TRUE(expect_configs_.size() > 0);
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

  void VerifyAndClear() {
    ASSERT_EQ(expect_configs_.size(), 0);
    ASSERT_EQ(mode_config_buffers_.size(), 0);
    ASSERT_FALSE(expect_enable_);
  }

 private:
  async_dispatcher_t* dispatcher_;
  component::OutgoingDirectory outgoing_;

  std::list<fuchsia_hardware_pwm::wire::PwmConfig> expect_configs_;
  std::list<std::unique_ptr<uint8_t[]>> mode_config_buffers_;

  bool expect_enable_ = false;
};

TEST(AmlPwmRegulatorTest, RegulatorTest) {
  // Create regulator device
  std::shared_ptr<MockDevice> fake_parent = MockDevice::FakeRootParent();

  async::Loop pwm_loop{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<MockPwmServer> pwm{pwm_loop.dispatcher(), std::in_place,
                                                         async_patterns::PassDispatcher};
  EXPECT_OK(pwm_loop.StartThread("pwm-servers"));

  fake_parent->AddFidlService(fuchsia_hardware_pwm::Service::Name,
                              pwm.SyncCall(&MockPwmServer::Connect), "pwm-0");

  fidl::Arena<2048> allocator;
  fuchsia_hardware_vreg::wire::PwmVregMetadataEntry pwm_entries[] = {
      vreg::BuildMetadata(allocator, 0, 1250, 690'000, 1'000, 11)};
  auto metadata = vreg::BuildMetadata(
      allocator, fidl::VectorView<fuchsia_hardware_vreg::wire::PwmVregMetadataEntry>::FromExternal(
                     pwm_entries));
  auto encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());
  pwm.SyncCall(&MockPwmServer::ExpectEnable);
  fake_parent->SetMetadata(DEVICE_METADATA_VREG, encoded.value().data(), encoded.value().size());
  EXPECT_OK(AmlPwmRegulator::Create(nullptr, fake_parent.get()));
  MockDevice* child_dev = fake_parent->GetLatestChild();
  AmlPwmRegulator* regulator = child_dev->GetDeviceContext<AmlPwmRegulator>();

  // Get vreg client
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_vreg::Vreg>();
  auto vreg_server = fidl::BindServer(loop.dispatcher(), std::move(endpoints->server), regulator);
  loop.StartThread("vreg-server");
  fidl::WireSyncClient vreg_client{std::move(endpoints->client)};

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
      .duty_cycle = 70,
      .mode_config =
          fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&mode), sizeof(mode)),
  };

  pwm.SyncCall(&MockPwmServer::ExpectSetConfig, cfg);
  set_and_expect_voltage_step(vreg_client, 3);

  cfg.duty_cycle = 10;
  pwm.SyncCall(&MockPwmServer::ExpectSetConfig, cfg);
  set_and_expect_voltage_step(vreg_client, 9);

  fidl::WireResult result = vreg_client->SetVoltageStep(14);
  EXPECT_TRUE(result.ok());
  EXPECT_TRUE(result->is_error());
  EXPECT_EQ(result->error_value(), ZX_ERR_INVALID_ARGS);

  pwm.SyncCall(&MockPwmServer::VerifyAndClear);
}

}  // namespace aml_pwm_regulator
