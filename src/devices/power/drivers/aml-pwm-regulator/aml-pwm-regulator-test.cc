// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/aml-pwm-regulator/aml-pwm-regulator.h"

#include <fuchsia/hardware/pwm/cpp/banjo-mock.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/ddk/metadata.h>

#include <iterator>
#include <vector>

#include "fidl/fuchsia.hardware.vreg/cpp/wire_types.h"
#include "lib/fidl/cpp/wire/wire_messaging_declarations.h"
#include "lib/fidl/cpp/wire/wire_types.h"
#include "src/devices/lib/metadata/llcpp/vreg.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

bool operator==(const pwm_config_t& lhs, const pwm_config_t& rhs) {
  return (lhs.polarity == rhs.polarity) && (lhs.period_ns == rhs.period_ns) &&
         (lhs.duty_cycle == rhs.duty_cycle) && (lhs.mode_config_size == rhs.mode_config_size) &&
         (reinterpret_cast<aml_pwm::mode_config*>(lhs.mode_config_buffer)->mode ==
          reinterpret_cast<aml_pwm::mode_config*>(rhs.mode_config_buffer)->mode);
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

TEST(AmlPwmRegulatorTest, RegulatorTest) {
  // Create regulator device
  std::shared_ptr<MockDevice> fake_parent = MockDevice::FakeRootParent();
  ddk::MockPwm pwm;
  fake_parent->AddProtocol(ZX_PROTOCOL_PWM, pwm.GetProto()->ops, pwm.GetProto()->ctx, "pwm-0");
  fidl::Arena<2048> allocator;
  fuchsia_hardware_vreg::wire::PwmVregMetadataEntry pwm_entries[] = {
      vreg::BuildMetadata(allocator, 0, 1250, 690'000, 1'000, 11)};
  auto metadata = vreg::BuildMetadata(
      allocator, fidl::VectorView<fuchsia_hardware_vreg::wire::PwmVregMetadataEntry>::FromExternal(
                     pwm_entries));
  auto encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());
  pwm.ExpectEnable(ZX_OK);
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
      .mode = aml_pwm::ON,
      .regular = {},
  };
  pwm_config_t cfg = {
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 70,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&mode),
      .mode_config_size = sizeof(mode),
  };
  pwm.ExpectSetConfig(ZX_OK, cfg);
  set_and_expect_voltage_step(vreg_client, 3);

  cfg.duty_cycle = 10;
  pwm.ExpectSetConfig(ZX_OK, cfg);
  set_and_expect_voltage_step(vreg_client, 9);

  fidl::WireResult result = vreg_client->SetVoltageStep(14);
  EXPECT_TRUE(result.ok());
  EXPECT_TRUE(result->is_error());
  EXPECT_EQ(result->error_value(), ZX_ERR_INVALID_ARGS);

  pwm.VerifyAndClear();
}

}  // namespace aml_pwm_regulator
