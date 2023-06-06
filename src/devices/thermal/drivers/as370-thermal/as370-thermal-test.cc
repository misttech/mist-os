// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "as370-thermal.h"

#include <fidl/fuchsia.hardware.clock/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.power/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.thermal/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/mock-function/mock-function.h>

#include <fbl/algorithm.h>
#include <mock-mmio-reg/mock-mmio-reg.h>
#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

bool FloatNear(float a, float b) { return std::abs(a - b) < 0.001f; }

}  // namespace

namespace thermal {

using ThermalClient = fidl::WireSyncClient<fuchsia_hardware_thermal::Device>;
using fuchsia_hardware_thermal::wire::OperatingPoint;
using fuchsia_hardware_thermal::wire::OperatingPointEntry;
using fuchsia_hardware_thermal::wire::PowerDomain;
using fuchsia_hardware_thermal::wire::ThermalDeviceInfo;

class FakeClockDevice : public fidl::testing::WireTestBase<fuchsia_hardware_clock::Clock> {
 public:
  void SetRate(SetRateRequestView request, SetRateCompleter::Sync& completer) override {
    rate_ = request->hz;
    if (send_io_err_) {
      completer.ReplyError(ZX_ERR_IO);
    } else {
      completer.ReplySuccess();
    }
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  zx::result<fidl::ClientEnd<fuchsia_hardware_clock::Clock>> BindServer() {
    if (binding_.has_value()) {
      return zx::error(ZX_ERR_BAD_STATE);
    }

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_clock::Clock>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }
    binding_.emplace(async_get_default_dispatcher(), std::move(endpoints->server), this,
                     fidl::kIgnoreBindingClosure);
    return zx::ok(std::move(endpoints->client));
  }

  uint64_t rate() const { return rate_; }

  void set_send_io_err(bool send) { send_io_err_ = send; }

 private:
  uint64_t rate_ = 0;
  bool send_io_err_ = false;
  std::optional<fidl::ServerBinding<fuchsia_hardware_clock::Clock>> binding_;
};

class FakePowerDevice : public fidl::testing::WireTestBase<fuchsia_hardware_power::Device> {
 public:
  void RegisterPowerDomain(RegisterPowerDomainRequestView request,
                           RegisterPowerDomainCompleter::Sync& completer) override {
    std::tuple<zx_status_t> ret = mock_register_power_domain_.Call(request->min_needed_voltage,
                                                                   request->max_supported_voltage);
    zx_status_t status = std::get<0>(ret);
    if (status == ZX_OK) {
      completer.ReplySuccess();
    } else {
      completer.ReplyError(status);
    }
  }
  void RequestVoltage(RequestVoltageRequestView request,
                      RequestVoltageCompleter::Sync& completer) override {
    std::tuple<zx_status_t, uint32_t> ret = mock_request_voltage_.Call(request->voltage);
    zx_status_t status = std::get<0>(ret);
    if (status == ZX_OK) {
      completer.ReplySuccess(std::get<1>(ret));
    } else {
      completer.ReplyError(status);
    }
  }

  void ExpectRegisterPowerDomain(zx_status_t out_s, uint32_t min_needed_voltage,
                                 uint32_t max_supported_voltage) {
    mock_register_power_domain_.ExpectCall({out_s}, min_needed_voltage, max_supported_voltage);
  }

  void ExpectRequestVoltage(zx_status_t out_s, uint32_t voltage, uint32_t out_actual_voltage) {
    mock_request_voltage_.ExpectCall({out_s, out_actual_voltage}, voltage);
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  void VerifyAndClear() {
    mock_register_power_domain_.VerifyAndClear();
    mock_request_voltage_.VerifyAndClear();
  }

  zx::result<fidl::ClientEnd<fuchsia_hardware_power::Device>> BindServer() {
    if (binding_.has_value()) {
      return zx::error(ZX_ERR_BAD_STATE);
    }

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_power::Device>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }
    binding_.emplace(async_get_default_dispatcher(), std::move(endpoints->server), this,
                     fidl::kIgnoreBindingClosure);
    return zx::ok(std::move(endpoints->client));
  }

 private:
  mock_function::MockFunction<std::tuple<zx_status_t>, uint32_t, uint32_t>
      mock_register_power_domain_;
  mock_function::MockFunction<std::tuple<zx_status_t, uint32_t>, uint32_t> mock_request_voltage_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_power::Device>> binding_;
};

class As370ThermalTest : public zxtest::Test {
 public:
  As370ThermalTest() : reg_region_(sizeof(uint32_t), 8) {}

  void SetUp() override {
    loop_.StartThread();
    ASSERT_OK(mock_fidl_servers_loop_.StartThread("mock-fidl-servers"));
    zx::result clock_client = clock_.SyncCall(&FakeClockDevice::BindServer);
    ASSERT_OK(clock_client);
    zx::result power_client = power_.SyncCall(&FakePowerDevice::BindServer);
    ASSERT_OK(power_client);
    dut_.emplace(nullptr, ddk::MmioBuffer(reg_region_.GetMmioBuffer()), kThermalDeviceInfo,
                 std::move(*clock_client), std::move(*power_client));
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_thermal::Device>();
    ASSERT_OK(endpoints.status_value());
    fidl::BindServer(loop_.dispatcher(), std::move(endpoints->server), &dut_.value());
    client_.Bind(std::move(endpoints->client));
  }

  void TearDown() override { loop_.Shutdown(); }

 protected:
  void VerifyAll() { ASSERT_NO_FATAL_FAILURE(power_.SyncCall(&FakePowerDevice::VerifyAndClear)); }

  std::shared_ptr<MockDevice> root_ = MockDevice::FakeRootParent();
  ddk_mock::MockMmioRegRegion reg_region_;
  async::Loop mock_fidl_servers_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<FakeClockDevice> clock_{mock_fidl_servers_loop_.dispatcher(),
                                                              std::in_place};
  async_patterns::TestDispatcherBound<FakePowerDevice> power_{mock_fidl_servers_loop_.dispatcher(),
                                                              std::in_place};
  std::optional<As370Thermal> dut_;
  ThermalClient client_;

 private:
  static constexpr ThermalDeviceInfo kThermalDeviceInfo = {
      .active_cooling = false,
      .passive_cooling = true,
      .gpu_throttling = false,
      .num_trip_points = 0,
      .big_little = false,
      .critical_temp_celsius = 0.0f,
      .trip_point_info = {},
      .opps =
          fidl::Array<OperatingPoint, 2>{
              OperatingPoint{
                  .opp =
                      fidl::Array<OperatingPointEntry, 16>{
                          // clang-format off
                          OperatingPointEntry{.freq_hz =   400'000'000, .volt_uv = 825'000},
                          OperatingPointEntry{.freq_hz =   800'000'000, .volt_uv = 825'000},
                          OperatingPointEntry{.freq_hz = 1'200'000'000, .volt_uv = 825'000},
                          OperatingPointEntry{.freq_hz = 1'400'000'000, .volt_uv = 825'000},
                          OperatingPointEntry{.freq_hz = 1'500'000'000, .volt_uv = 900'000},
                          OperatingPointEntry{.freq_hz = 1'800'000'000, .volt_uv = 900'000},
                          // clang-format on
                      },
                  .latency = 0,
                  .count = 6,
              },
              {
                  .opp = {},
                  .latency = 0,
                  .count = 0,
              },
          },
  };

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

TEST_F(As370ThermalTest, GetTemperature) {
  {
    reg_region_[0x14].ReadReturns(0x17ff);
    auto result = client_->GetTemperatureCelsius();
    EXPECT_OK(result->status);
    EXPECT_TRUE(FloatNear(result->temp, 40.314f));
  }

  {
    reg_region_[0x14].ReadReturns(0x182b);
    auto result = client_->GetTemperatureCelsius();
    EXPECT_OK(result->status);
    EXPECT_TRUE(FloatNear(result->temp, 43.019f));
  }
}

TEST_F(As370ThermalTest, DvfsOperatingPoint) {
  {
    // Success, sets operating point 0.
    power_.SyncCall(&FakePowerDevice::ExpectRequestVoltage, ZX_OK, 825'000, 825'000);
    auto set_result = client_->SetDvfsOperatingPoint(0, PowerDomain::kBigClusterPowerDomain);
    ASSERT_EQ(clock_.SyncCall(&FakeClockDevice::rate), 400'000'000);
    EXPECT_OK(set_result->status);
    ASSERT_NO_FATAL_FAILURE(VerifyAll());
  }

  {
    auto get_result = client_->GetDvfsOperatingPoint(PowerDomain::kBigClusterPowerDomain);
    EXPECT_OK(get_result->status);
    EXPECT_EQ(get_result->op_idx, 0);
  }

  {
    // Failure, unable to set exact voltage.
    power_.SyncCall(&FakePowerDevice::ExpectRequestVoltage, ZX_OK, 825'000, 900'000);
    auto set_result = client_->SetDvfsOperatingPoint(2, PowerDomain::kBigClusterPowerDomain);
    EXPECT_NOT_OK(set_result->status);
    ASSERT_NO_FATAL_FAILURE(VerifyAll());
  }

  {
    auto get_result = client_->GetDvfsOperatingPoint(PowerDomain::kBigClusterPowerDomain);
    EXPECT_OK(get_result->status);
    EXPECT_EQ(get_result->op_idx, 0);
  }

  {
    // Failure, unable to set frequency.
    power_.SyncCall(&FakePowerDevice::ExpectRequestVoltage, ZX_OK, 825'000, 825'000);
    clock_.SyncCall(&FakeClockDevice::set_send_io_err, true);
    auto set_result = client_->SetDvfsOperatingPoint(2, PowerDomain::kBigClusterPowerDomain);
    ASSERT_EQ(clock_.SyncCall(&FakeClockDevice::rate), 1'200'000'000);
    EXPECT_NOT_OK(set_result->status);
    ASSERT_NO_FATAL_FAILURE(VerifyAll());
    clock_.SyncCall(&FakeClockDevice::set_send_io_err, false);
  }

  {
    auto get_result = client_->GetDvfsOperatingPoint(PowerDomain::kBigClusterPowerDomain);
    EXPECT_OK(get_result->status);
    EXPECT_EQ(get_result->op_idx, 0);
  }

  {
    // Success, sets operating point 4.
    power_.SyncCall(&FakePowerDevice::ExpectRequestVoltage, ZX_OK, 900'000, 900'000);
    auto set_result = client_->SetDvfsOperatingPoint(4, PowerDomain::kBigClusterPowerDomain);
    ASSERT_EQ(clock_.SyncCall(&FakeClockDevice::rate), 1'500'000'000);
    EXPECT_OK(set_result->status);
    ASSERT_NO_FATAL_FAILURE(VerifyAll());
  }

  {
    auto get_result = client_->GetDvfsOperatingPoint(PowerDomain::kBigClusterPowerDomain);
    EXPECT_OK(get_result->status);
    EXPECT_EQ(get_result->op_idx, 4);
  }

  {
    // Failure, unable to set frequency.
    clock_.SyncCall(&FakeClockDevice::set_send_io_err, true);
    auto set_result = client_->SetDvfsOperatingPoint(1, PowerDomain::kBigClusterPowerDomain);
    ASSERT_EQ(clock_.SyncCall(&FakeClockDevice::rate), 800'000'000);
    EXPECT_NOT_OK(set_result->status);
    ASSERT_NO_FATAL_FAILURE(VerifyAll());
    clock_.SyncCall(&FakeClockDevice::set_send_io_err, false);
  }

  {
    auto get_result = client_->GetDvfsOperatingPoint(PowerDomain::kBigClusterPowerDomain);
    EXPECT_OK(get_result->status);
    EXPECT_EQ(get_result->op_idx, 4);
  }

  {
    // Failure, unable to set voltage.
    power_.SyncCall(&FakePowerDevice::ExpectRequestVoltage, ZX_ERR_IO, 825'000, 0);
    auto set_result = client_->SetDvfsOperatingPoint(1, PowerDomain::kBigClusterPowerDomain);
    ASSERT_EQ(clock_.SyncCall(&FakeClockDevice::rate), 800'000'000);
    EXPECT_NOT_OK(set_result->status);
    ASSERT_NO_FATAL_FAILURE(VerifyAll());
  }

  {
    auto get_result = client_->GetDvfsOperatingPoint(PowerDomain::kBigClusterPowerDomain);
    EXPECT_OK(get_result->status);
    EXPECT_EQ(get_result->op_idx, 4);
  }

  {
    // Success, sets operating point 1.
    power_.SyncCall(&FakePowerDevice::ExpectRequestVoltage, ZX_OK, 825'000, 825'000);
    auto set_result = client_->SetDvfsOperatingPoint(1, PowerDomain::kBigClusterPowerDomain);
    ASSERT_EQ(clock_.SyncCall(&FakeClockDevice::rate), 800'000'000);
    EXPECT_OK(set_result->status);
    ASSERT_NO_FATAL_FAILURE(VerifyAll());
  }

  {
    auto get_result = client_->GetDvfsOperatingPoint(PowerDomain::kBigClusterPowerDomain);
    EXPECT_OK(get_result->status);
    EXPECT_EQ(get_result->op_idx, 1);
  }
}

TEST_F(As370ThermalTest, Init) {
  power_.SyncCall(&FakePowerDevice::ExpectRegisterPowerDomain, ZX_OK, 825'000, 900'000);
  power_.SyncCall(&FakePowerDevice::ExpectRequestVoltage, ZX_OK, 900'000, 900'000);
  EXPECT_OK(dut_.value().Init());
  ASSERT_EQ(clock_.SyncCall(&FakeClockDevice::rate), 1'800'000'000);
  ASSERT_NO_FATAL_FAILURE(VerifyAll());
}

}  // namespace thermal
