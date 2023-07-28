// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "thermal-cli.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/mock-function/mock-function.h>

#include <zxtest/zxtest.h>

namespace {

class ThermalCliTest : public zxtest::Test,
                       public fidl::WireServer<fuchsia_hardware_thermal::Device> {
 public:
  ThermalCliTest() : loop_(&kAsyncLoopConfigAttachToCurrentThread) {
    loop_.StartThread("thermal-cli-test-loop");
  }

  void SetUp() override {
    zx::result server = fidl::CreateEndpoints(&client_);
    ASSERT_OK(server);
    fidl::BindServer(loop_.dispatcher(), std::move(server.value()), this);
  }

  mock_function::MockFunction<zx_status_t>& MockGetTemperatureCelsius() {
    return mock_GetTemperatureCelsius_;
  }

  mock_function::MockFunction<zx_status_t>& MockGetFanLevel() { return mock_GetFanLevel_; }

  mock_function::MockFunction<zx_status_t, uint32_t>& MockSetFanLevel() {
    return mock_SetFanLevel_;
  }

  mock_function::MockFunction<zx_status_t, fuchsia_hardware_thermal::wire::PowerDomain>&
  MockGetDvfsInfo() {
    return mock_GetDvfsInfo_;
  }

  mock_function::MockFunction<zx_status_t, fuchsia_hardware_thermal::wire::PowerDomain>&
  MockGetDvfsOperatingPoint() {
    return mock_GetDvfsOperatingPoint_;
  }

  mock_function::MockFunction<zx_status_t, uint16_t, fuchsia_hardware_thermal::wire::PowerDomain>&
  MockSetDvfsOperatingPoint() {
    return mock_SetDvfsOperatingPoint_;
  }

 protected:
  void GetInfo(GetInfoCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
  }

  void GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
  }

  void GetDvfsInfo(GetDvfsInfoRequestView request, GetDvfsInfoCompleter::Sync& completer) override {
    mock_GetDvfsInfo_.Call(request->power_domain);

    fuchsia_hardware_thermal::wire::OperatingPointEntry entry0{
        .freq_hz = 100,
        .volt_uv = 42,
    };
    fuchsia_hardware_thermal::wire::OperatingPointEntry entry1{
        .freq_hz = 200,
        .volt_uv = 42,
    };
    fuchsia_hardware_thermal::wire::OperatingPoint op_info{
        .opp = {entry0, entry1},
        .latency = 42,
        .count = 2,
    };
    completer.Reply(ZX_OK, fidl::ObjectView<decltype(op_info)>::FromExternal(&op_info));
  }

  void GetTemperatureCelsius(GetTemperatureCelsiusCompleter::Sync& completer) override {
    zx_status_t status = mock_GetTemperatureCelsius_.Call();
    completer.Reply(status, 0);
  }

  void GetSensorName(GetSensorNameCompleter::Sync& completer) override {}

  void GetStateChangeEvent(GetStateChangeEventCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
  }

  void GetStateChangePort(GetStateChangePortCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED, {});
  }

  void SetTripCelsius(SetTripCelsiusRequestView request,
                      SetTripCelsiusCompleter::Sync& completer) override {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
  }

  void GetDvfsOperatingPoint(GetDvfsOperatingPointRequestView request,
                             GetDvfsOperatingPointCompleter::Sync& completer) override {
    mock_GetDvfsOperatingPoint_.Call(request->power_domain);
    completer.Reply(ZX_OK, 1);
  }

  void SetDvfsOperatingPoint(SetDvfsOperatingPointRequestView request,
                             SetDvfsOperatingPointCompleter::Sync& completer) override {
    mock_SetDvfsOperatingPoint_.Call(request->op_idx, request->power_domain);
    completer.Reply(ZX_OK);
  }

  void GetFanLevel(GetFanLevelCompleter::Sync& completer) override {
    zx_status_t status = mock_GetFanLevel_.Call();
    completer.Reply(status, 0);
  }

  void SetFanLevel(SetFanLevelRequestView request, SetFanLevelCompleter::Sync& completer) override {
    zx_status_t status = mock_SetFanLevel_.Call(request->fan_level);
    completer.Reply(status);
  }

  async::Loop loop_;
  fidl::ClientEnd<fuchsia_hardware_thermal::Device> client_;

  mock_function::MockFunction<zx_status_t> mock_GetTemperatureCelsius_;
  mock_function::MockFunction<zx_status_t> mock_GetFanLevel_;
  mock_function::MockFunction<zx_status_t, uint32_t> mock_SetFanLevel_;
  mock_function::MockFunction<zx_status_t, fuchsia_hardware_thermal::wire::PowerDomain>
      mock_GetDvfsInfo_;
  mock_function::MockFunction<zx_status_t, fuchsia_hardware_thermal::wire::PowerDomain>
      mock_GetDvfsOperatingPoint_;
  mock_function::MockFunction<zx_status_t, uint16_t, fuchsia_hardware_thermal::wire::PowerDomain>
      mock_SetDvfsOperatingPoint_;
};

TEST_F(ThermalCliTest, Temperature) {
  ThermalCli thermal_cli(std::move(client_));

  MockGetTemperatureCelsius().ExpectCall(ZX_OK);
  EXPECT_OK(thermal_cli.PrintTemperature());
  MockGetTemperatureCelsius().VerifyAndClear();
}

TEST_F(ThermalCliTest, TemperatureFails) {
  ThermalCli thermal_cli(std::move(client_));

  MockGetTemperatureCelsius().ExpectCall(ZX_ERR_IO);
  EXPECT_EQ(thermal_cli.PrintTemperature(), ZX_ERR_IO);
  MockGetTemperatureCelsius().VerifyAndClear();
}

TEST_F(ThermalCliTest, GetFanLevel) {
  ThermalCli thermal_cli(std::move(client_));

  MockGetFanLevel().ExpectCall(ZX_OK);
  MockSetFanLevel().ExpectNoCall();
  EXPECT_OK(thermal_cli.FanLevelCommand(nullptr));
  MockGetFanLevel().VerifyAndClear();
  MockSetFanLevel().VerifyAndClear();
}

TEST_F(ThermalCliTest, SetFanLevel) {
  ThermalCli thermal_cli(std::move(client_));

  MockGetFanLevel().ExpectNoCall();
  MockSetFanLevel().ExpectCall(ZX_OK, 42);
  EXPECT_OK(thermal_cli.FanLevelCommand("42"));
  MockGetFanLevel().VerifyAndClear();
  MockSetFanLevel().VerifyAndClear();
}

TEST_F(ThermalCliTest, InvalidFanLevel) {
  ThermalCli thermal_cli(std::move(client_));

  MockGetFanLevel().ExpectNoCall();
  MockSetFanLevel().ExpectNoCall();
  EXPECT_EQ(thermal_cli.FanLevelCommand("123abcd"), ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(thermal_cli.FanLevelCommand("-1"), ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(thermal_cli.FanLevelCommand("4294967295"), ZX_ERR_INVALID_ARGS);
  MockGetFanLevel().VerifyAndClear();
  MockSetFanLevel().VerifyAndClear();
}

TEST_F(ThermalCliTest, GetOperatingPoint) {
  ThermalCli thermal_cli(std::move(client_));

  MockGetDvfsInfo().ExpectCall(ZX_OK,
                               fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain);
  MockGetDvfsOperatingPoint().ExpectCall(
      ZX_OK, fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain);
  EXPECT_OK(thermal_cli.FrequencyCommand(
      fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain, nullptr));
  MockGetDvfsInfo().VerifyAndClear();
  MockGetDvfsOperatingPoint().VerifyAndClear();
}

TEST_F(ThermalCliTest, SetOperatingPoint) {
  ThermalCli thermal_cli(std::move(client_));

  MockGetDvfsInfo().ExpectCall(ZX_OK,
                               fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain);
  MockSetDvfsOperatingPoint().ExpectCall(
      ZX_OK, 1, fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain);
  EXPECT_OK(thermal_cli.FrequencyCommand(
      fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain, "200"));
  MockGetDvfsInfo().VerifyAndClear();
  MockSetDvfsOperatingPoint().VerifyAndClear();
}

TEST_F(ThermalCliTest, FrequencyNotFound) {
  ThermalCli thermal_cli(std::move(client_));

  MockGetDvfsInfo().ExpectCall(ZX_OK,
                               fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain);
  MockSetDvfsOperatingPoint().ExpectNoCall();
  EXPECT_EQ(thermal_cli.FrequencyCommand(
                fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain, "300"),
            ZX_ERR_NOT_FOUND);
  MockGetDvfsInfo().VerifyAndClear();
  MockSetDvfsOperatingPoint().VerifyAndClear();
}

TEST_F(ThermalCliTest, InvalidFrequency) {
  ThermalCli thermal_cli(std::move(client_));

  MockGetDvfsInfo()
      .ExpectCall(ZX_OK, fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain)
      .ExpectCall(ZX_OK, fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain)
      .ExpectCall(ZX_OK, fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain);
  MockSetDvfsOperatingPoint().ExpectNoCall();
  EXPECT_EQ(thermal_cli.FrequencyCommand(
                fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain, "123abcd"),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(thermal_cli.FrequencyCommand(
                fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain, "-1"),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(thermal_cli.FrequencyCommand(
                fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain, "4294967295"),
            ZX_ERR_INVALID_ARGS);
  MockGetDvfsInfo().VerifyAndClear();
  MockSetDvfsOperatingPoint().VerifyAndClear();
}

}  // namespace
