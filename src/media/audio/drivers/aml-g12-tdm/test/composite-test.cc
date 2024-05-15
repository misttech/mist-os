// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/aml-g12-tdm/composite.h"

#include <fidl/fuchsia.hardware.clock/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.gpio/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fake-bti/bti.h>
#include <lib/fdio/directory.h>
#include <lib/fzl/vmo-mapper.h>
#include <zircon/errors.h>

#include <algorithm>

#include <gtest/gtest.h>

namespace audio::aml_g12 {

// These tests check the audio-composite driver implementation variation of AMLogic SoCs audio
// support. Helper code in AmlTdmConfigDevice and //src/devices/lib/amlogic is tested in other
// versions of this driver (StreamConfig and DAI).

constexpr uint64_t kEngineARingBufferIdOutput = 4;
constexpr uint64_t kEngineBRingBufferIdOutput = 5;
constexpr uint64_t kEngineCRingBufferIdOutput = 6;
constexpr uint64_t kEngineARingBufferIdInput = 7;
constexpr uint64_t kEngineBRingBufferIdInput = 8;
constexpr uint64_t kEngineCRingBufferIdInput = 9;
constexpr uint64_t kInvalidBufferId0 = 3;
constexpr uint64_t kInvalidBufferId1 = 10;

constexpr std::array<uint64_t, 6> kAllValidIds{
    kEngineARingBufferIdOutput, kEngineBRingBufferIdOutput, kEngineCRingBufferIdOutput,
    kEngineARingBufferIdInput,  kEngineBRingBufferIdInput,  kEngineCRingBufferIdInput,
};

class FakePlatformDevice : public fidl::Server<fuchsia_hardware_platform_device::Device> {
 public:
  fuchsia_hardware_platform_device::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_platform_device::Service::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                          fidl::kIgnoreBindingClosure),
    });
  }

  void InitResources() {
    EXPECT_EQ(ZX_OK, zx::vmo::create(kMmioSize, 0, &mmio_));
    fake_bti_create(bti_.reset_and_get_address());
  }

  cpp20::span<uint32_t> mmio() {
    // The test has to wait for the driver to set the MMIO cache policy before mapping.
    if (!mapped_mmio_.start()) {
      MapMmio();
    }

    return {reinterpret_cast<uint32_t*>(mapped_mmio_.start()), kMmioSize / sizeof(uint32_t)};
  }

 private:
  static constexpr size_t kMmioSize = 0x1000;

  void GetMmioById(GetMmioByIdRequest& request, GetMmioByIdCompleter::Sync& completer) override {
    if (request.index() != 0) {
      return completer.Reply(zx::error(ZX_ERR_OUT_OF_RANGE));
    }

    zx::vmo vmo;
    if (zx_status_t status = mmio_.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo); status != ZX_OK) {
      return completer.Reply(zx::error(status));
    }

    completer.Reply(zx::ok(fuchsia_hardware_platform_device::Mmio{{
        .offset = 0,
        .size = kMmioSize,
        .vmo = std::move(vmo),
    }}));
  }

  void GetMmioByName(GetMmioByNameRequest& request,
                     GetMmioByNameCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetInterruptById(GetInterruptByIdRequest& request,
                        GetInterruptByIdCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetInterruptByName(GetInterruptByNameRequest& request,
                          GetInterruptByNameCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetBtiById(GetBtiByIdRequest& request, GetBtiByIdCompleter::Sync& completer) override {
    zx::bti bti;
    if (zx_status_t status = bti_.duplicate(ZX_RIGHT_SAME_RIGHTS, &bti); status != ZX_OK) {
      return completer.Reply(zx::error(status));
    }
    completer.Reply(zx::ok((std::move(bti))));
  }

  void GetBtiByName(GetBtiByNameRequest& request, GetBtiByNameCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetSmcById(GetSmcByIdRequest& request, GetSmcByIdCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetNodeDeviceInfo(GetNodeDeviceInfoCompleter::Sync& completer) override {
    fuchsia_hardware_platform_device::NodeDeviceInfo info;
    info.vid(PDEV_VID_AMLOGIC).pid(PDEV_PID_AMLOGIC_A311D);
    completer.Reply(zx::ok(std::move(info)));
  }

  void GetSmcByName(GetSmcByNameRequest& request, GetSmcByNameCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_platform_device::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  void MapMmio() { EXPECT_EQ(ZX_OK, mapped_mmio_.Map(mmio_)); }

  void GetPowerConfiguration(GetPowerConfigurationCompleter::Sync& completer) override {
    // fuchsia_hardware_power uses FIDL uint8 for power levels matching fuchsia_power_broker's.
    constexpr uint8_t kPowerLevelOff =
        static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOff);
    constexpr uint8_t kPowerLevelOn =
        static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOn);
    constexpr char kPowerElementName[] = "audio-hw";
    fuchsia_hardware_power::LevelTuple wake_handling_on = {{
        .child_level = kPowerLevelOn,
        .parent_level = static_cast<uint8_t>(fuchsia_power_system::ExecutionStateLevel::kActive),
    }};
    fuchsia_hardware_power::PowerDependency passive_on_execution_state = {{
        .child = kPowerElementName,
        .parent = fuchsia_hardware_power::ParentElement::WithSag(
            fuchsia_hardware_power::SagElement::kExecutionState),
        .level_deps = {{std::move(wake_handling_on)}},
        .strength = fuchsia_hardware_power::RequirementType::kPassive,
    }};
    fuchsia_hardware_power::PowerLevel off = {{.level = kPowerLevelOff, .name = "off"}};
    fuchsia_hardware_power::PowerLevel on = {{.level = kPowerLevelOn, .name = "on"}};
    fuchsia_hardware_power::PowerElement element = {
        {.name = kPowerElementName, .levels = {{std::move(off), std::move(on)}}}};
    fuchsia_hardware_power::PowerElementConfiguration wake_config = {
        {.element = std::move(element), .dependencies = {{std::move(passive_on_execution_state)}}}};

    completer.Reply(zx::ok(
        std::vector<fuchsia_hardware_power::PowerElementConfiguration>{{std::move(wake_config)}}));
  }

  zx::vmo mmio_;
  fzl::VmoMapper mapped_mmio_;
  zx::bti bti_;

  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> bindings_;
};

// Fake clock for power management test.
class FakeClock : public fidl::testing::WireTestBase<fuchsia_hardware_clock::Clock> {
 public:
  FakeClock() = default;

  bool IsFakeClockEnabled() { return enabled_; }
  fuchsia_hardware_clock::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_clock::Service::InstanceHandler({
        .clock = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                         fidl::kIgnoreBindingClosure),
    });
  }

 protected:
  void Enable(EnableCompleter::Sync& completer) override {
    enabled_ = true;
    completer.Reply(zx::ok());
  }
  void Disable(DisableCompleter::Sync& completer) override {
    enabled_ = false;
    completer.Reply(zx::ok());
  }
  void IsEnabled(IsEnabledCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void SetRate(::fuchsia_hardware_clock::wire::ClockSetRateRequest* request,
               SetRateCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }
  void QuerySupportedRate(::fuchsia_hardware_clock::wire::ClockQuerySupportedRateRequest* request,
                          QuerySupportedRateCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetRate(GetRateCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }
  void SetInput(::fuchsia_hardware_clock::wire::ClockSetInputRequest* request,
                SetInputCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }
  void GetNumInputs(GetNumInputsCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }
  void GetInput(GetInputCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  bool enabled_ = false;
  fidl::ServerBindingGroup<fuchsia_hardware_clock::Clock> bindings_;
};

class FakeGpio : public fidl::testing::WireTestBase<fuchsia_hardware_gpio::Gpio> {
 public:
  FakeGpio() = default;

  fuchsia_hardware_gpio::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_gpio::Service::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                          fidl::kIgnoreBindingClosure),
    });
  }
  bool IsFakeGpioSetToSclk() { return set_to_sclk_; }

 protected:
  void ConfigOut(ConfigOutRequestView request, ConfigOutCompleter::Sync& completer) override {
    completer.Reply(zx::ok());
  }
  void SetAltFunction(SetAltFunctionRequestView request,
                      SetAltFunctionCompleter::Sync& completer) override {
    set_to_sclk_ = request->function == 1;  // function is SCLK.
    completer.Reply(zx::ok());
  }
  void GetName(GetNameCompleter::Sync& completer) override { completer.ReplySuccess("Test"); }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  bool set_to_sclk_ = false;  // Even though the board driver may set this, do not assume it is set.
  fidl::ServerBindingGroup<fuchsia_hardware_gpio::Gpio> bindings_;
};

// Power Specific.
class FakeSystemActivityGovernor : public fidl::Server<fuchsia_power_system::ActivityGovernor> {
 public:
  FakeSystemActivityGovernor(zx::event wake_handling) : wake_handling_(std::move(wake_handling)) {}

  fidl::ProtocolHandler<fuchsia_power_system::ActivityGovernor> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override {
    fuchsia_power_system::PowerElements elements;
    zx::event duplicate;
    wake_handling_.duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate);

    fuchsia_power_system::ExecutionState wake_handling = {
        {.passive_dependency_token = std::move(duplicate)}};

    elements = {{.execution_state = std::move(wake_handling)}};

    completer.Reply({{std::move(elements)}});
  }

  void RegisterListener(RegisterListenerRequest& request,
                        RegisterListenerCompleter::Sync& completer) override {}

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  zx::event wake_handling_;
  fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor> bindings_;
};

class FakeLeaseControl : public fidl::Server<fuchsia_power_broker::LeaseControl> {
 public:
  void WatchStatus(fuchsia_power_broker::LeaseControlWatchStatusRequest& request,
                   WatchStatusCompleter::Sync& completer) override {
    completer.Reply(lease_status_);
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::LeaseControl> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  fuchsia_power_broker::LeaseStatus lease_status_ = fuchsia_power_broker::LeaseStatus::kSatisfied;
};

class FakeLessor : public fidl::Server<fuchsia_power_broker::Lessor> {
 public:
  void Lease(fuchsia_power_broker::LessorLeaseRequest& request,
             LeaseCompleter::Sync& completer) override {
    auto lease_control = fidl::CreateEndpoints<fuchsia_power_broker::LeaseControl>();
    lease_control_binding_.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   std::move(lease_control->server), &lease_control_,
                                   [](fidl::UnbindInfo info) mutable {});

    completer.Reply(fit::success(std::move(lease_control->client)));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Lessor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  FakeLeaseControl lease_control_;
  std::optional<fidl::ServerBinding<fuchsia_power_broker::LeaseControl>> lease_control_binding_;
};

class FakeRequiredLevel : public fidl::Server<fuchsia_power_broker::RequiredLevel> {
 public:
  void Watch(WatchCompleter::Sync& completer) override {
    ZX_ASSERT(!completer_);
    completer_.emplace(completer.ToAsync());
    sync_completion_signal(&ready_);
  }

  void set_required_level(fuchsia_power_broker::PowerLevel level) {
    ASSERT_EQ(ZX_OK, sync_completion_wait(&ready_, zx::duration::infinite().get()));
    sync_completion_reset(&ready_);
    ZX_ASSERT(completer_);
    completer_->Reply(fit::success(level));
    completer_.reset();
    required_level_ = level;
  }

  bool watch_received() { return sync_completion_signaled(&ready_); }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::RequiredLevel> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  fuchsia_power_broker::PowerLevel required_level_ = AudioCompositeServer::kAudioHardwareOn;
  std::optional<WatchCompleter::Async> completer_;
  sync_completion_t ready_;
};

class FakeCurrentLevel : public fidl::Server<fuchsia_power_broker::CurrentLevel> {
 public:
  void Update(fuchsia_power_broker::CurrentLevelUpdateRequest& request,
              UpdateCompleter::Sync& completer) override {
    current_level_ = request.current_level();
    completer.Reply(fit::success());
  }

  fuchsia_power_broker::PowerLevel current_level() { return current_level_; }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::CurrentLevel> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  fuchsia_power_broker::PowerLevel current_level_ = AudioCompositeServer::kAudioHardwareOn;
};

class FakePowerBroker : public fidl::Server<fuchsia_power_broker::Topology> {
 public:
  fidl::ProtocolHandler<fuchsia_power_broker::Topology> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

  void AddElement(fuchsia_power_broker::ElementSchema& request,
                  AddElementCompleter::Sync& completer) override {
    auto element_control = fidl::CreateEndpoints<fuchsia_power_broker::ElementControl>();
    element_control_server_ = std::move(element_control->server);
    if (request.lessor_channel()) {
      auto lessor_impl = std::make_unique<FakeLessor>();
      wake_lessor_ = lessor_impl.get();
      fidl::BindServer<fuchsia_power_broker::Lessor>(
          fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(*request.lessor_channel()),
          std::move(lessor_impl),
          [](FakeLessor* impl, fidl::UnbindInfo info,
             fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) mutable {});
    }
    if (request.level_control_channels()) {
      auto required_level_impl = std::make_unique<FakeRequiredLevel>();
      required_level_ = required_level_impl.get();
      fidl::BindServer<fuchsia_power_broker::RequiredLevel>(
          fdf::Dispatcher::GetCurrent()->async_dispatcher(),
          std::move(request.level_control_channels()->required()), std::move(required_level_impl),
          [](FakeRequiredLevel* impl, fidl::UnbindInfo info,
             fidl::ServerEnd<fuchsia_power_broker::RequiredLevel> server_end) mutable {});
      auto current_level_impl = std::make_unique<FakeCurrentLevel>();
      current_level_ = current_level_impl.get();
      fidl::BindServer<fuchsia_power_broker::CurrentLevel>(
          fdf::Dispatcher::GetCurrent()->async_dispatcher(),
          std::move(request.level_control_channels()->current()), std::move(current_level_impl),
          [](FakeCurrentLevel* impl, fidl::UnbindInfo info,
             fidl::ServerEnd<fuchsia_power_broker::CurrentLevel> server_end) mutable {});
    }

    fuchsia_power_broker::TopologyAddElementResponse result{
        {.element_control_channel = std::move(element_control->client)},
    };

    completer.Reply(fit::success(std::move(result)));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Topology> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  uint8_t current_level() {
    ZX_ASSERT(current_level_);
    return current_level_->current_level();
  }
  FakeRequiredLevel* required_level() {
    ZX_ASSERT(required_level_);
    return required_level_;
  }

 private:
  fidl::ServerEnd<fuchsia_power_broker::ElementControl> element_control_server_;
  FakeLessor* wake_lessor_ = nullptr;
  FakeRequiredLevel* required_level_ = nullptr;
  FakeCurrentLevel* current_level_ = nullptr;
  fidl::ServerBindingGroup<fuchsia_power_broker::Topology> bindings_;
};

struct IncomingNamespace {
  IncomingNamespace() {
    zx::event::create(0, &execution_state);
    zx::event execution_state_duplicate;
    execution_state.duplicate(ZX_RIGHT_SAME_RIGHTS, &execution_state_duplicate);
    system_activity_governor.emplace(std::move(execution_state_duplicate));
  }
  fdf_testing::TestNode node_{std::string("root")};
  fdf_testing::TestEnvironment env_{fdf::Dispatcher::GetCurrent()->get()};
  zx::event execution_state;
  std::optional<FakeSystemActivityGovernor> system_activity_governor;
  FakePowerBroker power_broker;
};

class AmlG12CompositeTest : public testing::Test {
 public:
  AmlG12CompositeTest()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        driver_dispatcher_(runtime_.StartBackgroundDispatcher()),
        incoming_(env_dispatcher(), std::in_place) {}

  void SetUp() override { Init(/*power_framework_enabled=*/false); }

  void Init(bool power_framework_enabled) {
    fuchsia_driver_framework::DriverStartArgs driver_start_args;
    incoming_.SyncCall([power_framework_enabled, &driver_start_args,
                        this](IncomingNamespace* incoming) {
      auto start_args_result = incoming->node_.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());

      auto init_result =
          incoming->env_.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());

      platform_device_.InitResources();
      const zx::result add_platform_result =
          incoming->env_.incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
              platform_device_.GetInstanceHandler());
      EXPECT_TRUE(add_platform_result.is_ok());

      auto add_clock_gate_result =
          incoming->env_.incoming_directory().AddService<fuchsia_hardware_clock::Service>(
              clock_gate_server_.GetInstanceHandler(), "clock-gate");
      ASSERT_TRUE(add_clock_gate_result.is_ok());

      auto add_clock_pll_result =
          incoming->env_.incoming_directory().AddService<fuchsia_hardware_clock::Service>(
              clock_pll_server_.GetInstanceHandler(), "clock-pll");
      ASSERT_TRUE(add_clock_pll_result.is_ok());

      auto add_sclk_tdm_a_result =
          incoming->env_.incoming_directory().AddService<fuchsia_hardware_gpio::Service>(
              sclk_tdm_a_server_.GetInstanceHandler(), "gpio-tdm-a-sclk");
      ASSERT_TRUE(add_sclk_tdm_a_result.is_ok());

      auto add_sclk_tdm_b_result =
          incoming->env_.incoming_directory().AddService<fuchsia_hardware_gpio::Service>(
              sclk_tdm_b_server_.GetInstanceHandler(), "gpio-tdm-b-sclk");
      ASSERT_TRUE(add_sclk_tdm_b_result.is_ok());

      auto add_sclk_tdm_c_result =
          incoming->env_.incoming_directory().AddService<fuchsia_hardware_gpio::Service>(
              sclk_tdm_c_server_.GetInstanceHandler(), "gpio-tdm-c-sclk");
      ASSERT_TRUE(add_sclk_tdm_c_result.is_ok());

      // Power specific.
      // TODO(b/339038497): This driver is built with Bazel and structured configuration is not
      // yet supported when building a driver with Bazel. Once available add a config parameter
      // check and test with power awareness enabled and disabled.
      if (power_framework_enabled) {
        auto result_sag = incoming->env_.incoming_directory()
                              .component()
                              .AddUnmanagedProtocol<fuchsia_power_system::ActivityGovernor>(
                                  incoming->system_activity_governor->CreateHandler());
        EXPECT_EQ(ZX_OK, result_sag.status_value());
        auto result_broker = incoming->env_.incoming_directory()
                                 .component()
                                 .AddUnmanagedProtocol<fuchsia_power_broker::Topology>(
                                     incoming->power_broker.CreateHandler());
        EXPECT_EQ(ZX_OK, result_broker.status_value());
      }

      driver_start_args = std::move(start_args_result->start_args);
    });

    zx::result result = runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::DriverUnderTest<Driver>::Start, std::move(driver_start_args)));
    ASSERT_EQ(ZX_OK, result.status_value());

    incoming_.SyncCall([this](IncomingNamespace* incoming) {
      auto client_channel = incoming->node_.children().at(kDriverName).ConnectToDevice();
      client_.Bind(
          fidl::ClientEnd<fuchsia_hardware_audio::Composite>(std::move(client_channel.value())));
      ASSERT_TRUE(client_.is_valid());
    });
  }

  void TearDown() override {
    zx::result result =
        runtime_.RunToCompletion(dut_.SyncCall(&fdf_testing::DriverUnderTest<Driver>::PrepareStop));
    ASSERT_EQ(ZX_OK, result.status_value());
  }

 protected:
  fuchsia_hardware_audio::DaiFormat GetDefaultDaiFormat() {
    return fuchsia_hardware_audio::DaiFormat(
        2, 3, fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
        fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
            fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S),
        48'000, 32, 16);
  }

  fuchsia_hardware_audio::Format GetDefaultRingBufferFormat() {
    fuchsia_hardware_audio::PcmFormat pcm_format{
        2, fuchsia_hardware_audio::SampleFormat::kPcmSigned, 2, 16, 48'000};
    fuchsia_hardware_audio::Format format;
    format.pcm_format(std::move(pcm_format));
    return format;
  }

  bool IsFakeClockGateEnabled() { return clock_gate_server_.IsFakeClockEnabled(); }
  bool IsFakeClockPllEnabled() { return clock_pll_server_.IsFakeClockEnabled(); }

  bool IsTdmASclkSet() { return sclk_tdm_a_server_.IsFakeGpioSetToSclk(); }
  bool IsTdmBSclkSet() { return sclk_tdm_b_server_.IsFakeGpioSetToSclk(); }
  bool IsTdmCSclkSet() { return sclk_tdm_c_server_.IsFakeGpioSetToSclk(); }

  void CheckDefaultDaiFormats(uint64_t id) {
    auto dai_formats_result = client_->GetDaiFormats(id);
    ASSERT_TRUE(dai_formats_result.is_ok());
    ASSERT_EQ(1, dai_formats_result->dai_formats().size());
    auto& dai_formats = dai_formats_result->dai_formats()[0];
    ASSERT_EQ(1, dai_formats.number_of_channels().size());
    ASSERT_EQ(2, dai_formats.number_of_channels()[0]);
    ASSERT_EQ(1, dai_formats.sample_formats().size());
    ASSERT_EQ(fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned, dai_formats.sample_formats()[0]);
    ASSERT_EQ(5, dai_formats.frame_formats().size());
    ASSERT_EQ(fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                  fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S),
              dai_formats.frame_formats()[0]);
    ASSERT_EQ(fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                  fuchsia_hardware_audio::DaiFrameFormatStandard::kTdm1),
              dai_formats.frame_formats()[1]);
    ASSERT_EQ(fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                  fuchsia_hardware_audio::DaiFrameFormatStandard::kTdm2),
              dai_formats.frame_formats()[2]);
    ASSERT_EQ(fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                  fuchsia_hardware_audio::DaiFrameFormatStandard::kTdm3),
              dai_formats.frame_formats()[3]);
    ASSERT_EQ(fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                  fuchsia_hardware_audio::DaiFrameFormatStandard::kStereoLeft),
              dai_formats.frame_formats()[4]);
    ASSERT_EQ(5, dai_formats.frame_rates().size());
    ASSERT_EQ(8'000, dai_formats.frame_rates()[0]);
    ASSERT_EQ(16'000, dai_formats.frame_rates()[1]);
    ASSERT_EQ(32'000, dai_formats.frame_rates()[2]);
    ASSERT_EQ(48'000, dai_formats.frame_rates()[3]);
    ASSERT_EQ(96'000, dai_formats.frame_rates()[4]);
    ASSERT_EQ(2, dai_formats.bits_per_slot().size());
    ASSERT_EQ(16, dai_formats.bits_per_slot()[0]);
    ASSERT_EQ(32, dai_formats.bits_per_slot()[1]);
    ASSERT_EQ(2, dai_formats.bits_per_sample().size());
    ASSERT_EQ(16, dai_formats.bits_per_sample()[0]);
    ASSERT_EQ(32, dai_formats.bits_per_sample()[1]);
  }

  void SetDaiFormatDefault(uint64_t id) {
    auto format = GetDefaultDaiFormat();
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(id, std::move(format));
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_ok());
  }

  void SetDaiFormatFrameFormat(uint64_t id, fuchsia_hardware_audio::DaiFrameFormat frame_format) {
    auto format = GetDefaultDaiFormat();
    format.frame_format(std::move(frame_format));
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(id, std::move(format));
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_ok());
  }

  void SetDaiFormatFrameRate(uint64_t id, uint32_t rate) {
    auto format = GetDefaultDaiFormat();
    format.frame_rate(rate);
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(id, std::move(format));
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_ok());
  }

  void SetDaiFormatBitePerSampleAndBitsPerSlot(uint64_t id, uint8_t bits_per_sample,
                                               uint8_t bits_per_slot) {
    auto format = GetDefaultDaiFormat();
    format.bits_per_sample(bits_per_sample);
    format.bits_per_slot(bits_per_slot);
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(id, std::move(format));
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_ok());
  }

  fidl::SyncClient<fuchsia_hardware_audio::Composite> client_;
  FakePlatformDevice platform_device_;
  FakeClock clock_gate_server_;
  FakeClock clock_pll_server_;
  FakeGpio sclk_tdm_a_server_;
  FakeGpio sclk_tdm_b_server_;
  FakeGpio sclk_tdm_c_server_;
  fdf_testing::DriverRuntime runtime_;

 private:
  async_dispatcher_t* driver_dispatcher() { return driver_dispatcher_->async_dispatcher(); }
  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }

  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_;
  // Use dut_ instead of driver_ because driver_ is used by gtest.
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<Driver>> dut_{
      driver_dispatcher(), std::in_place};

 protected:
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
};

TEST_F(AmlG12CompositeTest, CompositeProperties) {
  fidl::Result result = client_->GetProperties();
  ASSERT_TRUE(result.is_ok());
  ASSERT_EQ(fuchsia_hardware_audio::kClockDomainMonotonic,
            result->properties().clock_domain().value());
}

TEST_F(AmlG12CompositeTest, Reset) {
  // TODO(https://fxbug.dev/42082341): Add behavior to change state recovered to the configuration
  // below.
  fidl::Result reset_result = client_->Reset();
  ASSERT_TRUE(reset_result.is_ok());

  // After reset we check we have configured all engines for TDM output and input.

  // Configure TDM OUT for I2S 16 bits per slot and per sample (default).
  // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 16 bits per slot.
  ASSERT_EQ(0x3001'002F, platform_device_.mmio()[0x500 / 4]);  // A output.
  ASSERT_EQ(0x3001'002F, platform_device_.mmio()[0x540 / 4]);  // B output.
  ASSERT_EQ(0x3001'002F, platform_device_.mmio()[0x580 / 4]);  // C output.

  // Configure TDM IN for I2S 16 bits per slot and per sample (default).
  // TDM IN CTRL0 config, PAD_TDMIN A(0), B(1) and C(2).
  ASSERT_EQ(0x7003'000F, platform_device_.mmio()[0x300 / 4]);  // A input.
  ASSERT_EQ(0x7013'000F, platform_device_.mmio()[0x340 / 4]);  // B input.
  ASSERT_EQ(0x7023'000F, platform_device_.mmio()[0x380 / 4]);  // C input.

  // Configure clocks.
  // SCLK CTRL, clk in/out enabled, 24 sdiv, 16 lrduty, 32 lrdiv.
  ASSERT_EQ(0xc180'3c1f, platform_device_.mmio()[0x040 / 4]);  // A.
  ASSERT_EQ(0xc180'3c1f, platform_device_.mmio()[0x048 / 4]);  // B.
  ASSERT_EQ(0xc180'3c1f, platform_device_.mmio()[0x050 / 4]);  // C.
}

TEST_F(AmlG12CompositeTest, ElementsAndTopology) {
  auto endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_audio_signalprocessing::SignalProcessing>();
  auto connect_result = client_->SignalProcessingConnect(std::move(endpoints->server));
  ASSERT_TRUE(connect_result.is_ok());
  fidl::SyncClient signal_client{std::move(endpoints->client)};
  auto elements_result = signal_client->GetElements();
  ASSERT_TRUE(elements_result.is_ok());

  // Got ring buffer and DAIs for all engines input and output.
  constexpr size_t kNumberOfElements = 9;
  ASSERT_EQ(kNumberOfElements, elements_result->processing_elements().size());
  for (size_t i = 0; i < kNumberOfElements; ++i) {
    auto& element = elements_result->processing_elements()[i];
    ASSERT_TRUE(element.type() ==
                    fuchsia_hardware_audio_signalprocessing::ElementType::kRingBuffer ||
                element.type() == fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint);
    if (element.type() == fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint) {
      ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kHardwired,
                element.type_specific()->endpoint()->plug_detect_capabilities());
      auto watch_element_result = signal_client->WatchElementState(*element.id());
      ASSERT_TRUE(watch_element_result.is_ok());
      ASSERT_EQ(true,
                watch_element_result->state().type_specific()->endpoint()->plug_state()->plugged());
    }
  }
  auto& element0 = elements_result->processing_elements()[0];
  auto& element1 = elements_result->processing_elements()[1];
  auto& element2 = elements_result->processing_elements()[2];
  auto& element3 = elements_result->processing_elements()[3];
  auto& element4 = elements_result->processing_elements()[4];
  auto& element5 = elements_result->processing_elements()[5];
  auto& element6 = elements_result->processing_elements()[6];
  auto& element7 = elements_result->processing_elements()[7];
  auto& element8 = elements_result->processing_elements()[8];

  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::ElementType::kRingBuffer, element0.type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::ElementType::kRingBuffer, element1.type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::ElementType::kRingBuffer, element2.type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::ElementType::kRingBuffer, element3.type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::ElementType::kRingBuffer, element4.type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::ElementType::kRingBuffer, element5.type());

  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint, element6.type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint, element7.type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint, element8.type());

  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect,
            element6.type_specific()->endpoint()->type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect,
            element7.type_specific()->endpoint()->type());
  ASSERT_EQ(fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect,
            element8.type_specific()->endpoint()->type());

  constexpr uint64_t kExpectedTopologyId = 1;
  constexpr uint64_t kUnrecognizedTopologyId = 2;
  auto topology_result = signal_client->GetTopologies();
  ASSERT_TRUE(topology_result.is_ok());
  ASSERT_EQ(1, topology_result->topologies().size());
  auto& topology = topology_result->topologies()[0];
  ASSERT_EQ(kExpectedTopologyId, topology.id());

  auto set_topology_result = signal_client->SetTopology(kUnrecognizedTopologyId);
  ASSERT_TRUE(set_topology_result.is_error());
  ASSERT_TRUE(set_topology_result.error_value().is_domain_error());
  ASSERT_EQ(set_topology_result.error_value().domain_error(), ZX_ERR_INVALID_ARGS);

  set_topology_result = signal_client->SetTopology(kExpectedTopologyId);
  ASSERT_TRUE(set_topology_result.is_ok());

  // Get edges for ring buffer and DAIs for all engines.
  constexpr size_t kNumberOfEdges = 6;
  ASSERT_EQ(kNumberOfEdges, topology.processing_elements_edge_pairs()->size());
  auto& edge0 = (*topology.processing_elements_edge_pairs())[0];
  auto& edge1 = (*topology.processing_elements_edge_pairs())[1];
  auto& edge2 = (*topology.processing_elements_edge_pairs())[2];
  auto& edge3 = (*topology.processing_elements_edge_pairs())[3];
  auto& edge4 = (*topology.processing_elements_edge_pairs())[4];
  auto& edge5 = (*topology.processing_elements_edge_pairs())[5];

  // Output.
  ASSERT_EQ(4, edge0.processing_element_id_from());
  ASSERT_EQ(1, edge0.processing_element_id_to());
  ASSERT_EQ(5, edge1.processing_element_id_from());
  ASSERT_EQ(2, edge1.processing_element_id_to());
  ASSERT_EQ(6, edge2.processing_element_id_from());
  ASSERT_EQ(3, edge2.processing_element_id_to());

  // Input.
  ASSERT_EQ(1, edge3.processing_element_id_from());
  ASSERT_EQ(7, edge3.processing_element_id_to());
  ASSERT_EQ(2, edge4.processing_element_id_from());
  ASSERT_EQ(8, edge4.processing_element_id_to());
  ASSERT_EQ(3, edge5.processing_element_id_from());
  ASSERT_EQ(9, edge5.processing_element_id_to());

  auto watch_topology_result = signal_client->WatchTopology();
  ASSERT_TRUE(watch_topology_result.is_ok());
  ASSERT_EQ(1, watch_topology_result->topology_id());
}

TEST_F(AmlG12CompositeTest, ElementsState) {
  auto endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_audio_signalprocessing::SignalProcessing>();
  auto connect_result = client_->SignalProcessingConnect(std::move(endpoints->server));
  ASSERT_TRUE(connect_result.is_ok());
  fidl::SyncClient signal_client{std::move(endpoints->client)};
  auto elements_result = signal_client->GetElements();
  ASSERT_TRUE(elements_result.is_ok());

  // Able to set element state for all (endpoint) elements with no parameters.
  for (auto element : elements_result->processing_elements()) {
    if (element.type() == fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint) {
      fuchsia_hardware_audio_signalprocessing::SignalProcessingSetElementStateRequest request;
      request.processing_element_id(*element.id());
      auto set_element_result = signal_client->SetElementState(std::move(request));
      ASSERT_TRUE(set_element_result.is_ok());
    }
  }
}

TEST_F(AmlG12CompositeTest, GetDaiFormatsErrors) {
  // Only ids 1, 2, and 3 configure HW DAI formats.
  {
    auto dai_formats_result = client_->GetDaiFormats(0);
    ASSERT_TRUE(dai_formats_result.is_error());
    ASSERT_TRUE(dai_formats_result.error_value().is_domain_error());
    ASSERT_TRUE(dai_formats_result.error_value().domain_error() ==
                fuchsia_hardware_audio::DriverError::kInvalidArgs);
  }
  {
    auto dai_formats_result = client_->GetDaiFormats(4);
    ASSERT_TRUE(dai_formats_result.is_error());
    ASSERT_TRUE(dai_formats_result.error_value().is_domain_error());
    ASSERT_TRUE(dai_formats_result.error_value().domain_error() ==
                fuchsia_hardware_audio::DriverError::kInvalidArgs);
  }
}

TEST_F(AmlG12CompositeTest, GetDaiFormatsValid) {
  // Ids 1, 2, and 3 are valid configure HW DAI formats.
  CheckDefaultDaiFormats(1);
  CheckDefaultDaiFormats(2);
  CheckDefaultDaiFormats(3);
}

TEST_F(AmlG12CompositeTest, SetDaiFormatsErrors) {
  // Only ids 1, 2, and 3 configure HW DAI formats.
  {
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(0, GetDefaultDaiFormat());
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_error());
    ASSERT_TRUE(dai_formats_result.error_value().is_domain_error());
    ASSERT_TRUE(dai_formats_result.error_value().domain_error() ==
                fuchsia_hardware_audio::DriverError::kInvalidArgs);
  }
  {
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(4, GetDefaultDaiFormat());
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_error());
    ASSERT_TRUE(dai_formats_result.error_value().is_domain_error());
    ASSERT_TRUE(dai_formats_result.error_value().domain_error() ==
                fuchsia_hardware_audio::DriverError::kInvalidArgs);
  }
  {
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(1, GetDefaultDaiFormat());
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_ok());

    // Configure TDM A for 32 bits I2S:
    // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 32 bits per slot.
    ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x500 / 4]);
    // TDM IN CTRL0 config, PAD_TDMIN A, 32 bits per slot.
    ASSERT_EQ(0x7003'001F, platform_device_.mmio()[0x300 / 4]);
  }
  {
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(2, GetDefaultDaiFormat());
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_ok());
    // Configure TDM B for 32 bits I2S:
    // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 32 bits per slot.
    ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x540 / 4]);
    // TDM IN CTRL0 config, PAD_TDMIN B, 32 bits per slot.
    ASSERT_EQ(0x7013'001F, platform_device_.mmio()[0x340 / 4]);
  }
  {
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(3, GetDefaultDaiFormat());
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_ok());
    // Configure TDM C for 32 bits I2S:
    // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 32 bits per slot.
    ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x580 / 4]);
    // TDM IN CTRL0 config, PAD_TDMIN C, 32bits per slot.
    ASSERT_EQ(0x7023'001F, platform_device_.mmio()[0x380 / 4]);
  }

  // Any DAI field not in the supported ones returns an error.
  {
    auto format = GetDefaultDaiFormat();
    format.number_of_channels(123);
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(3, std::move(format));
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_error());
    ASSERT_TRUE(dai_formats_result.error_value().is_domain_error());
    ASSERT_TRUE(dai_formats_result.error_value().domain_error() ==
                fuchsia_hardware_audio::DriverError::kInvalidArgs);
  }
  {
    auto format = GetDefaultDaiFormat();
    format.sample_format(fuchsia_hardware_audio::DaiSampleFormat::kPcmFloat);
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(3, std::move(format));
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_error());
    ASSERT_TRUE(dai_formats_result.error_value().is_domain_error());
    ASSERT_TRUE(dai_formats_result.error_value().domain_error() ==
                fuchsia_hardware_audio::DriverError::kInvalidArgs);
  }
  {
    auto format = GetDefaultDaiFormat();
    format.frame_format(fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
        fuchsia_hardware_audio::DaiFrameFormatStandard::kStereoRight));
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(3, std::move(format));
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_error());
    ASSERT_TRUE(dai_formats_result.error_value().is_domain_error());
    ASSERT_TRUE(dai_formats_result.error_value().domain_error() ==
                fuchsia_hardware_audio::DriverError::kInvalidArgs);
  }
  {
    auto format = GetDefaultDaiFormat();
    format.frame_rate(123);
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(3, std::move(format));
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_error());
    ASSERT_TRUE(dai_formats_result.error_value().is_domain_error());
    ASSERT_TRUE(dai_formats_result.error_value().domain_error() ==
                fuchsia_hardware_audio::DriverError::kInvalidArgs);
  }
  {
    auto format = GetDefaultDaiFormat();
    format.bits_per_slot(123);
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(3, std::move(format));
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_error());
    ASSERT_TRUE(dai_formats_result.error_value().is_domain_error());
    ASSERT_TRUE(dai_formats_result.error_value().domain_error() ==
                fuchsia_hardware_audio::DriverError::kInvalidArgs);
  }
  {
    auto format = GetDefaultDaiFormat();
    format.bits_per_sample(123);
    fuchsia_hardware_audio::CompositeSetDaiFormatRequest request(3, std::move(format));
    auto dai_formats_result = client_->SetDaiFormat(std::move(request));
    ASSERT_TRUE(dai_formats_result.is_error());
    ASSERT_TRUE(dai_formats_result.error_value().is_domain_error());
    ASSERT_TRUE(dai_formats_result.error_value().domain_error() ==
                fuchsia_hardware_audio::DriverError::kInvalidArgs);
  }
}

TEST_F(AmlG12CompositeTest, SetDaiFormatsValid) {
  // Ids 1, 2, and 3 are valid configure HW DAI formats.

  SetDaiFormatDefault(1);
  // Configure TDM A for 32 bits I2S:
  // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 32 bits per slot.
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x500 / 4]);
  // TDM IN CTRL0 config, PAD_TDMIN A, 32 bits per slot.
  ASSERT_EQ(0x7003'001F, platform_device_.mmio()[0x300 / 4]);

  SetDaiFormatDefault(2);
  // Configure TDM B for 32 bits I2S:
  // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 32 bits per slot.
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x540 / 4]);
  // TDM IN CTRL0 config, PAD_TDMIN B, 32 bits per slot.
  ASSERT_EQ(0x7013'001F, platform_device_.mmio()[0x340 / 4]);

  SetDaiFormatDefault(3);
  // Configure TDM C for 32 bits I2S:
  // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 32 bits per slot.
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x580 / 4]);
  // TDM IN CTRL0 config, PAD_TDMIN C, 32bits per slot.
  ASSERT_EQ(0x7023'001F, platform_device_.mmio()[0x380 / 4]);

  SetDaiFormatFrameFormat(1, fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                                 fuchsia_hardware_audio::DaiFrameFormatStandard::kTdm1));
  // TDM OUT CTRL0 config, reg_tdm_init_bitnum (bitoffset) 3
  ASSERT_EQ(0x3001'803F, platform_device_.mmio()[0x500 / 4]);
  // TDM IN CTRL0 config, reg_tdmin_in_bit_skew 4
  ASSERT_EQ(0x3004'001F, platform_device_.mmio()[0x300 / 4]);

  SetDaiFormatFrameFormat(1, fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                                 fuchsia_hardware_audio::DaiFrameFormatStandard::kTdm2));
  // TDM OUT CTRL0 config, reg_tdm_init_bitnum (bitoffset) 2
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x500 / 4]);
  // TDM IN CTRL0 config, reg_tdmin_in_bit_skew 3
  ASSERT_EQ(0x3003'001F, platform_device_.mmio()[0x300 / 4]);

  SetDaiFormatFrameFormat(1, fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                                 fuchsia_hardware_audio::DaiFrameFormatStandard::kTdm3));
  // TDM OUT CTRL0 config, reg_tdm_init_bitnum (bitoffset) 1
  ASSERT_EQ(0x3000'803F, platform_device_.mmio()[0x500 / 4]);
  // TDM IN CTRL0 config, reg_tdmin_in_bit_skew 2
  ASSERT_EQ(0x3002'001F, platform_device_.mmio()[0x300 / 4]);

  SetDaiFormatFrameFormat(1, fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                                 fuchsia_hardware_audio::DaiFrameFormatStandard::kStereoLeft));
  // TDM OUT CTRL0 config, reg_tdm_init_bitnum (bitoffset) 3
  ASSERT_EQ(0x3001'803F, platform_device_.mmio()[0x500 / 4]);
  // TDM IN CTRL0 config, reg_tdmin_in_bit_skew 4
  ASSERT_EQ(0x3004'001F, platform_device_.mmio()[0x300 / 4]);

  SetDaiFormatFrameRate(1, 8'000);
  EXPECT_EQ(0x8400'003b, platform_device_.mmio()[0x001]);  // MCLK CTRL, div 60.

  SetDaiFormatFrameRate(1, 16'000);
  EXPECT_EQ(0x8400'001d, platform_device_.mmio()[0x001]);  // MCLK CTRL, div 30.

  SetDaiFormatFrameRate(1, 32'000);
  EXPECT_EQ(0x8400'000e, platform_device_.mmio()[0x001]);  // MCLK CTRL, div 15.

  SetDaiFormatFrameRate(1, 48'000);
  EXPECT_EQ(0x8400'0009, platform_device_.mmio()[0x001]);  // MCLK CTRL, div 10.

  SetDaiFormatFrameRate(1, 96'000);
  EXPECT_EQ(0x8400'0004, platform_device_.mmio()[0x001]);  // MCLK CTRL, div 5.

  SetDaiFormatFrameRate(2, 8'000);                         // A change to id 2 does not affect id 1.
  SetDaiFormatFrameRate(3, 48'000);                        // A change to id 3 does not affect id 1.
  EXPECT_EQ(0x8400'0004, platform_device_.mmio()[0x001]);  // MCLK CTRL, div 5.

  SetDaiFormatBitePerSampleAndBitsPerSlot(1, 32, 32);
  // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 32 bits per slot.
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x500 / 4]);
  // TDM OUT CTRL1 FRDDR with 32 bits per sample.
  ASSERT_EQ(0x0000'1f40, platform_device_.mmio()[0x504 / 4]);
  // SCLK CTRL, clk in/out enabled, 24 sdiv, 32 lrduty, 32 lrdiv.
  ASSERT_EQ(0xc180'7c3f, platform_device_.mmio()[0x040 / 4]);

  SetDaiFormatBitePerSampleAndBitsPerSlot(1, 16, 16);
  // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 16 bits per slot.
  ASSERT_EQ(0x3001'002F, platform_device_.mmio()[0x500 / 4]);
  // TDM OUT CTRL1 FRDDR with 16 bits per sample.
  ASSERT_EQ(0x0000'0f20, platform_device_.mmio()[0x504 / 4]);
  // SCLK CTRL, clk in/out enabled, 24 sdiv, 16 lrduty, 16 lrdiv.
  ASSERT_EQ(0xc180'3c1f, platform_device_.mmio()[0x040 / 4]);

  SetDaiFormatBitePerSampleAndBitsPerSlot(1, 16, 32);
  // TDM OUT CTRL0 config, bitoffset 2, 2 slots, 32 bits per slot.
  ASSERT_EQ(0x3001'003F, platform_device_.mmio()[0x500 / 4]);
  // TDM OUT CTRL1 FRDDR with 16 bits per sample.
  ASSERT_EQ(0x0000'0f20, platform_device_.mmio()[0x504 / 4]);
  // SCLK CTRL, clk in/out enabled, 24 sdiv, 32 lrduty, 32 lrdiv.
  ASSERT_EQ(0xc180'7c3f, platform_device_.mmio()[0x040 / 4]);
}

TEST_F(AmlG12CompositeTest, GetRingBufferFormats) {
  {
    auto ring_buffer_formats_result = client_->GetRingBufferFormats(kInvalidBufferId0);
    ASSERT_FALSE(ring_buffer_formats_result.is_ok());
  }
  {
    auto ring_buffer_formats_result = client_->GetRingBufferFormats(kInvalidBufferId1);
    ASSERT_FALSE(ring_buffer_formats_result.is_ok());
  }
  {
    auto ring_buffer_formats_result = client_->GetRingBufferFormats(kEngineARingBufferIdOutput);
    ASSERT_TRUE(ring_buffer_formats_result.is_ok());
    // There is at least one entry reported.
    ASSERT_GE(1, ring_buffer_formats_result->ring_buffer_formats().size());
    // Only 2 bytes per sample supported.
    auto& first_format = ring_buffer_formats_result->ring_buffer_formats()[0];
    ASSERT_TRUE(first_format.pcm_supported_formats());
    auto& first_pcm_format = *first_format.pcm_supported_formats();
    ASSERT_TRUE(first_pcm_format.bytes_per_sample());
    ASSERT_EQ(1, first_pcm_format.bytes_per_sample()->size());
    ASSERT_EQ(2, (*first_pcm_format.bytes_per_sample())[0]);
    ASSERT_EQ(1, first_pcm_format.valid_bits_per_sample()->size());
    ASSERT_EQ(16, (*first_pcm_format.valid_bits_per_sample())[0]);
  }
}

TEST_F(AmlG12CompositeTest, CreateRingBuffer) {
  {
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::RingBuffer>();
    fuchsia_hardware_audio::CompositeCreateRingBufferRequest request(
        kInvalidBufferId0, GetDefaultRingBufferFormat(), std::move(endpoints->server));
    auto ring_buffer_result = client_->CreateRingBuffer(std::move(request));
    ASSERT_FALSE(ring_buffer_result.is_ok());
  }
  {
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::RingBuffer>();
    fuchsia_hardware_audio::CompositeCreateRingBufferRequest request(
        kInvalidBufferId1, GetDefaultRingBufferFormat(), std::move(endpoints->server));
    auto ring_buffer_result = client_->CreateRingBuffer(std::move(request));
    ASSERT_FALSE(ring_buffer_result.is_ok());
  }
  // Any Ring Buffer field not in the supported ones returns an error.
  {
    auto format = GetDefaultRingBufferFormat();
    format.pcm_format()->number_of_channels(123);  // Bad number of channels.
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::RingBuffer>();
    fuchsia_hardware_audio::CompositeCreateRingBufferRequest request(
        kEngineARingBufferIdOutput, std::move(format), std::move(endpoints->server));
    auto ring_buffer_result = client_->CreateRingBuffer(std::move(request));
    ASSERT_FALSE(ring_buffer_result.is_ok());
  }
  {
    auto format = GetDefaultRingBufferFormat();
    format.pcm_format()->sample_format(
        fuchsia_hardware_audio::SampleFormat::kPcmFloat);  // Bad format.
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::RingBuffer>();
    fuchsia_hardware_audio::CompositeCreateRingBufferRequest request(
        kEngineARingBufferIdOutput, std::move(format), std::move(endpoints->server));
    auto ring_buffer_result = client_->CreateRingBuffer(std::move(request));
    ASSERT_FALSE(ring_buffer_result.is_ok());
  }
  {
    auto format = GetDefaultRingBufferFormat();
    format.pcm_format()->bytes_per_sample(123);  // Bad bytes per sample.
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::RingBuffer>();
    fuchsia_hardware_audio::CompositeCreateRingBufferRequest request(
        kEngineARingBufferIdOutput, std::move(format), std::move(endpoints->server));
    auto ring_buffer_result = client_->CreateRingBuffer(std::move(request));
    ASSERT_FALSE(ring_buffer_result.is_ok());
  }
  {
    auto format = GetDefaultRingBufferFormat();
    format.pcm_format()->valid_bits_per_sample(123);  // Bad valid bits per sample.
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::RingBuffer>();
    fuchsia_hardware_audio::CompositeCreateRingBufferRequest request(
        kEngineARingBufferIdOutput, std::move(format), std::move(endpoints->server));
    auto ring_buffer_result = client_->CreateRingBuffer(std::move(request));
    ASSERT_FALSE(ring_buffer_result.is_ok());
  }
  {
    auto format = GetDefaultRingBufferFormat();
    format.pcm_format()->frame_rate(123);  // Bad frame rate.
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::RingBuffer>();
    fuchsia_hardware_audio::CompositeCreateRingBufferRequest request(
        kEngineARingBufferIdOutput, std::move(format), std::move(endpoints->server));
    auto ring_buffer_result = client_->CreateRingBuffer(std::move(request));
    ASSERT_FALSE(ring_buffer_result.is_ok());
  }
}

class AmlG12CompositeRingBufferTest : public AmlG12CompositeTest {
 protected:
  void SetUp() override { AmlG12CompositeTest::SetUp(); }

  fidl::SyncClient<fuchsia_hardware_audio::RingBuffer> GetClient(uint64_t id) {
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::RingBuffer>();
    fuchsia_hardware_audio::CompositeCreateRingBufferRequest request(
        id, GetDefaultRingBufferFormat(), std::move(endpoints->server));
    auto ring_buffer_result = client_->CreateRingBuffer(std::move(request));
    ZX_ASSERT(ring_buffer_result.is_ok());
    return fidl::SyncClient<fuchsia_hardware_audio::RingBuffer>(std::move(endpoints->client));
  }

  void TestProperties(uint64_t id) {
    auto client = GetClient(id);
    fidl::Result result = client->GetProperties();
    ASSERT_TRUE(result.is_ok());
    ASSERT_TRUE(result->properties().needs_cache_flush_or_invalidate().has_value());
    ASSERT_TRUE(*result->properties().needs_cache_flush_or_invalidate());
    ASSERT_TRUE(result->properties().driver_transfer_bytes().has_value());
  }

  void TestGetVmo(uint64_t id) {
    auto client = GetClient(id);
    constexpr uint32_t kMinFrames = 1;
    auto get_vmo_result =
        client->GetVmo(fuchsia_hardware_audio::RingBufferGetVmoRequest(kMinFrames, 0));
    ASSERT_TRUE(get_vmo_result.is_ok());
    ASSERT_TRUE(get_vmo_result->ring_buffer().is_valid());
  }

  void TestGetVmoMultipleTimes(uint64_t id) {
    auto client = GetClient(id);
    constexpr uint32_t kMinFrames = 1;
    auto get_vmo_result0 =
        client->GetVmo(fuchsia_hardware_audio::RingBufferGetVmoRequest(kMinFrames, 0));
    ASSERT_TRUE(get_vmo_result0.is_ok());
    ASSERT_TRUE(get_vmo_result0->ring_buffer().is_valid());

    auto get_vmo_result1 =
        client->GetVmo(fuchsia_hardware_audio::RingBufferGetVmoRequest(kMinFrames, 0));
    ASSERT_FALSE(get_vmo_result1.is_ok());
  }

  void TestStartRingBufferNoVmo(uint64_t id) {
    auto client = GetClient(id);
    auto start_result = client->Start();
    ASSERT_FALSE(start_result.is_ok());  // Not ok to start a ring buffer with no VMO.
  }

  void TestStopRingBufferNoVmo(uint64_t id) {
    auto client = GetClient(id);
    auto stop_result = client->Stop();
    ASSERT_FALSE(stop_result.is_ok());  // Not ok to stop a ring buffer with no VMO.
  }

  void TestCreationAndStartStop(uint64_t id) {
    auto client = GetClient(id);
    constexpr uint32_t kMinFrames = 8192;
    auto get_vmo_result =
        client->GetVmo(fuchsia_hardware_audio::RingBufferGetVmoRequest(kMinFrames, 0));
    ASSERT_TRUE(get_vmo_result.is_ok());

    auto start_result = client->Start();
    ASSERT_TRUE(start_result.is_ok());

    auto stop_result = client->Stop();
    ASSERT_TRUE(stop_result.is_ok());
  }

  void TestStartStartedRingBuffer(uint64_t id) {
    auto client = GetClient(id);
    constexpr uint32_t kMinFrames = 8192;
    auto get_vmo_result =
        client->GetVmo(fuchsia_hardware_audio::RingBufferGetVmoRequest(kMinFrames, 0));
    ASSERT_TRUE(get_vmo_result.is_ok());

    auto start_result0 = client->Start();
    ASSERT_TRUE(start_result0.is_ok());

    auto start_result1 = client->Start();
    ASSERT_FALSE(start_result1.is_ok());  // Not ok to start a started ring buffer.
  }

  void TestStopStoppedRingBuffer(uint64_t id) {
    auto client = GetClient(id);
    constexpr uint32_t kMinFrames = 8192;
    auto get_vmo_result =
        client->GetVmo(fuchsia_hardware_audio::RingBufferGetVmoRequest(kMinFrames, 0));
    ASSERT_TRUE(get_vmo_result.is_ok());

    auto start_result = client->Start();
    ASSERT_TRUE(start_result.is_ok());

    auto stop_result0 = client->Stop();
    ASSERT_TRUE(stop_result0.is_ok());

    auto stop_result1 = client->Stop();
    ASSERT_TRUE(stop_result1.is_ok());  // Ok to stop a stopped ring buffer.
  }

  void TestGetDelay(uint64_t id) {
    auto client = GetClient(id);
    auto delay_result = client->WatchDelayInfo();
    ASSERT_TRUE(delay_result.is_ok());
    ASSERT_TRUE(delay_result->delay_info().internal_delay().has_value());  // Some delay reported.
  }
};

TEST_F(AmlG12CompositeRingBufferTest, RingBufferProperties) {
  for (auto& id : kAllValidIds) {
    TestProperties(id);
  }
}

TEST_F(AmlG12CompositeRingBufferTest, RingBufferGetVmo) {
  for (auto& id : kAllValidIds) {
    TestGetVmo(id);
  }
}

TEST_F(AmlG12CompositeRingBufferTest, RingBufferGetVmoMultipleTimes) {
  for (auto& id : kAllValidIds) {
    TestGetVmoMultipleTimes(id);
  }
}

TEST_F(AmlG12CompositeRingBufferTest, RingBuffersStartStop) {
  for (auto& id : kAllValidIds) {
    TestCreationAndStartStop(id);
  }
}

TEST_F(AmlG12CompositeRingBufferTest, RingBuffersClientCloseChannel) {
  for (auto& id : kAllValidIds) {
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::RingBuffer>();
    fuchsia_hardware_audio::CompositeCreateRingBufferRequest request(
        id, GetDefaultRingBufferFormat(), std::move(endpoints->server));
    auto ring_buffer_result = client_->CreateRingBuffer(std::move(request));
    ASSERT_TRUE(ring_buffer_result.is_ok());

    endpoints->client.TakeChannel().reset();
  }
}

TEST_F(AmlG12CompositeRingBufferTest, RingBufferStartStartedRingBuffer) {
  for (auto& id : kAllValidIds) {
    TestStartStartedRingBuffer(id);
  }
}

TEST_F(AmlG12CompositeRingBufferTest, RingBufferStopStoppedRingBuffer) {
  for (auto& id : kAllValidIds) {
    TestStopStoppedRingBuffer(id);
  }
}

TEST_F(AmlG12CompositeRingBufferTest, RingBufferGetDelay) {
  for (auto& id : kAllValidIds) {
    TestGetDelay(id);
  }
}

class AmlG12CompositeTestPower : public AmlG12CompositeTest {
 protected:
  void SetUp() override { Init(/*power_framework_enabled=*/true); }
};

TEST_F(AmlG12CompositeTestPower, DriverIsAlive) {
  fidl::Result result = client_->GetProperties();
  ASSERT_TRUE(result.is_ok());
  ASSERT_EQ(fuchsia_hardware_audio::kClockDomainMonotonic,
            result->properties().clock_domain().value());
}

TEST_F(AmlG12CompositeTestPower, PowerSuspendResume) {
  EXPECT_TRUE(IsTdmASclkSet());
  EXPECT_TRUE(IsTdmBSclkSet());
  EXPECT_TRUE(IsTdmCSclkSet());
  EXPECT_TRUE(IsFakeClockGateEnabled());
  EXPECT_TRUE(IsFakeClockPllEnabled());

  incoming_.SyncCall([](IncomingNamespace* incoming) {
    incoming->power_broker.required_level()->set_required_level(
        AudioCompositeServer::kAudioHardwareOff);
  });
  fuchsia_power_broker::PowerLevel level = {};
  do {
    level = incoming_.SyncCall(
        [](IncomingNamespace* incoming) { return incoming->power_broker.current_level(); });
  } while (level != AudioCompositeServer::kAudioHardwareOff);

  EXPECT_FALSE(IsTdmASclkSet());
  EXPECT_FALSE(IsTdmBSclkSet());
  EXPECT_FALSE(IsTdmCSclkSet());
  EXPECT_FALSE(IsFakeClockGateEnabled());
  EXPECT_FALSE(IsFakeClockPllEnabled());

  bool watch_received = false;
  do {
    watch_received = incoming_.SyncCall([](IncomingNamespace* incoming) {
      return incoming->power_broker.required_level()->watch_received();
    });
  } while (!watch_received);

  incoming_.SyncCall([](IncomingNamespace* incoming) {
    incoming->power_broker.required_level()->set_required_level(
        AudioCompositeServer::kAudioHardwareOn);
  });
  {
    fuchsia_power_broker::PowerLevel level = {};
    do {
      level = incoming_.SyncCall(
          [](IncomingNamespace* incoming) { return incoming->power_broker.current_level(); });
    } while (level != AudioCompositeServer::kAudioHardwareOn);
  }

  EXPECT_TRUE(IsTdmASclkSet());
  EXPECT_TRUE(IsTdmBSclkSet());
  EXPECT_TRUE(IsTdmCSclkSet());
  EXPECT_TRUE(IsFakeClockGateEnabled());
  EXPECT_TRUE(IsFakeClockPllEnabled());
}

}  // namespace audio::aml_g12
