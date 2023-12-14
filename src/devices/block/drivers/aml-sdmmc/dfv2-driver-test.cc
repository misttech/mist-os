// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dfv2-driver.h"

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fake-bti/bti.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/inspect/testing/cpp/zxtest/inspect.h>
#include <lib/mmio-ptr/fake.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/sdio/hw.h>
#include <lib/sdmmc/hw.h>
#include <threads.h>
#include <zircon/types.h>

#include <memory>
#include <vector>

#include <soc/aml-s912/s912-hw.h>
#include <zxtest/zxtest.h>

#include "aml-sdmmc-regs.h"
#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/lib/mmio/test-helper.h"

namespace aml_sdmmc {

class TestDfv2Driver : public Dfv2Driver {
 public:
  TestDfv2Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : Dfv2Driver(std::move(start_args), std::move(dispatcher)) {}

  void* SetTestHooks() {
    view_.emplace(mmio().View(0));
    return descs_buffer();
  }

  const inspect::Hierarchy* GetInspectRoot(const std::string& suffix) {
    const zx::vmo inspect_vmo = inspector().DuplicateVmo();
    if (!inspect_vmo.is_valid()) {
      return nullptr;
    }

    inspector_.ReadInspect(inspect_vmo);
    return inspector_.hierarchy().GetByPath({"aml-sdmmc-port" + suffix});
  }

  void ExpectInspectPropertyValue(const std::string& name, uint64_t value) {
    const auto* root = GetInspectRoot("-unknown");
    ASSERT_NOT_NULL(root);

    const auto* property = root->node().get_property<inspect::UintPropertyValue>(name);
    ASSERT_NOT_NULL(property);
    EXPECT_EQ(property->value(), value);
  }

  void ExpectInspectPropertyValue(const std::string& path, const std::string& name,
                                  std::string_view value) {
    const auto* root = GetInspectRoot("-unknown");
    ASSERT_NOT_NULL(root);

    const auto* property =
        root->GetByPath({path})->node().get_property<inspect::StringPropertyValue>(name);
    ASSERT_NOT_NULL(property);
    EXPECT_STREQ(property->value(), value);
  }

  zx_status_t WaitForInterruptImpl() override {
    fake_bti_pinned_vmo_info_t pinned_vmos[2];
    size_t actual = 0;
    zx_status_t status =
        fake_bti_get_pinned_vmos(bti().get(), pinned_vmos, std::size(pinned_vmos), &actual);
    // In the tuning case there are exactly two VMOs pinned: one to hold the DMA descriptors, and
    // one to hold the received tuning block. Write the expected tuning data to the second pinned
    // VMO so that the tuning check always passes.
    if (status == ZX_OK && actual == std::size(pinned_vmos) &&
        pinned_vmos[0].size >= sizeof(aml_sdmmc_tuning_blk_pattern_4bit)) {
      zx_vmo_write(pinned_vmos[1].vmo, aml_sdmmc_tuning_blk_pattern_4bit, pinned_vmos[1].offset,
                   sizeof(aml_sdmmc_tuning_blk_pattern_4bit));
    }
    for (size_t i = 0; i < std::min(actual, std::size(pinned_vmos)); i++) {
      zx_handle_close(pinned_vmos[i].vmo);
    }

    if (request_index_ < request_results_.size() && request_results_[request_index_] == 0) {
      // Indicate a receive CRC error.
      view_->Write32(1, kAmlSdmmcStatusOffset);

      successful_transfers_ = 0;
      request_index_++;
    } else if (interrupt_status_.has_value()) {
      view_->Write32(interrupt_status_.value(), kAmlSdmmcStatusOffset);
    } else {
      // Indicate that the request completed successfully.
      view_->Write32(1 << 13, kAmlSdmmcStatusOffset);

      // Each tuning transfer is attempted five times with a short-circuit if one fails.
      // Report every successful transfer five times to make the results arrays easier to
      // follow.
      if (++successful_transfers_ % AML_SDMMC_TUNING_TEST_ATTEMPTS == 0) {
        successful_transfers_ = 0;
        request_index_++;
      }
    }
    return ZX_OK;
  }

  void WaitForBus() const override { /* Do nothing, bus is always ready in tests */
  }

  void SetRequestResults(const char* request_results) {
    request_results_.clear();
    const size_t results_size = strlen(request_results);
    request_results_.reserve(results_size);

    for (size_t i = 0; i < results_size; i++) {
      ASSERT_TRUE((request_results[i] == '|') || (request_results[i] == '-'));
      request_results_.push_back(request_results[i] == '|' ? 1 : 0);
    }

    request_index_ = 0;
  }

  void SetRequestInterruptStatus(uint32_t status) { interrupt_status_ = status; }

 private:
  std::vector<uint8_t> request_results_;
  size_t request_index_ = 0;
  uint32_t successful_transfers_ = 0;
  // The optional interrupt status to set after a request is completed.
  std::optional<uint32_t> interrupt_status_;
  inspect::InspectTestHelper inspector_;
  std::optional<fdf::MmioView> view_;
};

class FakeClock : public fidl::WireServer<fuchsia_hardware_clock::Clock> {
 public:
  fuchsia_hardware_clock::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_clock::Service::InstanceHandler({
        .clock = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                         fidl::kIgnoreBindingClosure),
    });
  }

  bool enabled() const { return enabled_; }

 private:
  void Enable(EnableCompleter::Sync& completer) override {
    enabled_ = true;
    completer.ReplySuccess();
  }

  void Disable(DisableCompleter::Sync& completer) override {
    enabled_ = false;
    completer.ReplySuccess();
  }

  void IsEnabled(IsEnabledCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void SetRate(SetRateRequestView request, SetRateCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void QuerySupportedRate(QuerySupportedRateRequestView request,
                          QuerySupportedRateCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetRate(GetRateCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void SetInput(SetInputRequestView request, SetInputCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetNumInputs(GetNumInputsCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetInput(GetInputCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  fidl::ServerBindingGroup<fuchsia_hardware_clock::Clock> bindings_;

  bool enabled_ = false;
};

struct IncomingNamespace {
  fdf_testing::TestNode node{"root"};
  fdf_testing::TestEnvironment env{fdf::Dispatcher::GetCurrent()->get()};
  compat::DeviceServer device_server;
  fake_pdev::FakePDevFidl pdev_server;
  FakeClock clock_server;
};

class AmlSdmmcTest : public zxtest::Test {
 public:
  AmlSdmmcTest()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        incoming_(env_dispatcher_->async_dispatcher(), std::in_place),
        mmio_buffer_(
            fdf_testing::CreateMmioBuffer(S912_SD_EMMC_B_LENGTH, ZX_CACHE_POLICY_UNCACHED_DEVICE)) {
    mmio_.emplace(mmio_buffer_.View(0));
  }

  void StartDriver(bool create_fake_bti_with_paddrs = false) {
    memset(bti_paddrs_, 0, sizeof(bti_paddrs_));
    // This is used by AmlSdmmc::Init() to create the descriptor buffer -- can be any nonzero paddr.
    bti_paddrs_[0] = zx_system_get_page_size();

    zx::bti bti;
    if (create_fake_bti_with_paddrs) {
      ASSERT_OK(fake_bti_create_with_paddrs(bti_paddrs_, std::size(bti_paddrs_),
                                            bti.reset_and_get_address()));
    } else {
      ASSERT_OK(fake_bti_create(bti.reset_and_get_address()));
    }

    // Initialize driver test environment.
    fuchsia_driver_framework::DriverStartArgs start_args;
    fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client;
    aml_sdmmc_config_t metadata = {
        .min_freq = 400000,
        .max_freq = 120000000,
        .version_3 = true,
        .prefs = 0,
    };
    incoming_.SyncCall([&, bti = std::move(bti)](IncomingNamespace* incoming) mutable {
      auto start_args_result = incoming->node.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());
      start_args = std::move(start_args_result->start_args);
      outgoing_directory_client = std::move(start_args_result->outgoing_directory_client);

      ASSERT_OK(incoming->env.Initialize(std::move(start_args_result->incoming_directory_server)));

      incoming->device_server.Init("default", "");
      // Serve metadata.
      ASSERT_OK(incoming->device_server.AddMetadata(DEVICE_METADATA_PRIVATE, &metadata,
                                                    sizeof(metadata)));
      ASSERT_OK(incoming->device_server.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                              &incoming->env.incoming_directory()));

      // Serve (fake) pdev_server.
      fake_pdev::FakePDevFidl::Config config;
      config.use_fake_irq = true;
      zx::vmo dup;
      mmio_buffer_.get_vmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);
      config.mmios[0] =
          fake_pdev::MmioInfo{std::move(dup), mmio_buffer_.get_offset(), mmio_buffer_.get_size()};
      config.btis[0] = std::move(bti);
      config.device_info = pdev_device_info_t{};
      incoming->pdev_server.SetConfig(std::move(config));
      {
        auto result = incoming->env.incoming_directory()
                          .AddService<fuchsia_hardware_platform_device::Service>(
                              std::move(incoming->pdev_server.GetInstanceHandler(
                                  fdf::Dispatcher::GetCurrent()->async_dispatcher())),
                              "default");
        ASSERT_TRUE(result.is_ok());
      }

      // Serve (fake) clock_server.
      {
        auto result =
            incoming->env.incoming_directory().AddService<fuchsia_hardware_clock::Service>(
                std::move(incoming->clock_server.GetInstanceHandler()), "clock-gate");
        ASSERT_TRUE(result.is_ok());
      }
    });

    // Start dut_.
    ASSERT_OK(runtime_.RunToCompletion(dut_.Start(std::move(start_args))));

    descs_ = dut_->SetTestHooks();

    dut_->set_board_config({
        .min_freq = 400000,
        .max_freq = 120000000,
        .version_3 = true,
        .prefs = 0,
    });

    mmio_->Write32(0xff, kAmlSdmmcDelay1Offset);
    mmio_->Write32(0xff, kAmlSdmmcDelay2Offset);
    mmio_->Write32(0xff, kAmlSdmmcAdjustOffset);

    dut_->SdmmcHwReset();

    EXPECT_EQ(mmio_->Read32(kAmlSdmmcDelay1Offset), 0);
    EXPECT_EQ(mmio_->Read32(kAmlSdmmcDelay2Offset), 0);
    EXPECT_EQ(mmio_->Read32(kAmlSdmmcAdjustOffset), 0);

    mmio_->Write32(1, kAmlSdmmcCfgOffset);  // Set bus width 4.
  }

 protected:
  static zx_koid_t GetVmoKoid(const zx::vmo& vmo) {
    zx_info_handle_basic_t info = {};
    size_t actual = 0;
    size_t available = 0;
    zx_status_t status =
        vmo.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), &actual, &available);
    if (status != ZX_OK || actual < 1) {
      return ZX_KOID_INVALID;
    }
    return info.koid;
  }

  void InitializeContiguousPaddrs(const size_t vmos) {
    // Start at 1 because one paddr has already been read to create the DMA descriptor buffer.
    for (size_t i = 0; i < vmos; i++) {
      bti_paddrs_[i + 1] = (i << 24) | zx_system_get_page_size();
    }
  }

  void InitializeSingleVmoPaddrs(const size_t pages) {
    // Start at 1 (see comment above).
    for (size_t i = 0; i < pages; i++) {
      bti_paddrs_[i + 1] = zx_system_get_page_size() * (i + 1);
    }
  }

  void InitializeNonContiguousPaddrs(const size_t vmos) {
    // Start at 1 (see comment above).
    for (size_t i = 0; i < vmos; i++) {
      bti_paddrs_[i + 1] = zx_system_get_page_size() * (i + 1) * 2;
    }
  }

  aml_sdmmc_desc_t* descriptors() const { return reinterpret_cast<aml_sdmmc_desc_t*>(descs_); }

  zx_paddr_t bti_paddrs_[64] = {};

  std::optional<fdf::MmioView> mmio_;
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
  fdf_testing::DriverUnderTest<TestDfv2Driver> dut_;

 private:
  fdf::MmioBuffer mmio_buffer_;
  void* descs_ = nullptr;
};

TEST_F(AmlSdmmcTest, InitV3) {
  StartDriver();

  dut_->set_board_config({
      .min_freq = 400000,
      .max_freq = 120000000,
      .version_3 = true,
      .prefs = 0,
  });

  AmlSdmmcClock::Get().FromValue(0).WriteTo(&*mmio_);

  ASSERT_OK(dut_->Init({}));

  EXPECT_EQ(AmlSdmmcClock::Get().ReadFrom(&*mmio_).reg_value(), AmlSdmmcClockV3::Get()
                                                                    .FromValue(0)
                                                                    .set_cfg_div(60)
                                                                    .set_cfg_src(0)
                                                                    .set_cfg_co_phase(2)
                                                                    .set_cfg_tx_phase(0)
                                                                    .set_cfg_rx_phase(0)
                                                                    .set_cfg_always_on(1)
                                                                    .reg_value());
}

TEST_F(AmlSdmmcTest, InitV2) {
  StartDriver();

  dut_->set_board_config({
      .min_freq = 400000,
      .max_freq = 120000000,
      .version_3 = false,
      .prefs = 0,
  });

  AmlSdmmcClock::Get().FromValue(0).WriteTo(&*mmio_);

  ASSERT_OK(dut_->Init({}));

  EXPECT_EQ(AmlSdmmcClock::Get().ReadFrom(&*mmio_).reg_value(), AmlSdmmcClockV2::Get()
                                                                    .FromValue(0)
                                                                    .set_cfg_div(60)
                                                                    .set_cfg_src(0)
                                                                    .set_cfg_co_phase(2)
                                                                    .set_cfg_tx_phase(0)
                                                                    .set_cfg_rx_phase(0)
                                                                    .set_cfg_always_on(1)
                                                                    .reg_value());
}

TEST_F(AmlSdmmcTest, TuningV3) {
  StartDriver();

  ASSERT_OK(dut_->Init({}));

  AmlSdmmcClock::Get().FromValue(0).set_cfg_div(10).WriteTo(&*mmio_);
  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_bus_width(AmlSdmmcCfg::kBusWidth4Bit).WriteTo(&*mmio_);

  auto adjust = AmlSdmmcAdjust::Get().FromValue(0);
  auto adjust_v2 = AmlSdmmcAdjustV2::Get().FromValue(0);

  adjust.set_adj_fixed(0).set_adj_delay(0x3f).WriteTo(&*mmio_);
  adjust_v2.set_adj_fixed(0).set_adj_delay(0x3f).WriteTo(&*mmio_);

  EXPECT_OK(dut_->SdmmcPerformTuning(SD_SEND_TUNING_BLOCK));

  adjust.ReadFrom(&*mmio_);
  adjust_v2.ReadFrom(&*mmio_);

  EXPECT_EQ(adjust.adj_fixed(), 1);
  EXPECT_EQ(adjust.adj_delay(), 0);
}

TEST_F(AmlSdmmcTest, TuningV2) {
  StartDriver();

  dut_->set_board_config({
      .min_freq = 400000,
      .max_freq = 120000000,
      .version_3 = false,
      .prefs = 0,
  });

  ASSERT_OK(dut_->Init({}));

  AmlSdmmcClock::Get().FromValue(0).set_cfg_div(10).WriteTo(&*mmio_);
  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_bus_width(AmlSdmmcCfg::kBusWidth4Bit).WriteTo(&*mmio_);

  auto adjust = AmlSdmmcAdjust::Get().FromValue(0);
  auto adjust_v2 = AmlSdmmcAdjustV2::Get().FromValue(0);

  adjust.set_adj_fixed(0).set_adj_delay(0x3f).WriteTo(&*mmio_);
  adjust_v2.set_adj_fixed(0).set_adj_delay(0x3f).WriteTo(&*mmio_);

  EXPECT_OK(dut_->SdmmcPerformTuning(SD_SEND_TUNING_BLOCK));

  adjust.ReadFrom(&*mmio_);
  adjust_v2.ReadFrom(&*mmio_);

  EXPECT_EQ(adjust_v2.adj_fixed(), 1);
  EXPECT_EQ(adjust_v2.adj_delay(), 0);
}

TEST_F(AmlSdmmcTest, DelayLineTuningAllPass) {
  StartDriver();

  dut_->set_board_config({
      .min_freq = 400000,
      .max_freq = 120000000,
      .version_3 = true,
      .prefs = 0,
  });
  ASSERT_OK(dut_->Init({}));

  AmlSdmmcClock::Get().FromValue(0).set_cfg_div(10).WriteTo(&*mmio_);
  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_bus_width(AmlSdmmcCfg::kBusWidth4Bit).WriteTo(&*mmio_);

  auto adjust = AmlSdmmcAdjust::Get().FromValue(0).set_adj_delay(0x3f).WriteTo(&*mmio_);
  auto delay1 = AmlSdmmcDelay1::Get().FromValue(0).WriteTo(&*mmio_);
  auto delay2 = AmlSdmmcDelay2::Get().FromValue(0).WriteTo(&*mmio_);

  EXPECT_OK(dut_->SdmmcPerformTuning(SD_SEND_TUNING_BLOCK));

  adjust.ReadFrom(&*mmio_);
  delay1.ReadFrom(&*mmio_);
  delay2.ReadFrom(&*mmio_);

  // No failing window was found, so the default settings should be used.
  EXPECT_EQ(adjust.adj_delay(), 0);
  EXPECT_EQ(delay1.dly_0(), 0);
  EXPECT_EQ(delay1.dly_1(), 0);
  EXPECT_EQ(delay1.dly_2(), 0);
  EXPECT_EQ(delay1.dly_3(), 0);
  EXPECT_EQ(delay1.dly_4(), 0);
  EXPECT_EQ(delay2.dly_5(), 0);
  EXPECT_EQ(delay2.dly_6(), 0);
  EXPECT_EQ(delay2.dly_7(), 0);
  EXPECT_EQ(delay2.dly_8(), 0);
  EXPECT_EQ(delay2.dly_9(), 0);

  dut_->ExpectInspectPropertyValue("adj_delay", 0);

  dut_->ExpectInspectPropertyValue("delay_lines", 0);

  dut_->ExpectInspectPropertyValue(
      "tuning_results_adj_delay_0", "tuning_results",
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||");

  dut_->ExpectInspectPropertyValue("distance_to_failing_point", 63);
}

TEST_F(AmlSdmmcTest, DelayLineTuningFailingPoint) {
  StartDriver();

  dut_->SetRequestResults(
      "-----------|||||||||||||||||||||||||||||||||||||||||||||||||----"
      "-------------------------------|||||||||||||||||||||||||||||||||"
      "||||||||||-----------------------------------------|||||||||||||"
      "||||||||||||||||||||||||||||||----------------------------------"
      "||||||||||||||||||||||||||||||||||||||||||||||||||--------------"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "-----------|||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "-------------------------------|||||||||||||||||||||||||||||||||"
      "||||||||||||||||||||-------------------------------|||||||||||||"
      "||||||||||||||||||||||||||||||||||||||||------------------------");

  dut_->set_board_config({
      .min_freq = 400000,
      .max_freq = 120000000,
      .version_3 = true,
      .prefs = 0,
  });
  ASSERT_OK(dut_->Init({}));

  AmlSdmmcClock::Get().FromValue(0).set_cfg_div(10).WriteTo(&*mmio_);
  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_bus_width(AmlSdmmcCfg::kBusWidth4Bit).WriteTo(&*mmio_);

  auto adjust = AmlSdmmcAdjust::Get().FromValue(0).set_adj_delay(0x3f).WriteTo(&*mmio_);
  auto delay1 = AmlSdmmcDelay1::Get().FromValue(0).WriteTo(&*mmio_);
  auto delay2 = AmlSdmmcDelay2::Get().FromValue(0).WriteTo(&*mmio_);

  EXPECT_OK(dut_->SdmmcPerformTuning(SD_SEND_TUNING_BLOCK));

  adjust.ReadFrom(&*mmio_);
  delay1.ReadFrom(&*mmio_);
  delay2.ReadFrom(&*mmio_);

  EXPECT_EQ(adjust.adj_delay(), 7);
  EXPECT_EQ(delay1.dly_0(), 30);
  EXPECT_EQ(delay1.dly_1(), 30);
  EXPECT_EQ(delay1.dly_2(), 30);
  EXPECT_EQ(delay1.dly_3(), 30);
  EXPECT_EQ(delay1.dly_4(), 30);
  EXPECT_EQ(delay2.dly_5(), 30);
  EXPECT_EQ(delay2.dly_6(), 30);
  EXPECT_EQ(delay2.dly_7(), 30);
  EXPECT_EQ(delay2.dly_8(), 30);
  EXPECT_EQ(delay2.dly_9(), 30);

  dut_->ExpectInspectPropertyValue("adj_delay", 7);

  dut_->ExpectInspectPropertyValue("delay_lines", 30);

  dut_->ExpectInspectPropertyValue(
      "tuning_results_adj_delay_7", "tuning_results",
      "-------------------------------|||||||||||||||||||||||||||||||||");

  dut_->ExpectInspectPropertyValue("distance_to_failing_point", 0);
}

TEST_F(AmlSdmmcTest, DelayLineTuningEvenDivider) {
  StartDriver();

  dut_->SetRequestResults(
      // Largest failing window: adj_delay 8, middle delay 25
      "||||||||||||||||||||||||||||||||||||||||||||||||||--------------"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "-|||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "---------------------|||||||||||||||||||||||||||||||||||||||||||"
      "||||||||||-------------------------------|||||||||||||||||||||||"
      "||||||||||||||||||||||||||||||-------------------------------|||");

  dut_->set_board_config({
      .min_freq = 400000,
      .max_freq = 120000000,
      .version_3 = true,
      .prefs = 0,
  });
  ASSERT_OK(dut_->Init({}));

  AmlSdmmcClock::Get().FromValue(0).set_cfg_div(10).WriteTo(&*mmio_);
  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_bus_width(AmlSdmmcCfg::kBusWidth4Bit).WriteTo(&*mmio_);

  auto adjust = AmlSdmmcAdjust::Get().FromValue(0).set_adj_delay(0x3f).WriteTo(&*mmio_);
  auto delay1 = AmlSdmmcDelay1::Get().FromValue(0).WriteTo(&*mmio_);
  auto delay2 = AmlSdmmcDelay2::Get().FromValue(0).WriteTo(&*mmio_);

  EXPECT_OK(dut_->SdmmcPerformTuning(SD_SEND_TUNING_BLOCK));

  adjust.ReadFrom(&*mmio_);
  delay1.ReadFrom(&*mmio_);
  delay2.ReadFrom(&*mmio_);

  EXPECT_EQ(adjust.adj_delay(), 3);
  EXPECT_EQ(delay1.dly_0(), 25);
  EXPECT_EQ(delay1.dly_1(), 25);
  EXPECT_EQ(delay1.dly_2(), 25);
  EXPECT_EQ(delay1.dly_3(), 25);
  EXPECT_EQ(delay1.dly_4(), 25);
  EXPECT_EQ(delay2.dly_5(), 25);
  EXPECT_EQ(delay2.dly_6(), 25);
  EXPECT_EQ(delay2.dly_7(), 25);
  EXPECT_EQ(delay2.dly_8(), 25);
  EXPECT_EQ(delay2.dly_9(), 25);

  dut_->ExpectInspectPropertyValue("adj_delay", 3);

  dut_->ExpectInspectPropertyValue("delay_lines", 25);

  dut_->ExpectInspectPropertyValue(
      "tuning_results_adj_delay_4", "tuning_results",
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||");

  dut_->ExpectInspectPropertyValue("distance_to_failing_point", 63);
}

TEST_F(AmlSdmmcTest, DelayLineTuningOddDivider) {
  StartDriver();

  dut_->SetRequestResults(
      // Largest failing window: adj_delay 3, first delay 0
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "-----------|||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "-------------------------------|||||||||||||||||||||||||||||||||"
      "||||||||||||||||||||-------------------------------|||||||||||||"
      "||||||||||||||||||||||||||||||||||||||||------------------------"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||----"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||");

  dut_->set_board_config({
      .min_freq = 400000,
      .max_freq = 120000000,
      .version_3 = true,
      .prefs = 0,
  });
  ASSERT_OK(dut_->Init({}));

  AmlSdmmcClock::Get().FromValue(0).set_cfg_div(9).WriteTo(&*mmio_);
  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_bus_width(AmlSdmmcCfg::kBusWidth4Bit).WriteTo(&*mmio_);

  auto adjust = AmlSdmmcAdjust::Get().FromValue(0).set_adj_delay(0x3f).WriteTo(&*mmio_);
  auto delay1 = AmlSdmmcDelay1::Get().FromValue(0).WriteTo(&*mmio_);
  auto delay2 = AmlSdmmcDelay2::Get().FromValue(0).WriteTo(&*mmio_);

  EXPECT_OK(dut_->SdmmcPerformTuning(SD_SEND_TUNING_BLOCK));

  adjust.ReadFrom(&*mmio_);
  delay1.ReadFrom(&*mmio_);
  delay2.ReadFrom(&*mmio_);

  EXPECT_EQ(adjust.adj_delay(), 7);
  EXPECT_EQ(delay1.dly_0(), 0);
  EXPECT_EQ(delay1.dly_1(), 0);
  EXPECT_EQ(delay1.dly_2(), 0);
  EXPECT_EQ(delay1.dly_3(), 0);
  EXPECT_EQ(delay1.dly_4(), 0);
  EXPECT_EQ(delay2.dly_5(), 0);
  EXPECT_EQ(delay2.dly_6(), 0);
  EXPECT_EQ(delay2.dly_7(), 0);
  EXPECT_EQ(delay2.dly_8(), 0);
  EXPECT_EQ(delay2.dly_9(), 0);

  dut_->ExpectInspectPropertyValue("adj_delay", 7);

  dut_->ExpectInspectPropertyValue("delay_lines", 0);

  dut_->ExpectInspectPropertyValue(
      "tuning_results_adj_delay_7", "tuning_results",
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||");

  dut_->ExpectInspectPropertyValue("distance_to_failing_point", 63);
}

TEST_F(AmlSdmmcTest, DelayLineTuningCorrectFailingWindowIfLastOne) {
  StartDriver();

  dut_->SetRequestResults(
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||----"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||"
      "||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||||");

  dut_->set_board_config({
      .min_freq = 400000,
      .max_freq = 120000000,
      .version_3 = true,
      .prefs = 0,
  });
  ASSERT_OK(dut_->Init({}));

  AmlSdmmcClock::Get().FromValue(0).set_cfg_div(5).WriteTo(&*mmio_);
  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_bus_width(AmlSdmmcCfg::kBusWidth4Bit).WriteTo(&*mmio_);

  auto adjust = AmlSdmmcAdjust::Get().FromValue(0).set_adj_delay(0x3f).WriteTo(&*mmio_);
  auto delay1 = AmlSdmmcDelay1::Get().FromValue(0).WriteTo(&*mmio_);
  auto delay2 = AmlSdmmcDelay2::Get().FromValue(0).WriteTo(&*mmio_);

  EXPECT_OK(dut_->SdmmcPerformTuning(SD_SEND_TUNING_BLOCK));

  adjust.ReadFrom(&*mmio_);
  delay1.ReadFrom(&*mmio_);
  delay2.ReadFrom(&*mmio_);

  EXPECT_EQ(adjust.adj_delay(), 2);
  EXPECT_EQ(delay1.dly_0(), 60);
  EXPECT_EQ(delay1.dly_1(), 60);
  EXPECT_EQ(delay1.dly_2(), 60);
  EXPECT_EQ(delay1.dly_3(), 60);
  EXPECT_EQ(delay1.dly_4(), 60);
  EXPECT_EQ(delay2.dly_5(), 60);
  EXPECT_EQ(delay2.dly_6(), 60);
  EXPECT_EQ(delay2.dly_7(), 60);
  EXPECT_EQ(delay2.dly_8(), 60);
  EXPECT_EQ(delay2.dly_9(), 60);

  dut_->ExpectInspectPropertyValue("adj_delay", 2);

  dut_->ExpectInspectPropertyValue("delay_lines", 60);
}

TEST_F(AmlSdmmcTest, SetBusFreq) {
  StartDriver();

  ASSERT_OK(dut_->Init({}));
  dut_->ExpectInspectPropertyValue("bus_clock_frequency", 400'000);

  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_bus_width(AmlSdmmcCfg::kBusWidth4Bit).WriteTo(&*mmio_);

  auto clock = AmlSdmmcClock::Get().FromValue(0).WriteTo(&*mmio_);

  EXPECT_OK(dut_->SdmmcSetBusFreq(100'000'000));
  EXPECT_EQ(clock.ReadFrom(&*mmio_).cfg_div(), 10);
  EXPECT_EQ(clock.cfg_src(), 1);
  dut_->ExpectInspectPropertyValue("bus_clock_frequency", 100'000'000);

  // Greater than the max, will be capped at 120 MHz.
  EXPECT_OK(dut_->SdmmcSetBusFreq(200'000'000));
  EXPECT_EQ(clock.ReadFrom(&*mmio_).cfg_div(), 9);
  EXPECT_EQ(clock.cfg_src(), 1);
  dut_->ExpectInspectPropertyValue("bus_clock_frequency", 111'111'111);

  EXPECT_OK(dut_->SdmmcSetBusFreq(0));
  EXPECT_EQ(clock.ReadFrom(&*mmio_).cfg_div(), 0);
  dut_->ExpectInspectPropertyValue("bus_clock_frequency", 0);

  EXPECT_OK(dut_->SdmmcSetBusFreq(54'000'000));
  EXPECT_EQ(clock.ReadFrom(&*mmio_).cfg_div(), 19);
  EXPECT_EQ(clock.cfg_src(), 1);
  dut_->ExpectInspectPropertyValue("bus_clock_frequency", 52'631'578);

  EXPECT_OK(dut_->SdmmcSetBusFreq(400'000));
  EXPECT_EQ(clock.ReadFrom(&*mmio_).cfg_div(), 60);
  EXPECT_EQ(clock.cfg_src(), 0);
  dut_->ExpectInspectPropertyValue("bus_clock_frequency", 400'000);
}

TEST_F(AmlSdmmcTest, ClearStatus) {
  StartDriver();

  ASSERT_OK(dut_->Init({}));

  // Set end_of_chain to indicate we're done and to have something to clear
  dut_->SetRequestInterruptStatus(1 << 13);
  sdmmc_req_t request;
  memset(&request, 0, sizeof(request));
  uint32_t unused_response[4];
  EXPECT_OK(dut_->SdmmcRequest(&request, unused_response));

  auto status = AmlSdmmcStatus::Get().FromValue(0);
  EXPECT_EQ(AmlSdmmcStatus::kClearStatus, status.ReadFrom(&*mmio_).reg_value());
}

TEST_F(AmlSdmmcTest, TxCrcError) {
  StartDriver();

  ASSERT_OK(dut_->Init({}));

  // Set TX CRC error bit (8) and desc_busy bit (30)
  dut_->SetRequestInterruptStatus(1 << 8 | 1 << 30);
  sdmmc_req_t request;
  memset(&request, 0, sizeof(request));
  uint32_t unused_response[4];
  EXPECT_EQ(ZX_ERR_IO_DATA_INTEGRITY, dut_->SdmmcRequest(&request, unused_response));

  auto start = AmlSdmmcStart::Get().FromValue(0);
  // The desc busy bit should now have been cleared because of the error
  EXPECT_EQ(0, start.ReadFrom(&*mmio_).desc_busy());
}

TEST_F(AmlSdmmcTest, UnownedVmosBlockMode) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(10);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmos[10] = {};
  sdmmc_buffer_region_t buffers[10];
  for (uint32_t i = 0; i < std::size(vmos); i++) {
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmos[i]));
    buffers[i] = {
        .buffer =
            {
                .vmo = vmos[i].get(),
            },
        .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
        .offset = i * 16,
        .size = 32 * (i + 2),
    };
  }

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(2)
                               .set_block_mode(1)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, zx_system_get_page_size());
  EXPECT_EQ(descs[0].resp_addr, 0);

  for (uint32_t i = 1; i < std::size(vmos); i++) {
    expected_desc_cfg.set_len(i + 2).set_no_resp(1).set_no_cmd(1).set_resp_num(0).set_cmd_idx(0);
    if (i == std::size(vmos) - 1) {
      expected_desc_cfg.set_end_of_chain(1);
    }
    EXPECT_EQ(descs[i].cmd_info, expected_desc_cfg.reg_value());
    EXPECT_EQ(descs[i].cmd_arg, 0);
    EXPECT_EQ(descs[i].data_addr, (i << 24) | (zx_system_get_page_size() + (i * 16)));
    EXPECT_EQ(descs[i].resp_addr, 0);
  }
}

TEST_F(AmlSdmmcTest, UnownedVmosNotBlockSizeMultiple) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(10);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmos[10] = {};
  sdmmc_buffer_region_t buffers[10];
  for (uint32_t i = 0; i < std::size(vmos); i++) {
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmos[i]));
    buffers[i] = {
        .buffer =
            {
                .vmo = vmos[i].get(),
            },
        .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
        .offset = 0,
        .size = 32 * (i + 2),
    };
  }

  buffers[5].size = 25;

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  EXPECT_NOT_OK(dut_->SdmmcRequest(&request, response));
}

TEST_F(AmlSdmmcTest, UnownedVmosByteMode) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(10);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmos[10] = {};
  sdmmc_buffer_region_t buffers[10];
  for (uint32_t i = 0; i < std::size(vmos); i++) {
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmos[i]));
    buffers[i] = {
        .buffer =
            {
                .vmo = vmos[i].get(),
            },
        .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
        .offset = i * 4,
        .size = 50,
    };
  }

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 50,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(50)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, zx_system_get_page_size());
  EXPECT_EQ(descs[0].resp_addr, 0);

  for (uint32_t i = 1; i < std::size(vmos); i++) {
    expected_desc_cfg.set_len(50).set_no_resp(1).set_no_cmd(1).set_resp_num(0).set_cmd_idx(0);
    if (i == std::size(vmos) - 1) {
      expected_desc_cfg.set_end_of_chain(1);
    }
    EXPECT_EQ(descs[i].cmd_info, expected_desc_cfg.reg_value());
    EXPECT_EQ(descs[i].cmd_arg, 0);
    EXPECT_EQ(descs[i].data_addr, (i << 24) | (zx_system_get_page_size() + (i * 4)));
    EXPECT_EQ(descs[i].resp_addr, 0);
  }
}

TEST_F(AmlSdmmcTest, UnownedVmoByteModeMultiBlock) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(1);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 0,
      .size = 400,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 100,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(100)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, zx_system_get_page_size());
  EXPECT_EQ(descs[0].resp_addr, 0);

  for (uint32_t i = 1; i < 4; i++) {
    expected_desc_cfg.set_no_resp(1).set_no_cmd(1).set_resp_num(0).set_cmd_idx(0);
    if (i == 3) {
      expected_desc_cfg.set_end_of_chain(1);
    }
    EXPECT_EQ(descs[i].cmd_info, expected_desc_cfg.reg_value());
    EXPECT_EQ(descs[i].cmd_arg, 0);
    EXPECT_EQ(descs[i].data_addr, zx_system_get_page_size() + (i * 100));
    EXPECT_EQ(descs[i].resp_addr, 0);
  }
}

TEST_F(AmlSdmmcTest, UnownedVmoOffsetNotAligned) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(1);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 3,
      .size = 64,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_NOT_OK(dut_->SdmmcRequest(&request, response));
}

TEST_F(AmlSdmmcTest, UnownedVmoSingleBufferMultipleDescriptors) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  const size_t pages = ((32 * 514) / zx_system_get_page_size()) + 1;
  InitializeSingleVmoPaddrs(pages);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(pages * zx_system_get_page_size(), 0, &vmo));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 16,
      .size = 32 * 513,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(511)
                               .set_block_mode(1)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, zx_system_get_page_size() + 16);
  EXPECT_EQ(descs[0].resp_addr, 0);

  expected_desc_cfg.set_len(2)
      .set_end_of_chain(1)
      .set_no_resp(1)
      .set_no_cmd(1)
      .set_resp_num(0)
      .set_cmd_idx(0);

  EXPECT_EQ(descs[1].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[1].cmd_arg, 0);
  EXPECT_EQ(descs[1].data_addr, zx_system_get_page_size() + (511 * 32) + 16);
  EXPECT_EQ(descs[1].resp_addr, 0);
}

TEST_F(AmlSdmmcTest, UnownedVmoSingleBufferNotPageAligned) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  const size_t pages = ((32 * 514) / zx_system_get_page_size()) + 1;
  InitializeNonContiguousPaddrs(pages);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(pages * zx_system_get_page_size(), 0, &vmo));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 16,
      .size = 32 * 513,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_NOT_OK(dut_->SdmmcRequest(&request, response));
}

TEST_F(AmlSdmmcTest, UnownedVmoSingleBufferPageAligned) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  const size_t pages = ((32 * 514) / zx_system_get_page_size()) + 1;
  InitializeNonContiguousPaddrs(pages);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(pages * zx_system_get_page_size(), 0, &vmo));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 32,
      .size = 32 * 513,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(127)
                               .set_block_mode(1)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, (zx_system_get_page_size() * 2) + 32);
  EXPECT_EQ(descs[0].resp_addr, 0);

  for (uint32_t i = 1; i < 5; i++) {
    expected_desc_cfg.set_len(128).set_no_resp(1).set_no_cmd(1).set_resp_num(0).set_cmd_idx(0);
    if (i == 4) {
      expected_desc_cfg.set_len(2).set_end_of_chain(1);
    }

    EXPECT_EQ(descs[i].cmd_info, expected_desc_cfg.reg_value());
    EXPECT_EQ(descs[i].cmd_arg, 0);
    EXPECT_EQ(descs[i].data_addr, zx_system_get_page_size() * (i + 1) * 2);
    EXPECT_EQ(descs[i].resp_addr, 0);
  }
}

TEST_F(AmlSdmmcTest, OwnedVmosBlockMode) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(10);

  ASSERT_OK(dut_->Init({}));

  sdmmc_buffer_region_t buffers[10];
  for (uint32_t i = 0; i < std::size(buffers); i++) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
    EXPECT_OK(dut_->SdmmcRegisterVmo(i, 0, std::move(vmo), i * 64, 512, SDMMC_VMO_RIGHT_WRITE));
    buffers[i] = {
        .buffer =
            {
                .vmo_id = i,
            },
        .type = SDMMC_BUFFER_TYPE_VMO_ID,
        .offset = i * 16,
        .size = 32 * (i + 2),
    };
  }

  zx::vmo vmo;
  EXPECT_NOT_OK(dut_->SdmmcUnregisterVmo(3, 1, &vmo));

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(2)
                               .set_block_mode(1)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, zx_system_get_page_size());
  EXPECT_EQ(descs[0].resp_addr, 0);

  for (uint32_t i = 1; i < std::size(buffers); i++) {
    expected_desc_cfg.set_len(i + 2).set_no_resp(1).set_no_cmd(1).set_resp_num(0).set_cmd_idx(0);
    if (i == std::size(buffers) - 1) {
      expected_desc_cfg.set_end_of_chain(1);
    }
    EXPECT_EQ(descs[i].cmd_info, expected_desc_cfg.reg_value());
    EXPECT_EQ(descs[i].cmd_arg, 0);
    EXPECT_EQ(descs[i].data_addr, (i << 24) | (zx_system_get_page_size() + (i * 80)));
    EXPECT_EQ(descs[i].resp_addr, 0);
  }

  request.client_id = 7;
  EXPECT_NOT_OK(dut_->SdmmcRequest(&request, response));

  EXPECT_OK(dut_->SdmmcUnregisterVmo(3, 0, &vmo));
  EXPECT_NOT_OK(dut_->SdmmcRegisterVmo(2, 0, std::move(vmo), 0, 512, SDMMC_VMO_RIGHT_WRITE));

  request.client_id = 0;
  EXPECT_NOT_OK(dut_->SdmmcRequest(&request, response));
}

TEST_F(AmlSdmmcTest, OwnedVmosNotBlockSizeMultiple) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(10);

  ASSERT_OK(dut_->Init({}));

  sdmmc_buffer_region_t buffers[10];
  for (uint32_t i = 0; i < std::size(buffers); i++) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
    EXPECT_OK(dut_->SdmmcRegisterVmo(i, 0, std::move(vmo), i * 64, 512, SDMMC_VMO_RIGHT_WRITE));
    buffers[i] = {
        .buffer =
            {
                .vmo_id = i,
            },
        .type = SDMMC_BUFFER_TYPE_VMO_ID,
        .offset = 0,
        .size = 32 * (i + 2),
    };
  }

  buffers[5].size = 25;

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  EXPECT_NOT_OK(dut_->SdmmcRequest(&request, response));
}

TEST_F(AmlSdmmcTest, OwnedVmosByteMode) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(10);

  ASSERT_OK(dut_->Init({}));

  sdmmc_buffer_region_t buffers[10];
  for (uint32_t i = 0; i < std::size(buffers); i++) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
    EXPECT_OK(dut_->SdmmcRegisterVmo(i, 0, std::move(vmo), i * 64, 512, SDMMC_VMO_RIGHT_WRITE));
    buffers[i] = {
        .buffer =
            {
                .vmo_id = i,
            },
        .type = SDMMC_BUFFER_TYPE_VMO_ID,
        .offset = i * 4,
        .size = 50,
    };
  }

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 50,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(50)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, zx_system_get_page_size());
  EXPECT_EQ(descs[0].resp_addr, 0);

  for (uint32_t i = 1; i < std::size(buffers); i++) {
    expected_desc_cfg.set_len(50).set_no_resp(1).set_no_cmd(1).set_resp_num(0).set_cmd_idx(0);
    if (i == std::size(buffers) - 1) {
      expected_desc_cfg.set_end_of_chain(1);
    }
    EXPECT_EQ(descs[i].cmd_info, expected_desc_cfg.reg_value());
    EXPECT_EQ(descs[i].cmd_arg, 0);
    EXPECT_EQ(descs[i].data_addr, (i << 24) | (zx_system_get_page_size() + (i * 68)));
    EXPECT_EQ(descs[i].resp_addr, 0);
  }
}

TEST_F(AmlSdmmcTest, OwnedVmoByteModeMultiBlock) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(1);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  EXPECT_OK(dut_->SdmmcRegisterVmo(1, 0, std::move(vmo), 0, 512, SDMMC_VMO_RIGHT_WRITE));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo_id = 1,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = 0,
      .size = 400,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 100,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(100)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, zx_system_get_page_size());
  EXPECT_EQ(descs[0].resp_addr, 0);

  for (uint32_t i = 1; i < 4; i++) {
    expected_desc_cfg.set_no_resp(1).set_no_cmd(1).set_resp_num(0).set_cmd_idx(0);
    if (i == 3) {
      expected_desc_cfg.set_end_of_chain(1);
    }
    EXPECT_EQ(descs[i].cmd_info, expected_desc_cfg.reg_value());
    EXPECT_EQ(descs[i].cmd_arg, 0);
    EXPECT_EQ(descs[i].data_addr, zx_system_get_page_size() + (i * 100));
    EXPECT_EQ(descs[i].resp_addr, 0);
  }
}

TEST_F(AmlSdmmcTest, OwnedVmoOffsetNotAligned) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(1);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  EXPECT_OK(dut_->SdmmcRegisterVmo(1, 0, std::move(vmo), 2, 512, SDMMC_VMO_RIGHT_WRITE));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo_id = 1,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = 32,
      .size = 64,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_NOT_OK(dut_->SdmmcRequest(&request, response));
}

TEST_F(AmlSdmmcTest, OwnedVmoSingleBufferMultipleDescriptors) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  const size_t pages = ((32 * 514) / zx_system_get_page_size()) + 1;
  InitializeSingleVmoPaddrs(pages);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(pages * zx_system_get_page_size(), 0, &vmo));
  EXPECT_OK(dut_->SdmmcRegisterVmo(1, 0, std::move(vmo), 8, (pages * zx_system_get_page_size()) - 8,
                                   SDMMC_VMO_RIGHT_WRITE));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo_id = 1,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = 8,
      .size = 32 * 513,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(511)
                               .set_block_mode(1)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, zx_system_get_page_size() + 16);
  EXPECT_EQ(descs[0].resp_addr, 0);

  expected_desc_cfg.set_len(1)
      .set_len(2)
      .set_end_of_chain(1)
      .set_no_resp(1)
      .set_no_cmd(1)
      .set_resp_num(0)
      .set_cmd_idx(0);

  EXPECT_EQ(descs[1].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[1].cmd_arg, 0);
  EXPECT_EQ(descs[1].data_addr, zx_system_get_page_size() + (511 * 32) + 16);
  EXPECT_EQ(descs[1].resp_addr, 0);
}

TEST_F(AmlSdmmcTest, OwnedVmoSingleBufferNotPageAligned) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  const size_t pages = ((32 * 514) / zx_system_get_page_size()) + 1;
  InitializeNonContiguousPaddrs(pages);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(pages * zx_system_get_page_size(), 0, &vmo));
  EXPECT_OK(dut_->SdmmcRegisterVmo(1, 0, std::move(vmo), 8, (pages * zx_system_get_page_size()) - 8,
                                   SDMMC_VMO_RIGHT_WRITE));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = 1,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = 8,
      .size = 32 * 513,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_NOT_OK(dut_->SdmmcRequest(&request, response));
}

TEST_F(AmlSdmmcTest, OwnedVmoSingleBufferPageAligned) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  const size_t pages = ((32 * 514) / zx_system_get_page_size()) + 1;
  InitializeNonContiguousPaddrs(pages);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(pages * zx_system_get_page_size(), 0, &vmo));
  EXPECT_OK(dut_->SdmmcRegisterVmo(
      1, 0, std::move(vmo), 16, (pages * zx_system_get_page_size()) - 16, SDMMC_VMO_RIGHT_WRITE));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = 1,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = 16,
      .size = 32 * 513,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(127)
                               .set_block_mode(1)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, (zx_system_get_page_size() * 2) + 32);
  EXPECT_EQ(descs[0].resp_addr, 0);

  for (uint32_t i = 1; i < 5; i++) {
    expected_desc_cfg.set_len(128).set_no_resp(1).set_no_cmd(1).set_resp_num(0).set_cmd_idx(0);
    if (i == 4) {
      expected_desc_cfg.set_len(2).set_end_of_chain(1);
    }

    EXPECT_EQ(descs[i].cmd_info, expected_desc_cfg.reg_value());
    EXPECT_EQ(descs[i].cmd_arg, 0);
    EXPECT_EQ(descs[i].data_addr, zx_system_get_page_size() * (i + 1) * 2);
    EXPECT_EQ(descs[i].resp_addr, 0);
  }
}

TEST_F(AmlSdmmcTest, OwnedVmoWritePastEnd) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  const size_t pages = ((32 * 514) / zx_system_get_page_size()) + 1;
  InitializeNonContiguousPaddrs(pages);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(pages * zx_system_get_page_size(), 0, &vmo));
  EXPECT_OK(dut_->SdmmcRegisterVmo(1, 0, std::move(vmo), 32, 32 * 384, SDMMC_VMO_RIGHT_WRITE));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = 1,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = 32,
      .size = 32 * 383,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(126)
                               .set_block_mode(1)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, (zx_system_get_page_size() * 2) + 64);
  EXPECT_EQ(descs[0].resp_addr, 0);

  for (uint32_t i = 1; i < 4; i++) {
    expected_desc_cfg.set_len(128).set_no_resp(1).set_no_cmd(1).set_resp_num(0).set_cmd_idx(0);
    if (i == 3) {
      expected_desc_cfg.set_len(1).set_end_of_chain(1);
    }

    EXPECT_EQ(descs[i].cmd_info, expected_desc_cfg.reg_value());
    EXPECT_EQ(descs[i].cmd_arg, 0);
    EXPECT_EQ(descs[i].data_addr, zx_system_get_page_size() * (i + 1) * 2);
    EXPECT_EQ(descs[i].resp_addr, 0);
  }

  buffer.size = 32 * 384;
  EXPECT_NOT_OK(dut_->SdmmcRequest(&request, response));
}

TEST_F(AmlSdmmcTest, SeparateClientVmoSpaces) {
  StartDriver();

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  const zx_koid_t vmo1_koid = GetVmoKoid(vmo);
  EXPECT_NE(vmo1_koid, ZX_KOID_INVALID);
  EXPECT_OK(dut_->SdmmcRegisterVmo(1, 0, std::move(vmo), 0, zx_system_get_page_size(),
                                   SDMMC_VMO_RIGHT_WRITE));

  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  const zx_koid_t vmo2_koid = GetVmoKoid(vmo);
  EXPECT_NE(vmo2_koid, ZX_KOID_INVALID);
  EXPECT_OK(dut_->SdmmcRegisterVmo(2, 0, std::move(vmo), 0, zx_system_get_page_size(),
                                   SDMMC_VMO_RIGHT_WRITE));

  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  EXPECT_NOT_OK(dut_->SdmmcRegisterVmo(1, 0, std::move(vmo), 0, zx_system_get_page_size(),
                                       SDMMC_VMO_RIGHT_WRITE));

  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  EXPECT_NOT_OK(dut_->SdmmcRegisterVmo(1, 8, std::move(vmo), 0, zx_system_get_page_size(),
                                       SDMMC_VMO_RIGHT_WRITE));

  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  const zx_koid_t vmo3_koid = GetVmoKoid(vmo);
  EXPECT_NE(vmo3_koid, ZX_KOID_INVALID);
  EXPECT_OK(dut_->SdmmcRegisterVmo(1, 1, std::move(vmo), 0, zx_system_get_page_size(),
                                   SDMMC_VMO_RIGHT_WRITE));

  EXPECT_OK(dut_->SdmmcUnregisterVmo(1, 0, &vmo));
  EXPECT_EQ(GetVmoKoid(vmo), vmo1_koid);

  EXPECT_OK(dut_->SdmmcUnregisterVmo(2, 0, &vmo));
  EXPECT_EQ(GetVmoKoid(vmo), vmo2_koid);

  EXPECT_OK(dut_->SdmmcUnregisterVmo(1, 1, &vmo));
  EXPECT_EQ(GetVmoKoid(vmo), vmo3_koid);

  EXPECT_NOT_OK(dut_->SdmmcUnregisterVmo(1, 0, &vmo));
  EXPECT_NOT_OK(dut_->SdmmcUnregisterVmo(2, 0, &vmo));
  EXPECT_NOT_OK(dut_->SdmmcUnregisterVmo(1, 1, &vmo));
}

TEST_F(AmlSdmmcTest, RequestWithOwnedAndUnownedVmos) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(10);

  ASSERT_OK(dut_->Init({}));

  zx::vmo vmos[5] = {};
  sdmmc_buffer_region_t buffers[10];
  for (uint32_t i = 0; i < 5; i++) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmos[i]));

    EXPECT_OK(dut_->SdmmcRegisterVmo(i, 0, std::move(vmo), i * 64, 512, SDMMC_VMO_RIGHT_WRITE));
    buffers[i * 2] = {
        .buffer =
            {
                .vmo_id = i,
            },
        .type = SDMMC_BUFFER_TYPE_VMO_ID,
        .offset = i * 16,
        .size = 32 * (i + 2),
    };
    buffers[(i * 2) + 1] = {
        .buffer =
            {
                .vmo = vmos[i].get(),
            },
        .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
        .offset = i * 16,
        .size = 32 * (i + 2),
    };
  }

  zx::vmo vmo;
  EXPECT_NOT_OK(dut_->SdmmcUnregisterVmo(3, 1, &vmo));

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  AmlSdmmcCmdResp::Get().FromValue(0xfedc9876).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(response[0], 0xfedc9876);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(2)
                               .set_block_mode(1)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDMMC_READ_MULTIPLE_BLOCK)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x1234abcd);
  EXPECT_EQ(descs[0].data_addr, zx_system_get_page_size());
  EXPECT_EQ(descs[0].resp_addr, 0);

  expected_desc_cfg.set_no_resp(1).set_no_cmd(1).set_resp_num(0).set_cmd_idx(0);
  EXPECT_EQ(descs[1].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[1].cmd_arg, 0);
  EXPECT_EQ(descs[1].data_addr, (5 << 24) | zx_system_get_page_size());
  EXPECT_EQ(descs[1].resp_addr, 0);

  expected_desc_cfg.set_len(3);
  EXPECT_EQ(descs[2].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[2].cmd_arg, 0);
  EXPECT_EQ(descs[2].data_addr, (1 << 24) | (zx_system_get_page_size() + 64 + 16));
  EXPECT_EQ(descs[2].resp_addr, 0);

  EXPECT_EQ(descs[3].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[3].cmd_arg, 0);
  EXPECT_EQ(descs[3].data_addr, (6 << 24) | (zx_system_get_page_size() + 16));
  EXPECT_EQ(descs[3].resp_addr, 0);

  expected_desc_cfg.set_len(4);
  EXPECT_EQ(descs[4].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[4].cmd_arg, 0);
  EXPECT_EQ(descs[4].data_addr, (2 << 24) | (zx_system_get_page_size() + 128 + 32));
  EXPECT_EQ(descs[4].resp_addr, 0);

  EXPECT_EQ(descs[5].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[5].cmd_arg, 0);
  EXPECT_EQ(descs[5].data_addr, (7 << 24) | (zx_system_get_page_size() + 32));
  EXPECT_EQ(descs[5].resp_addr, 0);

  expected_desc_cfg.set_len(5);
  EXPECT_EQ(descs[6].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[6].cmd_arg, 0);
  EXPECT_EQ(descs[6].data_addr, (3 << 24) | (zx_system_get_page_size() + 192 + 48));
  EXPECT_EQ(descs[6].resp_addr, 0);

  EXPECT_EQ(descs[7].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[7].cmd_arg, 0);
  EXPECT_EQ(descs[7].data_addr, (8 << 24) | (zx_system_get_page_size() + 48));
  EXPECT_EQ(descs[7].resp_addr, 0);

  expected_desc_cfg.set_len(6);
  EXPECT_EQ(descs[8].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[8].cmd_arg, 0);
  EXPECT_EQ(descs[8].data_addr, (4 << 24) | (zx_system_get_page_size() + 256 + 64));
  EXPECT_EQ(descs[8].resp_addr, 0);

  expected_desc_cfg.set_end_of_chain(1);
  EXPECT_EQ(descs[9].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[9].cmd_arg, 0);
  EXPECT_EQ(descs[9].data_addr, (9 << 24) | (zx_system_get_page_size() + 64));
  EXPECT_EQ(descs[9].resp_addr, 0);
}

TEST_F(AmlSdmmcTest, ResetCmdInfoBits) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);

  ASSERT_OK(dut_->Init({}));

  // Start at 1 because one paddr has already been read to create the DMA descriptor buffer.
  bti_paddrs_[1] = 0x1897'7000;
  bti_paddrs_[2] = 0x1997'8000;
  bti_paddrs_[3] = 0x1997'e000;

  // Make sure the appropriate cmd_info bits get cleared.
  descriptors()[0].cmd_info = 0xffff'ffff;
  descriptors()[1].cmd_info = 0xffff'ffff;
  descriptors()[2].cmd_info = 0xffff'ffff;

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size() * 3, 0, &vmo));
  EXPECT_OK(dut_->SdmmcRegisterVmo(1, 2, std::move(vmo), 0, zx_system_get_page_size() * 3,
                                   SDMMC_VMO_RIGHT_WRITE));

  sdmmc_buffer_region_t buffer = {
      .buffer = {.vmo_id = 1},
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = 0,
      .size = 10752,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDIO_IO_RW_DIRECT_EXTENDED,
      .cmd_flags = SDIO_IO_RW_DIRECT_EXTENDED_FLAGS | SDMMC_CMD_READ,
      .arg = 0x29000015,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 2,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  AmlSdmmcCfg::Get().ReadFrom(&*mmio_).set_blk_len(0).WriteTo(&*mmio_);
  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  EXPECT_EQ(AmlSdmmcCfg::Get().ReadFrom(&*mmio_).blk_len(), 9);

  const aml_sdmmc_desc_t* descs = descriptors();
  auto expected_desc_cfg = AmlSdmmcCmdCfg::Get()
                               .FromValue(0)
                               .set_len(8)
                               .set_block_mode(1)
                               .set_timeout(AmlSdmmcCmdCfg::kDefaultCmdTimeout)
                               .set_data_io(1)
                               .set_data_wr(0)
                               .set_resp_num(1)
                               .set_cmd_idx(SDIO_IO_RW_DIRECT_EXTENDED)
                               .set_owner(1);

  EXPECT_EQ(descs[0].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[0].cmd_arg, 0x29000015);
  EXPECT_EQ(descs[0].data_addr, 0x1897'7000);
  EXPECT_EQ(descs[0].resp_addr, 0);

  expected_desc_cfg.set_no_resp(1).set_no_cmd(1).set_resp_num(0).set_cmd_idx(0);
  EXPECT_EQ(descs[1].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[1].cmd_arg, 0);
  EXPECT_EQ(descs[1].data_addr, 0x1997'8000);
  EXPECT_EQ(descs[1].resp_addr, 0);

  expected_desc_cfg.set_len(5).set_end_of_chain(1);
  EXPECT_EQ(descs[2].cmd_info, expected_desc_cfg.reg_value());
  EXPECT_EQ(descs[2].cmd_arg, 0);
  EXPECT_EQ(descs[2].data_addr, 0x1997'e000);
  EXPECT_EQ(descs[2].resp_addr, 0);
}

TEST_F(AmlSdmmcTest, WriteToReadOnlyVmo) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(10);

  ASSERT_OK(dut_->Init({}));

  sdmmc_buffer_region_t buffers[10];
  for (uint32_t i = 0; i < std::size(buffers); i++) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
    const uint32_t vmo_rights = SDMMC_VMO_RIGHT_READ | (i == 5 ? 0 : SDMMC_VMO_RIGHT_WRITE);
    EXPECT_OK(dut_->SdmmcRegisterVmo(i, 0, std::move(vmo), i * 64, 512, vmo_rights));
    buffers[i] = {
        .buffer =
            {
                .vmo_id = i,
            },
        .type = SDMMC_BUFFER_TYPE_VMO_ID,
        .offset = 0,
        .size = 32 * (i + 2),
    };
  }

  sdmmc_req_t request = {
      .cmd_idx = SDIO_IO_RW_DIRECT_EXTENDED,
      .cmd_flags = SDIO_IO_RW_DIRECT_EXTENDED_FLAGS | SDMMC_CMD_READ,
      .arg = 0x29000015,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  EXPECT_NOT_OK(dut_->SdmmcRequest(&request, response));
}

TEST_F(AmlSdmmcTest, ReadFromWriteOnlyVmo) {
  StartDriver(/*create_fake_bti_with_paddrs=*/true);
  InitializeContiguousPaddrs(10);

  ASSERT_OK(dut_->Init({}));

  sdmmc_buffer_region_t buffers[10];
  for (uint32_t i = 0; i < std::size(buffers); i++) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
    const uint32_t vmo_rights = SDMMC_VMO_RIGHT_WRITE | (i == 5 ? 0 : SDMMC_VMO_RIGHT_READ);
    EXPECT_OK(dut_->SdmmcRegisterVmo(i, 0, std::move(vmo), i * 64, 512, vmo_rights));
    buffers[i] = {
        .buffer =
            {
                .vmo_id = i,
            },
        .type = SDMMC_BUFFER_TYPE_VMO_ID,
        .offset = 0,
        .size = 32 * (i + 2),
    };
  }

  sdmmc_req_t request = {
      .cmd_idx = SDIO_IO_RW_DIRECT_EXTENDED,
      .cmd_flags = SDIO_IO_RW_DIRECT_EXTENDED_FLAGS,
      .arg = 0x29000015,
      .blocksize = 32,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  EXPECT_NOT_OK(dut_->SdmmcRequest(&request, response));
}

TEST_F(AmlSdmmcTest, ConsecutiveErrorLogging) {
  StartDriver();

  ASSERT_OK(dut_->Init({}));

  // First data error.
  dut_->SetRequestInterruptStatus(1 << 8);
  sdmmc_req_t request;
  memset(&request, 0, sizeof(request));
  uint32_t unused_response[4];
  EXPECT_EQ(ZX_ERR_IO_DATA_INTEGRITY, dut_->SdmmcRequest(&request, unused_response));

  // First cmd error.
  dut_->SetRequestInterruptStatus(1 << 11);
  memset(&request, 0, sizeof(request));
  EXPECT_EQ(ZX_ERR_TIMED_OUT, dut_->SdmmcRequest(&request, unused_response));

  // Second data error.
  dut_->SetRequestInterruptStatus(1 << 7);
  memset(&request, 0, sizeof(request));
  EXPECT_EQ(ZX_ERR_IO_DATA_INTEGRITY, dut_->SdmmcRequest(&request, unused_response));

  // Second cmd error.
  dut_->SetRequestInterruptStatus(1 << 11);
  memset(&request, 0, sizeof(request));
  EXPECT_EQ(ZX_ERR_TIMED_OUT, dut_->SdmmcRequest(&request, unused_response));

  zx::vmo vmo;
  EXPECT_OK(zx::vmo::create(32, 0, &vmo));

  // cmd/data goes through.
  const sdmmc_buffer_region_t region{
      .buffer = {.vmo = vmo.get()},
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 0,
      .size = 32,
  };
  dut_->SetRequestInterruptStatus(1 << 13);
  memset(&request, 0, sizeof(request));
  request.cmd_flags = SDMMC_RESP_DATA_PRESENT;  // Must be set to clear the data error count.
  request.blocksize = 32;
  request.buffers_list = &region;
  request.buffers_count = 1;
  EXPECT_OK(dut_->SdmmcRequest(&request, unused_response));

  // Third data error.
  dut_->SetRequestInterruptStatus(1 << 7);
  memset(&request, 0, sizeof(request));
  EXPECT_EQ(ZX_ERR_IO_DATA_INTEGRITY, dut_->SdmmcRequest(&request, unused_response));

  // Third cmd error.
  dut_->SetRequestInterruptStatus(1 << 11);
  memset(&request, 0, sizeof(request));
  EXPECT_EQ(ZX_ERR_TIMED_OUT, dut_->SdmmcRequest(&request, unused_response));
}

TEST_F(AmlSdmmcTest, PowerSuspendResume) {
  StartDriver();

  auto clock = AmlSdmmcClock::Get().FromValue(0).WriteTo(&*mmio_);

  ASSERT_OK(dut_->Init({}));
  EXPECT_FALSE(dut_->power_suspended());
  EXPECT_NE(clock.ReadFrom(&*mmio_).cfg_div(), 0);
  EXPECT_TRUE(incoming_.SyncCall(
      [](IncomingNamespace* incoming) { return incoming->clock_server.enabled(); }));

  EXPECT_OK(dut_->SuspendPower());
  EXPECT_TRUE(dut_->power_suspended());
  EXPECT_EQ(clock.ReadFrom(&*mmio_).cfg_div(), 0);
  EXPECT_FALSE(incoming_.SyncCall(
      [](IncomingNamespace* incoming) { return incoming->clock_server.enabled(); }));

  EXPECT_OK(dut_->ResumePower());
  EXPECT_FALSE(dut_->power_suspended());
  EXPECT_NE(clock.ReadFrom(&*mmio_).cfg_div(), 0);
  EXPECT_TRUE(incoming_.SyncCall(
      [](IncomingNamespace* incoming) { return incoming->clock_server.enabled(); }));
}

}  // namespace aml_sdmmc

FUCHSIA_DRIVER_EXPORT(aml_sdmmc::TestDfv2Driver);
