// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.clock/cpp/wire_test_base.h>
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
#include <gtest/gtest.h>
#include <sdk/lib/inspect/testing/cpp/inspect.h>

#include "lib/inspect/cpp/reader.h"
#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/cpu/drivers/aml-cpu/aml-cpu-v1.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/lib/testing/predicates/status.h"

namespace amlogic_cpu {

using CpuCtrlClient = fidl::WireSyncClient<fuchsia_hardware_cpu_ctrl::Device>;

#define MHZ(x) ((x) * 1000000)

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

class TestClockDevice : public fidl::testing::WireTestBase<fuchsia_hardware_clock::Clock> {
 public:
  fuchsia_hardware_clock::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_clock::Service::InstanceHandler({
        .clock = binding_group_.CreateHandler(this, async_get_default_dispatcher(),
                                              fidl::kIgnoreBindingClosure),
    });
  }

  void Enable(EnableCompleter::Sync& completer) override {
    enabled_ = true;
    completer.ReplySuccess();
  }

  void Disable(DisableCompleter::Sync& completer) override {
    enabled_ = false;
    completer.ReplySuccess();
  }

  void IsEnabled(IsEnabledCompleter::Sync& completer) override { completer.ReplySuccess(enabled_); }

  void SetRate(SetRateRequestView request, SetRateCompleter::Sync& completer) override {
    rate_.emplace(request->hz);
    completer.ReplySuccess();
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  fidl::ClientEnd<fuchsia_hardware_clock::Clock> Connect() {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_clock::Clock>();
    EXPECT_OK(endpoints.status_value());
    binding_group_.AddBinding(async_get_default_dispatcher(), std::move(endpoints->server), this,
                              fidl::kIgnoreBindingClosure);
    return std::move(endpoints->client);
  }

  bool enabled() const { return enabled_; }
  std::optional<uint32_t> rate() const { return rate_; }

 private:
  bool enabled_ = false;
  std::optional<uint32_t> rate_;
  fidl::ServerBindingGroup<fuchsia_hardware_clock::Clock> binding_group_;
};

class TestPowerDevice : public fidl::WireServer<fuchsia_hardware_power::Device> {
 public:
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
    EXPECT_OK(endpoints.status_value());
    ConnectRequest(std::move(endpoints->server));
    return std::move(endpoints->client);
  }

  void RegisterPowerDomain(RegisterPowerDomainRequestView request,
                           RegisterPowerDomainCompleter::Sync& completer) override {
    min_needed_voltage_ = request->min_needed_voltage;
    max_supported_voltage_ = request->max_supported_voltage;
    completer.ReplySuccess();
  }

  void UnregisterPowerDomain(UnregisterPowerDomainCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }
  void GetPowerDomainStatus(GetPowerDomainStatusCompleter::Sync& completer) override {
    completer.ReplySuccess(fuchsia_hardware_power::wire::PowerDomainStatus::kEnabled);
  }
  void GetSupportedVoltageRange(GetSupportedVoltageRangeCompleter::Sync& completer) override {
    completer.ReplySuccess(min_voltage_, max_voltage_);
  }

  void RequestVoltage(RequestVoltageRequestView request,
                      RequestVoltageCompleter::Sync& completer) override {
    voltage_ = request->voltage;
    completer.ReplySuccess(voltage_);
  }

  void GetCurrentVoltage(GetCurrentVoltageRequestView request,
                         GetCurrentVoltageCompleter::Sync& completer) override {
    completer.ReplySuccess(voltage_);
  }

  void WritePmicCtrlReg(WritePmicCtrlRegRequestView request,
                        WritePmicCtrlRegCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }
  void ReadPmicCtrlReg(ReadPmicCtrlRegRequestView request,
                       ReadPmicCtrlRegCompleter::Sync& completer) override {
    completer.ReplySuccess(1);
  }

  void SetSupportedVoltageRange(uint32_t min_voltage, uint32_t max_voltage) {
    min_voltage_ = min_voltage;
    max_voltage_ = max_voltage;
  }

  void SetVoltage(uint32_t voltage) { voltage_ = voltage; }

  uint32_t voltage() const { return voltage_; }
  uint32_t min_needed_voltage() const { return min_needed_voltage_; }
  uint32_t max_supported_voltage() const { return max_supported_voltage_; }

 private:
  uint32_t voltage_ = 0;
  uint32_t min_voltage_ = 0;
  uint32_t max_voltage_ = 0;
  uint32_t min_needed_voltage_ = 0;
  uint32_t max_supported_voltage_ = 0;
  fidl::ServerBindingGroup<fuchsia_hardware_power::Device> binding_group_;
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  component::OutgoingDirectory pdev_outgoing{async_get_default_dispatcher()};
  TestPowerDevice power_server;
  component::OutgoingDirectory power_outgoing{async_get_default_dispatcher()};
  TestClockDevice clock_pll_div16_server;
  component::OutgoingDirectory clock_pll_div16_outgoing{async_get_default_dispatcher()};
  TestClockDevice clock_cpu_div16_server;
  component::OutgoingDirectory clock_cpu_div16_outgoing{async_get_default_dispatcher()};
  TestClockDevice clock_cpu_scaler_server;
  component::OutgoingDirectory clock_cpu_scaler_outgoing{async_get_default_dispatcher()};
};

class AmlCpuBindingTest : public testing::Test {
 public:
  void SetUp() override {
    ASSERT_OK(incoming_loop_.StartThread("incoming-ns-thread"));
    std::map<uint32_t, fake_pdev::Mmio> mmios;
    mmios[0] = mmio_.mmio();

    zx::result pdev_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(pdev_endpoints.status_value());
    zx::result power_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(power_endpoints.status_value());
    zx::result clock_pll_div16_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(clock_pll_div16_endpoints.status_value());
    zx::result clock_cpu_div16_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(clock_cpu_div16_endpoints.status_value());
    zx::result clock_cpu_scaler_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(clock_cpu_scaler_endpoints.status_value());
    incoming_.SyncCall([mmios = std::move(mmios), pdev_server = std::move(pdev_endpoints->server),
                        power_server = std::move(power_endpoints->server),
                        clock_pll_div16_server = std::move(clock_pll_div16_endpoints->server),
                        clock_cpu_div16_server = std::move(clock_cpu_div16_endpoints->server),
                        clock_cpu_scaler_server = std::move(clock_cpu_scaler_endpoints->server)](
                           IncomingNamespace* infra) mutable {
      infra->pdev_server.SetConfig(fake_pdev::FakePDevFidl::Config{
          .mmios = std::move(mmios),
          .device_info =
              pdev_device_info_t{
                  .pid = PDEV_PID_ASTRO,
              },
      });
      ASSERT_OK(infra->pdev_outgoing
                    .AddService<fuchsia_hardware_platform_device::Service>(
                        infra->pdev_server.GetInstanceHandler())
                    .status_value());
      ASSERT_OK(infra->pdev_outgoing.Serve(std::move(pdev_server)).status_value());

      infra->power_server.SetVoltage(0);
      infra->power_server.SetSupportedVoltageRange(0, 0);
      ASSERT_OK(
          infra->power_outgoing
              .AddService<fuchsia_hardware_power::Service>(infra->power_server.GetInstanceHandler())
              .status_value());
      ASSERT_OK(infra->power_outgoing.Serve(std::move(power_server)).status_value());

      ASSERT_OK(infra->clock_pll_div16_outgoing
                    .AddService<fuchsia_hardware_clock::Service>(
                        infra->clock_pll_div16_server.GetInstanceHandler())
                    .status_value());
      ASSERT_OK(
          infra->clock_pll_div16_outgoing.Serve(std::move(clock_pll_div16_server)).status_value());

      ASSERT_OK(infra->clock_cpu_div16_outgoing
                    .AddService<fuchsia_hardware_clock::Service>(
                        infra->clock_cpu_div16_server.GetInstanceHandler())
                    .status_value());
      ASSERT_OK(
          infra->clock_cpu_div16_outgoing.Serve(std::move(clock_cpu_div16_server)).status_value());

      ASSERT_OK(infra->clock_cpu_scaler_outgoing
                    .AddService<fuchsia_hardware_clock::Service>(
                        infra->clock_cpu_scaler_server.GetInstanceHandler())
                    .status_value());
      ASSERT_OK(infra->clock_cpu_scaler_outgoing.Serve(std::move(clock_cpu_scaler_server))
                    .status_value());
    });

    ASSERT_NO_FATAL_FAILURE();

    root_ = MockDevice::FakeRootParent();
    root_->SetMetadata(DEVICE_METADATA_AML_PERF_DOMAINS, &kPerfDomains, sizeof(kPerfDomains));

    root_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                          std::move(pdev_endpoints->client), "pdev");
    root_->AddFidlService(fuchsia_hardware_power::Service::Name, std::move(power_endpoints->client),
                          "power-01");
    root_->AddFidlService(fuchsia_hardware_clock::Service::Name,
                          std::move(clock_pll_div16_endpoints->client), "clock-pll-div16-01");
    root_->AddFidlService(fuchsia_hardware_clock::Service::Name,
                          std::move(clock_cpu_div16_endpoints->client), "clock-cpu-div16-01");
    root_->AddFidlService(fuchsia_hardware_clock::Service::Name,
                          std::move(clock_cpu_scaler_endpoints->client), "clock-cpu-scaler-01");

    root_->SetMetadata(DEVICE_METADATA_AML_PERF_DOMAINS, &kPerfDomains, sizeof(kPerfDomains));
  }

 protected:
  std::shared_ptr<MockDevice> root_;

  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};

  FakeMmio mmio_;
};

TEST_F(AmlCpuBindingTest, TrivialBinding) {
  root_->SetMetadata(DEVICE_METADATA_AML_OP_POINTS, &kOperatingPointsMetadata,
                     sizeof(kOperatingPointsMetadata));

  ASSERT_OK(CreateV1Domains(nullptr, root_.get()));
  ASSERT_EQ(root_->child_count(), 1ul);
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

  ASSERT_OK(CreateV1Domains(nullptr, root_.get()));
  ASSERT_EQ(root_->child_count(), 1ul);

  MockDevice* child = root_->GetLatestChild();
  AmlCpuV1* dev = child->GetDeviceContext<AmlCpuV1>();

  uint32_t out_opp;
  EXPECT_OK(dev->aml_cpu_for_testing().SetCurrentOperatingPointInternal(0, &out_opp));
  EXPECT_EQ(out_opp, 0ul);

  incoming_.SyncCall([](IncomingNamespace* infra) {
    uint32_t voltage = infra->power_server.voltage();
    EXPECT_EQ(voltage, 300'000u);
  });
}

class TestClockDeviceWrapper {
 public:
  explicit TestClockDeviceWrapper(const char* thread_name) { loop_.StartThread(thread_name); }

  fidl::ClientEnd<fuchsia_hardware_clock::Clock> Connect() {
    return clock_device_.SyncCall(&TestClockDevice::Connect);
  }

  bool enabled() { return clock_device_.SyncCall(&TestClockDevice::enabled); }
  std::optional<uint32_t> rate() { return clock_device_.SyncCall(&TestClockDevice::rate); }

 private:
  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<TestClockDevice> clock_device_{loop_.dispatcher(),
                                                                     std::in_place};
};

class TestPowerDeviceWrapper {
 public:
  TestPowerDeviceWrapper() { loop_.StartThread("power-loop"); }

  fidl::ClientEnd<fuchsia_hardware_power::Device> Connect() {
    return power_device_.SyncCall(&TestPowerDevice::Connect);
  }

  void SetSupportedVoltageRange(uint32_t min_voltage, uint32_t max_voltage) {
    power_device_.SyncCall(&TestPowerDevice::SetSupportedVoltageRange, min_voltage, max_voltage);
  }

  void SetVoltage(uint32_t voltage) {
    power_device_.SyncCall(&TestPowerDevice::SetVoltage, voltage);
  }

  uint32_t voltage() { return power_device_.SyncCall(&TestPowerDevice::voltage); }
  uint32_t min_needed_voltage() {
    return power_device_.SyncCall(&TestPowerDevice::min_needed_voltage);
  }
  uint32_t max_supported_voltage() {
    return power_device_.SyncCall(&TestPowerDevice::max_supported_voltage);
  }

 private:
  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<TestPowerDevice> power_device_{loop_.dispatcher(),
                                                                     std::in_place};
};

class AmlCpuTest : public AmlCpuV1 {
 public:
  AmlCpuTest(const std::vector<operating_point_t>& operating_points, const uint32_t core_count)
      : AmlCpuV1(
            nullptr, operating_points,
            perf_domain_t{
                .id = 0, .core_count = core_count, .relative_performance = 0, .name = "testpd"}) {}

  zx::vmo inspect_vmo() { return aml_cpu_for_testing().inspector_.DuplicateVmo(); }
};

class AmlCpuTestFixture : public testing::Test {
 public:
  AmlCpuTestFixture()
      : dut_(kTestOperatingPoints, kTestCoreCount),
        loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        operating_points_(kTestOperatingPoints) {}

  void SetUp() override {
    // Notes on AmlCpu Initialization:
    //  + Should enable the CPU and PLL clocks.
    //  + Should initially assume that the device is in it's lowest opp.
    //  + Should configure the device to it's highest opp.

    const operating_point_t& slowest = operating_points_.back();
    const operating_point_t& fastest = operating_points_.front();

    // The DUT should initialize.
    power_.SetSupportedVoltageRange(slowest.volt_uv, fastest.volt_uv);

    // The DUT scales up to the fastest available opp.
    power_.SetVoltage(fastest.volt_uv);

    ASSERT_OK(dut_.aml_cpu_for_testing().Init(pll_clock_.Connect(), cpu_clock_.Connect(),
                                              scaler_clock_.Connect(), power_.Connect()));

    ASSERT_EQ(power_.min_needed_voltage(), slowest.volt_uv);
    ASSERT_EQ(power_.max_supported_voltage(), fastest.volt_uv);

    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_cpu_ctrl::Device>();
    fidl::BindServer(loop_.dispatcher(), std::move(endpoints->server), &dut_);
    loop_.StartThread("aml-cpu-test-thread");

    cpu_client_.Bind(std::move(endpoints->client));
  }

 protected:
  TestClockDeviceWrapper pll_clock_{"pll-clock-loop"};
  TestClockDeviceWrapper cpu_clock_{"cpu-clock-loop"};
  TestClockDeviceWrapper scaler_clock_{"scaler-clock-loop"};
  TestPowerDeviceWrapper power_;

  AmlCpuTest dut_;
  CpuCtrlClient cpu_client_;

  async::Loop loop_;

  const std::vector<operating_point_t> operating_points_;
};

TEST_F(AmlCpuTestFixture, TestGetOperatingPointInfo) {
  auto opp_size = kTestOperatingPoints.size();
  // Make sure that we can get information about all the supported opps.
  for (size_t i = 0; i < opp_size; i++) {
    const uint32_t opp = static_cast<uint32_t>(i);
    auto oppInfo = cpu_client_->GetOperatingPointInfo(opp);

    // First, make sure there were no transport errors.
    ASSERT_OK(oppInfo.status());

    // Then make sure that the driver accepted the call.
    ASSERT_FALSE(oppInfo->is_error());

    // Then make sure that we're getting the expected frequency and voltage values.
    EXPECT_EQ(oppInfo->value()->info.frequency_hz, kTestOperatingPoints[i].freq_hz);
    EXPECT_EQ(oppInfo->value()->info.voltage_uv, kTestOperatingPoints[i].volt_uv);
  }

  // Make sure that we can't get any information about opps that don't
  // exist.
  for (size_t i = opp_size; i < opp_size + 10; i++) {
    const uint32_t opp = static_cast<uint32_t>(i);
    auto oppInfo = cpu_client_->GetOperatingPointInfo(opp);

    // Even if it's an unsupported opp, we still expect the transport to
    // deliver the message successfully.
    ASSERT_OK(oppInfo.status());

    // Make sure that the driver returns an error, however.
    EXPECT_TRUE(oppInfo->is_error());
  }
}

TEST_F(AmlCpuTestFixture, TestSetCurrentOperatingPoint) {
  // Scale to the lowest opp.
  const uint32_t min_opp_index = static_cast<uint32_t>(kTestOperatingPoints.size() - 1);
  const operating_point_t& min_opp = kTestOperatingPoints[min_opp_index];

  power_.SetVoltage(min_opp.volt_uv);

  auto min_result = cpu_client_->SetCurrentOperatingPoint(min_opp_index);
  EXPECT_OK(min_result.status());
  EXPECT_TRUE(min_result->is_ok());
  EXPECT_EQ(min_result->value()->out_opp, min_opp_index);
  auto rate = scaler_clock_.rate();
  ASSERT_TRUE(rate.has_value());
  ASSERT_EQ(rate.value(), min_opp.freq_hz);

  // Check that we get the same value back from GetCurrentOperatingPoint
  auto min_result_again = cpu_client_->GetCurrentOperatingPoint();
  EXPECT_OK(min_result_again.status());
  EXPECT_EQ(min_result_again.value().out_opp, min_opp_index);

  // Scale to the highest opp.
  const uint32_t max_opp_index = 0;
  const operating_point_t& max_opp = kTestOperatingPoints[max_opp_index];

  power_.SetVoltage(max_opp.volt_uv);

  auto max_result = cpu_client_->SetCurrentOperatingPoint(max_opp_index);
  EXPECT_OK(max_result.status());
  EXPECT_TRUE(max_result->is_ok());
  EXPECT_EQ(max_result->value()->out_opp, max_opp_index);
  rate = scaler_clock_.rate();
  ASSERT_TRUE(rate.has_value());
  ASSERT_EQ(rate.value(), max_opp.freq_hz);

  // Check that we get the same value back from GetCurrentOperatingPoint
  auto max_result_again = cpu_client_->GetCurrentOperatingPoint();
  EXPECT_OK(max_result_again.status());
  EXPECT_EQ(max_result_again.value().out_opp, max_opp_index);

  // Set to the opp that we're already at and make sure that it's a no-op.
  auto same_result = cpu_client_->SetCurrentOperatingPoint(max_opp_index);
  EXPECT_OK(same_result.status());
  EXPECT_TRUE(same_result->is_ok());
  EXPECT_EQ(same_result->value()->out_opp, max_opp_index);

  // Check that we get the same value back from GetCurrentOperatingPoint
  auto same_result_again = cpu_client_->GetCurrentOperatingPoint();
  EXPECT_OK(same_result_again.status());
  EXPECT_EQ(same_result_again.value().out_opp, max_opp_index);
}

TEST_F(AmlCpuTestFixture, TestSetCpuInfo) {
  using namespace inspect::testing;

  uint32_t test_cpu_version = 0x28200b02;
  dut_.aml_cpu_for_testing().SetCpuInfo(test_cpu_version);
  auto hierarchy = inspect::ReadFromVmo(dut_.inspect_vmo());
  ASSERT_TRUE(hierarchy.is_ok());
  auto* cpu_info = hierarchy.value().GetByPath({"cpu_info_service"});
  ASSERT_TRUE(cpu_info);

  // cpu_major_revision : 40
  EXPECT_THAT(*cpu_info, NodeMatches(AllOf(PropertyList(::testing::UnorderedElementsAre(
                             UintIs("cpu_major_revision", 40), UintIs("cpu_minor_revision", 11),
                             UintIs("cpu_package_id", 2))))));
}

TEST_F(AmlCpuTestFixture, TestGetOperatingPointCount) {
  auto resp = cpu_client_->GetOperatingPointCount();

  ASSERT_OK(resp.status());

  EXPECT_EQ(resp.value()->count, kTestOperatingPoints.size());
}

TEST_F(AmlCpuTestFixture, TestGetLogicalCoreCount) {
  auto coreCountResp = cpu_client_->GetNumLogicalCores();

  ASSERT_OK(coreCountResp.status());

  EXPECT_EQ(coreCountResp.value().count, kTestCoreCount);
}

}  // namespace amlogic_cpu
