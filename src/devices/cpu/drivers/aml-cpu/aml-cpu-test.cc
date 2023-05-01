// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-cpu.h"

#include <fuchsia/hardware/clock/cpp/banjo-mock.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pdev-fidl.h>

#include <memory>
#include <vector>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <fake-mmio-reg/fake-mmio-reg.h>
#include <sdk/lib/inspect/testing/cpp/zxtest/inspect.h>
#include <zxtest/zxtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace amlogic_cpu {

using fuchsia_device::wire::kMaxDevicePerformanceStates;
using CpuCtrlClient = fidl::WireSyncClient<fuchsia_cpuctrl::Device>;
using inspect::InspectTestHelper;

#define MHZ(x) ((x)*1000000)

constexpr uint32_t kPdArmA53 = 1;

constexpr amlogic_cpu::perf_domain_t kPerfDomains[] = {
    {.id = kPdArmA53, .core_count = 4, .relative_performance = 255, .name = "S905D2 ARM A53"},
};

constexpr amlogic_cpu::operating_point_t kOperatingPointsMetadata[] = {
    {.freq_hz = 100'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 250'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 500'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 667'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'000'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'200'000'000, .volt_uv = 731'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'398'000'000, .volt_uv = 761'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'512'000'000, .volt_uv = 791'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'608'000'000, .volt_uv = 831'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'704'000'000, .volt_uv = 861'000, .pd_id = kPdArmA53},
    {.freq_hz = 1'896'000'000, .volt_uv = 1'022'000, .pd_id = kPdArmA53},
};

const std::vector<operating_point_t> kTestOperatingPoints = {
    {.freq_hz = MHZ(10), .volt_uv = 1500, .pd_id = 0},
    {.freq_hz = MHZ(9), .volt_uv = 1350, .pd_id = 0},
    {.freq_hz = MHZ(8), .volt_uv = 1200, .pd_id = 0},
    {.freq_hz = MHZ(7), .volt_uv = 1050, .pd_id = 0},
    {.freq_hz = MHZ(6), .volt_uv = 900, .pd_id = 0},
    {.freq_hz = MHZ(5), .volt_uv = 750, .pd_id = 0},
    {.freq_hz = MHZ(4), .volt_uv = 600, .pd_id = 0},
    {.freq_hz = MHZ(3), .volt_uv = 450, .pd_id = 0},
    {.freq_hz = MHZ(2), .volt_uv = 300, .pd_id = 0},
    {.freq_hz = MHZ(1), .volt_uv = 150, .pd_id = 0},
};

const uint32_t kTestCoreCount = 1;

class FakeMmio {
 public:
  FakeMmio() : mmio_(sizeof(uint32_t), kRegCount) {
    mmio_[kCpuVersionOffset].SetReadCallback([]() { return kCpuVersion; });
  }

  fdf::MmioBuffer mmio() { return mmio_.GetMmioBuffer(); }

 private:
  static constexpr size_t kCpuVersionOffset = 0x220;
  static constexpr size_t kRegCount = kCpuVersionOffset / sizeof(uint32_t) + 1;

  constexpr static uint64_t kCpuVersion = 43;

  ddk_fake::FakeMmioRegRegion mmio_;
};

class TestPowerDevice : public fidl::WireServer<fuchsia_hardware_power::Device> {
 public:
  explicit TestPowerDevice(bool use_mocks) : use_mocks_(use_mocks) {}

  fuchsia_hardware_power::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_power::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(this, async_get_default_dispatcher(),
                                               fidl::kIgnoreBindingClosure),
    });
  }

  void ConnectRequest(fidl::ServerEnd<fuchsia_hardware_power::Device> request) {
    binding_group_.AddBinding(async_get_default_dispatcher(), std::move(request), this,
                              fidl::kIgnoreBindingClosure);
  }

  fidl::ClientEnd<fuchsia_hardware_power::Device> Connect() {
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_power::Device>();
    EXPECT_OK(endpoints);
    ConnectRequest(std::move(endpoints->server));
    return std::move(endpoints->client);
  }

  void RegisterPowerDomain(RegisterPowerDomainRequestView request,
                           RegisterPowerDomainCompleter::Sync& completer) override {
    if (use_mocks_) {
      auto result = mock_register_power_domain_.Call(request->min_needed_voltage,
                                                     request->max_supported_voltage);
      zx_status_t status = std::get<0>(result);
      if (status == ZX_OK) {
        completer.ReplySuccess();
        return;
      }

      completer.ReplyError(status);
      return;
    }

    completer.ReplySuccess();
  }

  void UnregisterPowerDomain(UnregisterPowerDomainCompleter::Sync& completer) override {
    if (use_mocks_) {
      auto result = mock_unregister_power_domain_.Call();
      zx_status_t status = std::get<0>(result);
      if (status == ZX_OK) {
        completer.ReplySuccess();
        return;
      }

      completer.ReplyError(status);
      return;
    }

    completer.ReplySuccess();
  }
  void GetPowerDomainStatus(GetPowerDomainStatusCompleter::Sync& completer) override {
    if (use_mocks_) {
      auto result = mock_get_power_domain_status_.Call();
      zx_status_t status = std::get<0>(result);
      if (status == ZX_OK) {
        completer.ReplySuccess(std::get<1>(result));
        return;
      }

      completer.ReplyError(status);
      return;
    }

    completer.ReplySuccess(fuchsia_hardware_power::wire::PowerDomainStatus::kEnabled);
  }
  void GetSupportedVoltageRange(GetSupportedVoltageRangeCompleter::Sync& completer) override {
    if (use_mocks_) {
      auto result = mock_get_supported_voltage_range_.Call();
      zx_status_t status = std::get<0>(result);
      if (status == ZX_OK) {
        completer.ReplySuccess(std::get<1>(result), std::get<2>(result));
        return;
      }

      completer.ReplyError(status);
      return;
    }

    completer.ReplySuccess(0, 1);
  }

  void RequestVoltage(RequestVoltageRequestView request,
                      RequestVoltageCompleter::Sync& completer) override {
    voltage_ = request->voltage;
    if (use_mocks_) {
      auto result = mock_request_voltage_.Call(request->voltage);
      zx_status_t status = std::get<0>(result);
      if (status == ZX_OK) {
        completer.ReplySuccess(std::get<1>(result));
        return;
      }

      completer.ReplyError(status);
      return;
    }

    completer.ReplySuccess(voltage_);
  }

  void GetCurrentVoltage(GetCurrentVoltageRequestView request,
                         GetCurrentVoltageCompleter::Sync& completer) override {
    if (use_mocks_) {
      auto result = mock_get_current_voltage_.Call(request->index);
      zx_status_t status = std::get<0>(result);
      if (status == ZX_OK) {
        completer.ReplySuccess(std::get<1>(result));
        return;
      }

      completer.ReplyError(status);
      return;
    }

    completer.ReplySuccess(voltage_);
  }

  void WritePmicCtrlReg(WritePmicCtrlRegRequestView request,
                        WritePmicCtrlRegCompleter::Sync& completer) override {
    if (use_mocks_) {
      auto result = mock_write_pmic_ctrl_reg_.Call(request->reg_addr, request->value);
      zx_status_t status = std::get<0>(result);
      if (status == ZX_OK) {
        completer.ReplySuccess();
        return;
      }

      completer.ReplyError(status);
      return;
    }

    completer.ReplySuccess();
  }
  void ReadPmicCtrlReg(ReadPmicCtrlRegRequestView request,
                       ReadPmicCtrlRegCompleter::Sync& completer) override {
    if (use_mocks_) {
      auto result = mock_read_pmic_ctrl_reg_.Call(request->reg_addr);
      zx_status_t status = std::get<0>(result);
      if (status == ZX_OK) {
        completer.ReplySuccess(std::get<1>(result));
        return;
      }

      completer.ReplyError(status);
      return;
    }

    completer.ReplySuccess(1);
  }

  void ExpectRegisterPowerDomain(zx_status_t out_s, uint32_t min_needed_voltage,
                                 uint32_t max_supported_voltage) {
    ZX_ASSERT_MSG(use_mocks_, "use_mocks needs to be true to use Expect functions.");
    mock_register_power_domain_.ExpectCall({out_s}, min_needed_voltage, max_supported_voltage);
  }

  void ExpectUnregisterPowerDomain(zx_status_t out_s) {
    ZX_ASSERT_MSG(use_mocks_, "use_mocks needs to be true to use Expect functions.");
    mock_unregister_power_domain_.ExpectCall({out_s});
  }

  void ExpectGetSupportedVoltageRange(zx_status_t out_s, uint32_t out_min, uint32_t out_max) {
    ZX_ASSERT_MSG(use_mocks_, "use_mocks needs to be true to use Expect functions.");
    mock_get_supported_voltage_range_.ExpectCall({out_s, out_min, out_max});
  }

  void ExpectRequestVoltage(zx_status_t out_s, uint32_t voltage, uint32_t out_actual_voltage) {
    ZX_ASSERT_MSG(use_mocks_, "use_mocks needs to be true to use Expect functions.");
    mock_request_voltage_.ExpectCall({out_s, out_actual_voltage}, voltage);
  }

  void ExpectGetCurrentVoltage(zx_status_t out_s, uint32_t index, uint32_t out_current_voltage) {
    ZX_ASSERT_MSG(use_mocks_, "use_mocks needs to be true to use Expect functions.");
    mock_get_current_voltage_.ExpectCall({out_s, out_current_voltage}, index);
  }

  void ExpectGetPowerDomainStatus(zx_status_t out_s,
                                  fuchsia_hardware_power::PowerDomainStatus out_status) {
    ZX_ASSERT_MSG(use_mocks_, "use_mocks needs to be true to use Expect functions.");
    mock_get_power_domain_status_.ExpectCall({out_s, out_status});
  }

  void ExpectWritePmicCtrlReg(zx_status_t out_s, uint32_t reg_addr, uint32_t value) {
    ZX_ASSERT_MSG(use_mocks_, "use_mocks needs to be true to use Expect functions.");
    mock_write_pmic_ctrl_reg_.ExpectCall({out_s}, reg_addr, value);
  }

  void ExpectReadPmicCtrlReg(zx_status_t out_s, uint32_t reg_addr, uint32_t out_value) {
    ZX_ASSERT_MSG(use_mocks_, "use_mocks needs to be true to use Expect functions.");
    mock_read_pmic_ctrl_reg_.ExpectCall({out_s, out_value}, reg_addr);
  }

  void VerifyAndClear() {
    ZX_ASSERT_MSG(use_mocks_, "use_mocks needs to be true to use VerifyAndClear.");
    mock_register_power_domain_.VerifyAndClear();
    mock_unregister_power_domain_.VerifyAndClear();
    mock_get_supported_voltage_range_.VerifyAndClear();
    mock_request_voltage_.VerifyAndClear();
    mock_get_current_voltage_.VerifyAndClear();
    mock_get_power_domain_status_.VerifyAndClear();
    mock_write_pmic_ctrl_reg_.VerifyAndClear();
    mock_read_pmic_ctrl_reg_.VerifyAndClear();
  }

  uint32_t voltage() const { return voltage_; }

 private:
  bool use_mocks_;
  uint32_t voltage_ = 0;
  fidl::ServerBindingGroup<fuchsia_hardware_power::Device> binding_group_;

  mock_function::MockFunction<std::tuple<zx_status_t>, uint32_t, uint32_t>
      mock_register_power_domain_;
  mock_function::MockFunction<std::tuple<zx_status_t>> mock_unregister_power_domain_;
  mock_function::MockFunction<std::tuple<zx_status_t, uint32_t, uint32_t>>
      mock_get_supported_voltage_range_;
  mock_function::MockFunction<std::tuple<zx_status_t, uint32_t>, uint32_t> mock_request_voltage_;
  mock_function::MockFunction<std::tuple<zx_status_t, uint32_t>, uint32_t>
      mock_get_current_voltage_;
  mock_function::MockFunction<std::tuple<zx_status_t, fuchsia_hardware_power::PowerDomainStatus>>
      mock_get_power_domain_status_;
  mock_function::MockFunction<std::tuple<zx_status_t>, uint32_t, uint32_t>
      mock_write_pmic_ctrl_reg_;
  mock_function::MockFunction<std::tuple<zx_status_t, uint32_t>, uint32_t> mock_read_pmic_ctrl_reg_;
};

class FakeClockDevice : public ddk::ClockProtocol<FakeClockDevice, ddk::base_protocol> {
 public:
  FakeClockDevice() : proto_({&clock_protocol_ops_, this}) {}

  zx_status_t ClockEnable() { return ZX_OK; }
  zx_status_t ClockDisable() { return ZX_OK; }
  zx_status_t ClockIsEnabled(bool* out_enabled) { return ZX_OK; }

  zx_status_t ClockSetRate(uint64_t hz) { return ZX_OK; }
  zx_status_t ClockQuerySupportedRate(uint64_t max_rate, uint64_t* out_max_supported_rate) {
    return ZX_OK;
  }
  zx_status_t ClockGetRate(uint64_t* out_current_rate) { return ZX_OK; }

  zx_status_t ClockSetInput(uint32_t idx) { return ZX_OK; }
  zx_status_t ClockGetNumInputs(uint32_t* out) { return ZX_OK; }
  zx_status_t ClockGetInput(uint32_t* out) { return ZX_OK; }

  const clock_protocol_t* proto() const { return &proto_; }

 private:
  clock_protocol_t proto_;
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  TestPowerDevice power_server{false};
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

class AmlCpuBindingTest : public zxtest::Test {
 public:
  AmlCpuBindingTest() {
    ASSERT_OK(incoming_loop_.StartThread("incoming-ns-thread"));
    std::map<uint32_t, fake_pdev::Mmio> mmios;
    mmios[0] = mmio_.mmio();
    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(outgoing_endpoints);
    zx::result power_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(power_endpoints);
    incoming_.SyncCall(
        [mmios = std::move(mmios), server = std::move(outgoing_endpoints->server),
         power_server = std::move(power_endpoints->server)](IncomingNamespace* infra) mutable {
          infra->pdev_server.SetConfig(fake_pdev::FakePDevFidl::Config{
              .mmios = std::move(mmios),
              .device_info =
                  pdev_device_info_t{
                      .pid = PDEV_PID_ASTRO,
                  },
          });

          ASSERT_OK(infra->outgoing.AddService<fuchsia_hardware_platform_device::Service>(
              infra->pdev_server.GetInstanceHandler()));
          ASSERT_OK(infra->outgoing.AddService<fuchsia_hardware_power::Service>(
              infra->power_server.GetInstanceHandler()));

          ASSERT_OK(infra->outgoing.Serve(std::move(server)));
          ASSERT_OK(infra->outgoing.Serve(std::move(power_server)));
        });
    ASSERT_NO_FATAL_FAILURE();

    root_ = MockDevice::FakeRootParent();
    root_->SetMetadata(DEVICE_METADATA_AML_PERF_DOMAINS, &kPerfDomains, sizeof(kPerfDomains));

    root_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                          std::move(outgoing_endpoints->client), "pdev");
    root_->AddFidlService(fuchsia_hardware_power::Service::Name, std::move(power_endpoints->client),
                          "power-01");

    root_->AddProtocol(ZX_PROTOCOL_CLOCK, clk0_.proto()->ops, clk0_.proto()->ctx,
                       "clock-pll-div16-01");
    root_->AddProtocol(ZX_PROTOCOL_CLOCK, clk1_.proto()->ops, clk1_.proto()->ctx,
                       "clock-cpu-div16-01");
    root_->AddProtocol(ZX_PROTOCOL_CLOCK, clk2_.proto()->ops, clk2_.proto()->ctx,
                       "clock-cpu-scaler-01");

    root_->SetMetadata(DEVICE_METADATA_AML_PERF_DOMAINS, &kPerfDomains, sizeof(kPerfDomains));
  }

 protected:
  std::shared_ptr<MockDevice> root_;

  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};

  FakeMmio mmio_;

  FakeClockDevice clk0_;
  FakeClockDevice clk1_;
  FakeClockDevice clk2_;
};

TEST_F(AmlCpuBindingTest, TrivialBinding) {
  root_->SetMetadata(DEVICE_METADATA_AML_OP_POINTS, &kOperatingPointsMetadata,
                     sizeof(kOperatingPointsMetadata));

  ASSERT_OK(AmlCpu::Create(nullptr, root_.get()));
  ASSERT_EQ(root_->child_count(), 1);
}

TEST_F(AmlCpuBindingTest, UnorderedOperatingPoints) {
  // AML CPU's bind hook expects that all operating points are strictly
  // ordered and it should handle the situation where there are duplicate
  // frequencies.
  constexpr amlogic_cpu::operating_point_t kOperatingPointsMetadata[] = {
      {.freq_hz = MHZ(1), .volt_uv = 200'000, .pd_id = kPdArmA53},
      {.freq_hz = MHZ(1), .volt_uv = 100'000, .pd_id = kPdArmA53},
      {.freq_hz = MHZ(1), .volt_uv = 300'000, .pd_id = kPdArmA53},
  };

  root_->SetMetadata(DEVICE_METADATA_AML_OP_POINTS, &kOperatingPointsMetadata,
                     sizeof(kOperatingPointsMetadata));

  ASSERT_OK(AmlCpu::Create(nullptr, root_.get()));
  ASSERT_EQ(root_->child_count(), 1);

  MockDevice* child = root_->GetLatestChild();
  AmlCpu* dev = child->GetDeviceContext<AmlCpu>();

  uint32_t out_state;
  EXPECT_OK(dev->DdkSetPerformanceState(0, &out_state));
  EXPECT_EQ(out_state, 0);

  incoming_.SyncCall([](IncomingNamespace* infra) {
    uint32_t voltage = infra->power_server.voltage();
    EXPECT_EQ(voltage, 300'000);
  });
}

class TestPowerDeviceWrapper {
 public:
  TestPowerDeviceWrapper() { loop_.StartThread("power-loop"); }

  fidl::ClientEnd<fuchsia_hardware_power::Device> Connect() {
    return power_device_.SyncCall(&TestPowerDevice::Connect);
  }

  void ExpectRegisterPowerDomain(zx_status_t out_s, uint32_t min_needed_voltage,
                                 uint32_t max_supported_voltage) {
    EXPECT_NO_FAILURES(power_device_.SyncCall(&TestPowerDevice::ExpectRegisterPowerDomain, out_s,
                                              min_needed_voltage, max_supported_voltage));
  }

  void ExpectUnregisterPowerDomain(zx_status_t out_s) {
    EXPECT_NO_FAILURES(
        power_device_.SyncCall(&TestPowerDevice::ExpectUnregisterPowerDomain, out_s));
  }

  void ExpectGetSupportedVoltageRange(zx_status_t out_s, uint32_t out_min, uint32_t out_max) {
    EXPECT_NO_FAILURES(power_device_.SyncCall(&TestPowerDevice::ExpectGetSupportedVoltageRange,
                                              out_s, out_min, out_max));
  }

  void ExpectRequestVoltage(zx_status_t out_s, uint32_t voltage, uint32_t out_actual_voltage) {
    EXPECT_NO_FAILURES(power_device_.SyncCall(&TestPowerDevice::ExpectRequestVoltage, out_s,
                                              voltage, out_actual_voltage));
  }

  void ExpectGetCurrentVoltage(zx_status_t out_s, uint32_t index, uint32_t out_current_voltage) {
    EXPECT_NO_FAILURES(power_device_.SyncCall(&TestPowerDevice::ExpectGetCurrentVoltage, out_s,
                                              index, out_current_voltage));
  }

  void ExpectGetPowerDomainStatus(zx_status_t out_s,
                                  fuchsia_hardware_power::PowerDomainStatus out_status) {
    EXPECT_NO_FAILURES(
        power_device_.SyncCall(&TestPowerDevice::ExpectGetPowerDomainStatus, out_s, out_status));
  }

  void ExpectWritePmicCtrlReg(zx_status_t out_s, uint32_t reg_addr, uint32_t value) {
    EXPECT_NO_FAILURES(
        power_device_.SyncCall(&TestPowerDevice::ExpectWritePmicCtrlReg, out_s, reg_addr, value));
  }

  void ExpectReadPmicCtrlReg(zx_status_t out_s, uint32_t reg_addr, uint32_t out_value) {
    EXPECT_NO_FAILURES(power_device_.SyncCall(&TestPowerDevice::ExpectReadPmicCtrlReg, out_s,
                                              reg_addr, out_value));
  }

  void VerifyAndClear() {
    EXPECT_NO_FAILURES(power_device_.SyncCall(&TestPowerDevice::VerifyAndClear));
  }

 private:
  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<TestPowerDevice> power_device_{loop_.dispatcher(),
                                                                     std::in_place, true};
};

class AmlCpuTest : public AmlCpu {
 public:
  AmlCpuTest(const ddk::ClockProtocolClient&& plldiv16, const ddk::ClockProtocolClient&& cpudiv16,
             const ddk::ClockProtocolClient&& cpuscaler,
             fidl::ClientEnd<fuchsia_hardware_power::Device> pwr,
             const std::vector<operating_point_t> operating_points, const uint32_t core_count)
      : AmlCpu(nullptr, std::move(plldiv16), std::move(cpudiv16), std::move(cpuscaler),
               std::move(pwr), operating_points, core_count) {}

  zx::vmo inspect_vmo() { return inspector_.DuplicateVmo(); }
};

class AmlCpuTestFixture : public InspectTestHelper, public zxtest::Test {
 public:
  AmlCpuTestFixture()
      : dut_(ddk::ClockProtocolClient(pll_clock_.GetProto()),
             ddk::ClockProtocolClient(cpu_clock_.GetProto()),
             ddk::ClockProtocolClient(scaler_clock_.GetProto()), power_.Connect(),
             kTestOperatingPoints, kTestCoreCount),
        loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        operating_points_(kTestOperatingPoints) {}

  void SetUp() override {
    // Notes on AmlCpu Initialization:
    //  + Should enable the CPU and PLL clocks.
    //  + Should initially assume that the device is in it's lowest performance state.
    //  + Should configure the device to it's highest performance state.
    pll_clock_.ExpectEnable(ZX_OK);
    cpu_clock_.ExpectEnable(ZX_OK);

    const operating_point_t& slowest = operating_points_.back();
    const operating_point_t& fastest = operating_points_.front();

    // The DUT should initialize.
    power_.ExpectGetSupportedVoltageRange(ZX_OK, slowest.volt_uv, fastest.volt_uv);
    power_.ExpectRegisterPowerDomain(ZX_OK, slowest.volt_uv, fastest.volt_uv);

    // The DUT scales up to the fastest available pstate.
    power_.ExpectRequestVoltage(ZX_OK, fastest.volt_uv, fastest.volt_uv);
    scaler_clock_.ExpectSetRate(ZX_OK, fastest.freq_hz);

    ASSERT_OK(dut_.Init());

    auto endpoints = fidl::CreateEndpoints<fuchsia_cpuctrl::Device>();
    fidl::BindServer(loop_.dispatcher(), std::move(endpoints->server), &dut_);
    loop_.StartThread("aml-cpu-test-thread");

    cpu_client_.Bind(std::move(endpoints->client));
  }

 protected:
  void VerifyAll() {
    ASSERT_NO_FATAL_FAILURE(pll_clock_.VerifyAndClear());
    ASSERT_NO_FATAL_FAILURE(cpu_clock_.VerifyAndClear());
    ASSERT_NO_FATAL_FAILURE(scaler_clock_.VerifyAndClear());
    ASSERT_NO_FATAL_FAILURE(power_.VerifyAndClear());
  }

  ddk::MockClock pll_clock_;
  ddk::MockClock cpu_clock_;
  ddk::MockClock scaler_clock_;
  TestPowerDeviceWrapper power_;

  AmlCpuTest dut_;
  CpuCtrlClient cpu_client_;

  async::Loop loop_;

  const std::vector<operating_point_t> operating_points_;
};

TEST_F(AmlCpuTestFixture, TestGetPerformanceStateInfo) {
  // Make sure that we can get information about all the supported pstates.
  for (size_t i = 0; i < kTestOperatingPoints.size(); i++) {
    const uint32_t pstate = static_cast<uint32_t>(i);
    auto pstateInfo = cpu_client_->GetPerformanceStateInfo(pstate);

    // First, make sure there were no transport errors.
    ASSERT_OK(pstateInfo.status());

    // Then make sure that the driver accepted the call.
    ASSERT_FALSE(pstateInfo->is_error());

    // Then make sure that we're getting the expected frequency and voltage values.
    EXPECT_EQ(pstateInfo->value()->info.frequency_hz, kTestOperatingPoints[i].freq_hz);
    EXPECT_EQ(pstateInfo->value()->info.voltage_uv, kTestOperatingPoints[i].volt_uv);
  }

  // Make sure that we can't get any information about pstates that don't
  // exist.
  for (size_t i = kTestOperatingPoints.size(); i < kMaxDevicePerformanceStates; i++) {
    const uint32_t pstate = static_cast<uint32_t>(i);
    auto pstateInfo = cpu_client_->GetPerformanceStateInfo(pstate);

    // Even if it's an unsupported pstate, we still expect the transport to
    // deliver the message successfully.
    ASSERT_OK(pstateInfo.status());

    // Make sure that the driver returns an error, however.
    EXPECT_TRUE(pstateInfo->is_error());
  }

  ASSERT_NO_FATAL_FAILURE(VerifyAll());
}

TEST_F(AmlCpuTestFixture, TestSetPerformanceState) {
  // Scale to the lowest performance state.
  const uint32_t min_pstate_index = static_cast<uint32_t>(kTestOperatingPoints.size() - 1);
  const operating_point_t& min_pstate = kTestOperatingPoints[min_pstate_index];

  scaler_clock_.ExpectSetRate(ZX_OK, min_pstate.freq_hz);
  power_.ExpectRequestVoltage(ZX_OK, min_pstate.volt_uv, min_pstate.volt_uv);

  uint32_t out_state = UINT32_MAX;
  zx_status_t result = dut_.DdkSetPerformanceState(min_pstate_index, &out_state);
  EXPECT_OK(result);
  EXPECT_EQ(out_state, min_pstate_index);

  // Scale to the highest performance state.
  const uint32_t max_pstate_index = 0;
  const operating_point_t& max_pstate = kTestOperatingPoints[max_pstate_index];

  scaler_clock_.ExpectSetRate(ZX_OK, max_pstate.freq_hz);
  power_.ExpectRequestVoltage(ZX_OK, max_pstate.volt_uv, max_pstate.volt_uv);

  out_state = UINT32_MAX;
  result = dut_.DdkSetPerformanceState(max_pstate_index, &out_state);
  EXPECT_OK(result);
  EXPECT_EQ(out_state, max_pstate_index);

  // Set to the pstate that we're already at and make sure that it's a no-op.
  result = dut_.DdkSetPerformanceState(max_pstate_index, &out_state);
  EXPECT_OK(result);
  EXPECT_EQ(out_state, max_pstate_index);

  ASSERT_NO_FATAL_FAILURE(VerifyAll());
}

TEST_F(AmlCpuTestFixture, TestSetCpuInfo) {
  uint32_t test_cpu_version = 0x28200b02;
  dut_.SetCpuInfo(test_cpu_version);
  ASSERT_NO_FATAL_FAILURE(ReadInspect(dut_.inspect_vmo()));
  auto* cpu_info = hierarchy().GetByPath({"cpu_info_service"});
  ASSERT_TRUE(cpu_info);

  // cpu_major_revision : 40
  ASSERT_NO_FATAL_FAILURE(
      CheckProperty(cpu_info->node(), "cpu_major_revision", inspect::UintPropertyValue(40)));
  // cpu_minor_revision : 11
  ASSERT_NO_FATAL_FAILURE(
      CheckProperty(cpu_info->node(), "cpu_minor_revision", inspect::UintPropertyValue(11)));
  // cpu_package_id : 2
  ASSERT_NO_FATAL_FAILURE(
      CheckProperty(cpu_info->node(), "cpu_package_id", inspect::UintPropertyValue(2)));
}

TEST_F(AmlCpuTestFixture, TestGetLogicalCoreCount) {
  auto coreCountResp = cpu_client_->GetNumLogicalCores();

  ASSERT_OK(coreCountResp.status());

  EXPECT_EQ(coreCountResp.value().count, kTestCoreCount);
}

}  // namespace amlogic_cpu
