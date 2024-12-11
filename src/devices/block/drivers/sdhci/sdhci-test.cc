// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdhci.h"

#include <lib/async/default.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/fake-bti/cpp/fake-bti.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/mmio-ptr/fake.h>
#include <lib/sync/completion.h>

#include <atomic>
#include <memory>
#include <optional>
#include <vector>

#include <gtest/gtest.h>

#include "src/devices/lib/mmio/test-helper.h"
#include "src/lib/testing/predicates/status.h"

// Stub out vmo_op_range to allow tests to use fake VMOs.
__EXPORT
zx_status_t zx_vmo_op_range(zx_handle_t handle, uint32_t op, uint64_t offset, uint64_t size,
                            void* buffer, size_t buffer_size) {
  return ZX_OK;
}

namespace {

zx_paddr_t PageMask() { return static_cast<uintptr_t>(zx_system_get_page_size()) - 1; }

}  // namespace

namespace sdhci {

class TestSdhci : public Sdhci {
 public:
  // Modify to configure the behaviour of this test driver.
  static fdf::MmioBuffer* mmio_;

  TestSdhci(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : Sdhci(std::move(start_args), std::move(dispatcher)) {}

  zx_status_t SdmmcRequest(sdmmc_req_t* req) { return ZX_ERR_NOT_SUPPORTED; }

  zx_status_t SdmmcRequest(const sdmmc_req_t* req, uint32_t out_response[4]) {
    cpp20::span<const sdmmc_buffer_region_t> buffers(req->buffers_list, req->buffers_count);
    size_t bytes = 0;
    for (const sdmmc_buffer_region_t& buffer : buffers) {
      bytes += buffer.size;
    }
    blocks_remaining_ = req->blocksize ? static_cast<uint16_t>(bytes / req->blocksize) : 0;
    current_block_ = 0;
    return Sdhci::SdmmcRequest(req, out_response);
  }

  void PrepareStop(fdf::PrepareStopCompleter completer) override {
    run_thread_ = false;
    Sdhci::PrepareStop(std::move(completer));
  }

  uint8_t reset_mask() {
    uint8_t ret = reset_mask_;
    reset_mask_ = 0;
    return ret;
  }

  void* iobuf_virt() const { return iobuf_->virt(); }

  void TriggerCardInterrupt() { card_interrupt_ = true; }
  void InjectTransferError() { inject_error_ = true; }

 protected:
  zx_status_t WaitForReset(const SoftwareReset mask) override {
    reset_mask_ = mask.reg_value();
    return ZX_OK;
  }

  zx_status_t WaitForInterrupt() override {
    auto status = InterruptStatus::Get().FromValue(0).WriteTo(&*regs_mmio_buffer_);

    while (run_thread_) {
      switch (GetRequestStatus()) {
        case RequestStatus::COMMAND:
          status.set_command_complete(1).WriteTo(&*regs_mmio_buffer_);
          return ZX_OK;
        case RequestStatus::TRANSFER_DATA_DMA:
          status.set_transfer_complete(1);
          if (inject_error_) {
            status.set_error(1).set_data_crc_error(1);
          }
          status.WriteTo(&*regs_mmio_buffer_);
          return ZX_OK;
        case RequestStatus::READ_DATA_PIO:
          if (++current_block_ == blocks_remaining_) {
            status.set_buffer_read_ready(1).set_transfer_complete(1).WriteTo(&*regs_mmio_buffer_);
          } else {
            status.set_buffer_read_ready(1).WriteTo(&*regs_mmio_buffer_);
          }
          return ZX_OK;
        case RequestStatus::WRITE_DATA_PIO:
          if (++current_block_ == blocks_remaining_) {
            status.set_buffer_write_ready(1).set_transfer_complete(1).WriteTo(&*regs_mmio_buffer_);
          } else {
            status.set_buffer_write_ready(1).WriteTo(&*regs_mmio_buffer_);
          }
          return ZX_OK;
        case RequestStatus::BUSY_RESPONSE:
          status.set_transfer_complete(1).WriteTo(&*regs_mmio_buffer_);
          return ZX_OK;
        default:
          break;
      }

      if (card_interrupt_.exchange(false) &&
          InterruptStatusEnable::Get().ReadFrom(&*regs_mmio_buffer_).card_interrupt() == 1) {
        status.set_card_interrupt(1).WriteTo(&*regs_mmio_buffer_);
        return ZX_OK;
      }
    }

    return ZX_ERR_CANCELED;
  }

  zx_status_t InitMmio() override {
    regs_mmio_buffer_ = mmio_->View(0);
    HostControllerVersion::Get()
        .FromValue(0)
        .set_specification_version(HostControllerVersion::kSpecificationVersion300)
        .WriteTo(&*regs_mmio_buffer_);
    ClockControl::Get().FromValue(0).set_internal_clock_stable(1).WriteTo(&*regs_mmio_buffer_);
    return ZX_OK;
  }

 private:
  uint8_t reset_mask_ = 0;
  std::atomic<bool> run_thread_ = true;
  std::atomic<uint16_t> blocks_remaining_ = 0;
  std::atomic<uint16_t> current_block_ = 0;
  std::atomic<bool> card_interrupt_ = false;
  std::atomic<bool> inject_error_ = false;
};

fdf::MmioBuffer* TestSdhci::mmio_;

class FakeSdhci : public fdf::WireServer<fuchsia_hardware_sdhci::Device> {
 public:
  // fuchsia.hardware.sdhci/Device protocol implementation
  void GetInterrupt(fdf::Arena& arena, GetInterruptCompleter::Sync& completer) override {
    zx::interrupt interrupt;
    zx_status_t status =
        zx::interrupt::create(zx::resource(ZX_HANDLE_INVALID), 0, ZX_INTERRUPT_VIRTUAL, &interrupt);
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }

    completer.buffer(arena).ReplySuccess(std::move(interrupt));
  }

  void GetMmio(fdf::Arena& arena, GetMmioCompleter::Sync& completer) override {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetBti(GetBtiRequestView request, fdf::Arena& arena,
              GetBtiCompleter::Sync& completer) override {
    if (request->index != 0) {
      completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
      return;
    }
    zx::result bti = fake_bti::CreateFakeBtiWithPaddrs(dma_paddrs_);
    if (bti.is_error()) {
      completer.buffer(arena).ReplyError(bti.status_value());
      return;
    }
    unowned_bti_ = bti.value().borrow();

    completer.buffer(arena).ReplySuccess(std::move(bti.value()));
  }

  void GetBaseClock(fdf::Arena& arena, GetBaseClockCompleter::Sync& completer) override {
    completer.buffer(arena).Reply(base_clock_);
  }

  void GetQuirks(fdf::Arena& arena, GetQuirksCompleter::Sync& completer) override {
    completer.buffer(arena).Reply(quirks_, dma_boundary_alignment_);
  }

  void HwReset(fdf::Arena& arena, HwResetCompleter::Sync& completer) override {
    hw_reset_invoked_ = true;
    completer.buffer(arena).Reply();
  }

  void VendorSetBusClock(VendorSetBusClockRequestView request, fdf::Arena& arena,
                         VendorSetBusClockCompleter::Sync& completer) override {
    if (!supports_set_bus_clock_) {
      completer.buffer(arena).ReplyError(ZX_ERR_STOP);
      return;
    }
    completer.buffer(arena).ReplySuccess();
  }

  fuchsia_hardware_sdhci::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_sdhci::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                               fidl::kIgnoreBindingClosure),
    });
  }

  void set_dma_paddrs(std::vector<zx_paddr_t> dma_paddrs) { dma_paddrs_ = std::move(dma_paddrs); }
  zx::unowned_bti& unowned_bti() { return unowned_bti_; }
  void set_base_clock(uint32_t base_clock) { base_clock_ = base_clock; }
  void set_quirks(fuchsia_hardware_sdhci::Quirk quirks) { quirks_ = quirks; }
  void set_dma_boundary_alignment(uint64_t dma_boundary_alignment) {
    dma_boundary_alignment_ = dma_boundary_alignment;
  }
  bool hw_reset_invoked() const { return hw_reset_invoked_; }
  void set_supports_set_bus_clock() { supports_set_bus_clock_ = true; }

 private:
  std::vector<zx_paddr_t> dma_paddrs_;
  zx::unowned_bti unowned_bti_;
  uint32_t base_clock_ = 100'000'000;
  fuchsia_hardware_sdhci::Quirk quirks_;
  uint64_t dma_boundary_alignment_ = 0;
  bool hw_reset_invoked_ = false;
  bool supports_set_bus_clock_ = false;

  fdf::ServerBindingGroup<fuchsia_hardware_sdhci::Device> binding_group_;
};

class Environment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    zx::result result =
        to_driver_vfs.AddService<fuchsia_hardware_sdhci::Service>(sdhci_.GetInstanceHandler());
    if (result.is_error()) {
      return result.take_error();
    }

    return zx::ok();
  }

  fdf::MmioBuffer& mmio() { return mmio_; }
  FakeSdhci& sdhci() { return sdhci_; }

 private:
  fdf::MmioBuffer mmio_ = fdf_testing::CreateMmioBuffer(kRegisterSetSize);
  FakeSdhci sdhci_;
};

class TestConfig final {
 public:
  using DriverType = TestSdhci;
  using EnvironmentType = Environment;
};

class SdhciTest : public ::testing::Test {
 protected:
  zx::result<> StartDriver(std::vector<zx_paddr_t> dma_paddrs,
                           fuchsia_hardware_sdhci::Quirk quirks = {},
                           uint64_t dma_boundary_alignment = 0) {
    driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
      TestSdhci::mmio_ = &env.mmio();
      env.sdhci().set_dma_paddrs(std::move(dma_paddrs));
      env.sdhci().set_quirks(quirks);
      env.sdhci().set_dma_boundary_alignment(dma_boundary_alignment);
    });

    return driver_test().StartDriver();
  }

  zx::result<> StartDriver(fuchsia_hardware_sdhci::Quirk quirks = {},
                           uint64_t dma_boundary_alignment = 0) {
    return StartDriver({}, quirks, dma_boundary_alignment);
  }

  zx::result<> StopDriver() { return driver_test().StopDriver(); }

  fdf_testing::ForegroundDriverTest<TestConfig>& driver_test() { return driver_test_; }

  void ExpectPmoCount(uint64_t count) {
    zx_info_bti_t bti_info;
    driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
      EXPECT_OK(env.sdhci().unowned_bti()->get_info(ZX_INFO_BTI, &bti_info, sizeof(bti_info),
                                                    nullptr, nullptr));
    });
    EXPECT_EQ(bti_info.pmo_count, count);
  }

  fdf_testing::ForegroundDriverTest<TestConfig> driver_test_;
};

TEST_F(SdhciTest, DriverLifecycle) {
  ASSERT_OK(StartDriver());
  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, BaseClockZero) {
  driver_test().RunInEnvironmentTypeContext(
      [&](Environment& env) { env.sdhci().set_base_clock(0); });

  zx::result<> result = StartDriver();
  EXPECT_TRUE(result.is_error());
}

TEST_F(SdhciTest, BaseClockFromDriver) {
  driver_test().RunInEnvironmentTypeContext(
      [&](Environment& env) { env.sdhci().set_base_clock(0xabcdef); });

  ASSERT_OK(StartDriver());

  EXPECT_EQ(driver_test().driver()->base_clock(), 0xabcdefu);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, BaseClockFromHardware) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get().FromValue(0).set_base_clock_frequency(104).WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  EXPECT_EQ(driver_test().driver()->base_clock(), 104'000'000u);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, HostInfo) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities1::Get()
        .FromValue(0)
        .set_sdr50_support(1)
        .set_sdr104_support(1)
        .set_use_tuning_for_sdr50(1)
        .WriteTo(&env.mmio());
    Capabilities0::Get()
        .FromValue(0)
        .set_base_clock_frequency(1)
        .set_bus_width_8_support(1)
        .set_voltage_3v3_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  sdmmc_host_info_t host_info = {};
  EXPECT_OK(driver_test().driver()->SdmmcHostInfo(&host_info));
  EXPECT_EQ(host_info.caps, SDMMC_HOST_CAP_BUS_WIDTH_8 | SDMMC_HOST_CAP_VOLTAGE_330 |
                                SDMMC_HOST_CAP_AUTO_CMD12 | SDMMC_HOST_CAP_SDR50 |
                                SDMMC_HOST_CAP_SDR104);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, HostInfoNoDma) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities1::Get().FromValue(0).set_sdr50_support(1).set_ddr50_support(1).WriteTo(
        &env.mmio());
    Capabilities0::Get()
        .FromValue(0)
        .set_base_clock_frequency(1)
        .set_bus_width_8_support(1)
        .set_voltage_3v3_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver(fuchsia_hardware_sdhci::Quirk::kNoDma));

  sdmmc_host_info_t host_info = {};
  EXPECT_OK(driver_test().driver()->SdmmcHostInfo(&host_info));
  EXPECT_EQ(host_info.caps, SDMMC_HOST_CAP_BUS_WIDTH_8 | SDMMC_HOST_CAP_VOLTAGE_330 |
                                SDMMC_HOST_CAP_AUTO_CMD12 | SDMMC_HOST_CAP_DDR50 |
                                SDMMC_HOST_CAP_SDR50 | SDMMC_HOST_CAP_NO_TUNING_SDR50);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, HostInfoNoTuning) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities1::Get().FromValue(0).WriteTo(&env.mmio());
    Capabilities0::Get().FromValue(0).set_base_clock_frequency(1).WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver(fuchsia_hardware_sdhci::Quirk::kNonStandardTuning));

  sdmmc_host_info_t host_info = {};
  EXPECT_OK(driver_test().driver()->SdmmcHostInfo(&host_info));
  EXPECT_EQ(host_info.caps, SDMMC_HOST_CAP_AUTO_CMD12 | SDMMC_HOST_CAP_NO_TUNING_SDR50);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, SetSignalVoltage) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get().FromValue(0).set_voltage_3v3_support(1).set_voltage_1v8_support(1).WriteTo(
        &env.mmio());
  });

  ASSERT_OK(StartDriver());

  PresentState::Get().FromValue(0).set_dat_3_0(0b0001).WriteTo(driver_test().driver()->mmio_);

  PowerControl::Get()
      .FromValue(0)
      .set_sd_bus_voltage_vdd1(PowerControl::kBusVoltage1V8)
      .set_sd_bus_power_vdd1(1)
      .WriteTo(driver_test().driver()->mmio_);
  EXPECT_OK(driver_test().driver()->SdmmcSetSignalVoltage(SDMMC_VOLTAGE_V180));
  EXPECT_TRUE(
      HostControl2::Get().ReadFrom(driver_test().driver()->mmio_).voltage_1v8_signalling_enable());

  PowerControl::Get()
      .FromValue(0)
      .set_sd_bus_voltage_vdd1(PowerControl::kBusVoltage3V3)
      .set_sd_bus_power_vdd1(1)
      .WriteTo(driver_test().driver()->mmio_);
  EXPECT_OK(driver_test().driver()->SdmmcSetSignalVoltage(SDMMC_VOLTAGE_V330));
  EXPECT_FALSE(
      HostControl2::Get().ReadFrom(driver_test().driver()->mmio_).voltage_1v8_signalling_enable());

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, SetSignalVoltageUnsupported) {
  ASSERT_OK(StartDriver());

  EXPECT_NE(ZX_OK, driver_test().driver()->SdmmcSetSignalVoltage(SDMMC_VOLTAGE_V330));

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, SetBusWidth) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get().FromValue(0).set_bus_width_8_support(1).WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  auto ctrl1 = HostControl1::Get().FromValue(0);

  EXPECT_OK(driver_test().driver()->SdmmcSetBusWidth(SDMMC_BUS_WIDTH_EIGHT));
  EXPECT_TRUE(ctrl1.ReadFrom(driver_test().driver()->mmio_).extended_data_transfer_width());
  EXPECT_FALSE(ctrl1.ReadFrom(driver_test().driver()->mmio_).data_transfer_width_4bit());

  EXPECT_OK(driver_test().driver()->SdmmcSetBusWidth(SDMMC_BUS_WIDTH_ONE));
  EXPECT_FALSE(ctrl1.ReadFrom(driver_test().driver()->mmio_).extended_data_transfer_width());
  EXPECT_FALSE(ctrl1.ReadFrom(driver_test().driver()->mmio_).data_transfer_width_4bit());

  EXPECT_OK(driver_test().driver()->SdmmcSetBusWidth(SDMMC_BUS_WIDTH_FOUR));
  EXPECT_FALSE(ctrl1.ReadFrom(driver_test().driver()->mmio_).extended_data_transfer_width());
  EXPECT_TRUE(ctrl1.ReadFrom(driver_test().driver()->mmio_).data_transfer_width_4bit());

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, SetBusWidthNotSupported) {
  ASSERT_OK(StartDriver());

  EXPECT_NE(ZX_OK, driver_test().driver()->SdmmcSetBusWidth(SDMMC_BUS_WIDTH_EIGHT));

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, SetBusFreq) {
  ASSERT_OK(StartDriver());

  auto clock = ClockControl::Get().FromValue(0);

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(0));
  EXPECT_TRUE(clock.ReadFrom(driver_test().driver()->mmio_).internal_clock_enable());

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(12'500'000));
  EXPECT_EQ(clock.ReadFrom(driver_test().driver()->mmio_).frequency_select(), 4);
  EXPECT_TRUE(clock.sd_clock_enable());
  EXPECT_TRUE(clock.internal_clock_enable());

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(65'190));
  EXPECT_EQ(clock.ReadFrom(driver_test().driver()->mmio_).frequency_select(), 767);
  EXPECT_TRUE(clock.sd_clock_enable());
  EXPECT_TRUE(clock.internal_clock_enable());

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(100'000'000));
  EXPECT_EQ(clock.ReadFrom(driver_test().driver()->mmio_).frequency_select(), 0);
  EXPECT_TRUE(clock.sd_clock_enable());
  EXPECT_TRUE(clock.internal_clock_enable());

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(26'000'000));
  EXPECT_EQ(clock.ReadFrom(driver_test().driver()->mmio_).frequency_select(), 2);
  EXPECT_TRUE(clock.sd_clock_enable());
  EXPECT_TRUE(clock.internal_clock_enable());

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(0));
  EXPECT_FALSE(clock.ReadFrom(driver_test().driver()->mmio_).sd_clock_enable());
  EXPECT_TRUE(clock.internal_clock_enable());

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, SetBusFreqVendorSpecific) {
  driver_test().RunInEnvironmentTypeContext(
      [&](Environment& env) { env.sdhci().set_supports_set_bus_clock(); });

  ASSERT_OK(StartDriver());

  auto clock_control = [&]() { return ClockControl::Get().ReadFrom(TestSdhci::mmio_).reg_value(); };

  const uint32_t initial_value = clock_control();

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(0));
  EXPECT_EQ(clock_control(), initial_value);

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(12'500'000));
  EXPECT_EQ(clock_control(), initial_value);

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(65'190));
  EXPECT_EQ(clock_control(), initial_value);

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(100'000'000));
  EXPECT_EQ(clock_control(), initial_value);

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(26'000'000));
  EXPECT_EQ(clock_control(), initial_value);

  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(0));
  EXPECT_EQ(clock_control(), initial_value);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, SetBusFreqTimeout) {
  ASSERT_OK(StartDriver());

  ClockControl::Get().FromValue(0).set_internal_clock_stable(1).WriteTo(
      driver_test().driver()->mmio_);
  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(12'500'000));

  ClockControl::Get().FromValue(0).WriteTo(driver_test().driver()->mmio_);
  EXPECT_NE(ZX_OK, driver_test().driver()->SdmmcSetBusFreq(12'500'000));

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, SetBusFreqInternalClockEnable) {
  ASSERT_OK(StartDriver());

  ClockControl::Get()
      .FromValue(0)
      .set_internal_clock_stable(1)
      .set_internal_clock_enable(0)
      .WriteTo(driver_test().driver()->mmio_);
  EXPECT_OK(driver_test().driver()->SdmmcSetBusFreq(12'500'000));
  EXPECT_TRUE(ClockControl::Get().ReadFrom(driver_test().driver()->mmio_).internal_clock_enable());

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, SetTiming) {
  ASSERT_OK(StartDriver());

  EXPECT_OK(driver_test().driver()->SdmmcSetTiming(SDMMC_TIMING_HS));
  EXPECT_TRUE(HostControl1::Get().ReadFrom(driver_test().driver()->mmio_).high_speed_enable());
  EXPECT_EQ(HostControl2::Get().ReadFrom(driver_test().driver()->mmio_).uhs_mode_select(),
            HostControl2::kUhsModeSdr25);

  EXPECT_OK(driver_test().driver()->SdmmcSetTiming(SDMMC_TIMING_LEGACY));
  EXPECT_FALSE(HostControl1::Get().ReadFrom(driver_test().driver()->mmio_).high_speed_enable());
  EXPECT_EQ(HostControl2::Get().ReadFrom(driver_test().driver()->mmio_).uhs_mode_select(),
            HostControl2::kUhsModeSdr12);

  EXPECT_OK(driver_test().driver()->SdmmcSetTiming(SDMMC_TIMING_HSDDR));
  EXPECT_TRUE(HostControl1::Get().ReadFrom(driver_test().driver()->mmio_).high_speed_enable());
  EXPECT_EQ(HostControl2::Get().ReadFrom(driver_test().driver()->mmio_).uhs_mode_select(),
            HostControl2::kUhsModeDdr50);

  EXPECT_OK(driver_test().driver()->SdmmcSetTiming(SDMMC_TIMING_SDR25));
  EXPECT_TRUE(HostControl1::Get().ReadFrom(driver_test().driver()->mmio_).high_speed_enable());
  EXPECT_EQ(HostControl2::Get().ReadFrom(driver_test().driver()->mmio_).uhs_mode_select(),
            HostControl2::kUhsModeSdr25);

  EXPECT_OK(driver_test().driver()->SdmmcSetTiming(SDMMC_TIMING_SDR12));
  EXPECT_TRUE(HostControl1::Get().ReadFrom(driver_test().driver()->mmio_).high_speed_enable());
  EXPECT_EQ(HostControl2::Get().ReadFrom(driver_test().driver()->mmio_).uhs_mode_select(),
            HostControl2::kUhsModeSdr12);

  EXPECT_OK(driver_test().driver()->SdmmcSetTiming(SDMMC_TIMING_HS400));
  EXPECT_TRUE(HostControl1::Get().ReadFrom(driver_test().driver()->mmio_).high_speed_enable());
  EXPECT_EQ(HostControl2::Get().ReadFrom(driver_test().driver()->mmio_).uhs_mode_select(),
            HostControl2::kUhsModeHs400);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, HwReset) {
  ASSERT_OK(StartDriver());

  driver_test().driver()->SdmmcHwReset();
  driver_test().RunInEnvironmentTypeContext(
      [&](Environment& env) { EXPECT_TRUE(env.sdhci().hw_reset_invoked()); });

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, RequestCommandOnly) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get().FromValue(0).set_adma2_support(1).WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_SEND_STATUS,
      .cmd_flags = SDMMC_SEND_STATUS_FLAGS,
      .arg = 0x7b7d9fbd,
      .buffers_count = 0,
  };

  Response::Get(0).FromValue(0xf3bbf2c0).WriteTo(driver_test().driver()->mmio_);
  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  auto command = Command::Get().FromValue(0);

  EXPECT_EQ(Argument::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), 0x7b7d9fbdu);
  EXPECT_EQ(command.ReadFrom(driver_test().driver()->mmio_).command_index(), SDMMC_SEND_STATUS);
  EXPECT_EQ(command.command_type(), Command::kCommandTypeNormal);
  EXPECT_FALSE(command.data_present());
  EXPECT_TRUE(command.command_index_check());
  EXPECT_TRUE(command.command_crc_check());
  EXPECT_EQ(command.response_type(), Command::kResponseType48Bits);

  EXPECT_EQ(response[0], 0xf3bbf2c0u);

  request = {
      .cmd_idx = SDMMC_SEND_CSD,
      .cmd_flags = SDMMC_SEND_CSD_FLAGS,
      .arg = 0x9c1dc1ed,
      .buffers_count = 0,
  };

  Response::Get(0).FromValue(0x9f93b17d).WriteTo(driver_test().driver()->mmio_);
  Response::Get(1).FromValue(0x89aaba9e).WriteTo(driver_test().driver()->mmio_);
  Response::Get(2).FromValue(0xc14b059e).WriteTo(driver_test().driver()->mmio_);
  Response::Get(3).FromValue(0x7329a9e3).WriteTo(driver_test().driver()->mmio_);
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(Argument::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), 0x9c1dc1edu);
  EXPECT_EQ(command.ReadFrom(driver_test().driver()->mmio_).command_index(), SDMMC_SEND_CSD);
  EXPECT_EQ(command.command_type(), Command::kCommandTypeNormal);
  EXPECT_FALSE(command.data_present());
  EXPECT_TRUE(command.command_crc_check());
  EXPECT_EQ(command.response_type(), Command::kResponseType136Bits);

  EXPECT_EQ(response[0], 0x9f93b17du);
  EXPECT_EQ(response[1], 0x89aaba9eu);
  EXPECT_EQ(response[2], 0xc14b059eu);
  EXPECT_EQ(response[3], 0x7329a9e3u);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, RequestAbort) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get().FromValue(0).set_adma2_support(1).WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(1024, 0, &vmo));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 0,
      .size = 1024,
  };

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0,
      .blocksize = 4,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };

  driver_test().driver()->reset_mask();

  uint32_t unused_response[4];
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, unused_response));
  EXPECT_EQ(driver_test().driver()->reset_mask(), 0);

  request.cmd_idx = SDMMC_STOP_TRANSMISSION;
  request.cmd_flags = SDMMC_STOP_TRANSMISSION_FLAGS;
  request.blocksize = 0;
  request.buffers_list = nullptr;
  request.buffers_count = 0;
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, unused_response));
  EXPECT_EQ(driver_test().driver()->reset_mask(),
            SoftwareReset::Get().FromValue(0).set_reset_dat(1).set_reset_cmd(1).reg_value());

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, SdioInBandInterrupt) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get().FromValue(0).set_adma2_support(1).WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  in_band_interrupt_protocol_ops_t callback_ops = {
      .callback = [](void* ctx) -> void {
        sync_completion_signal(reinterpret_cast<sync_completion_t*>(ctx));
      },
  };

  sync_completion_t callback_called;
  in_band_interrupt_protocol_t callback = {
      .ops = &callback_ops,
      .ctx = &callback_called,
  };

  EXPECT_OK(driver_test().driver()->SdmmcRegisterInBandInterrupt(&callback));

  driver_test().driver()->TriggerCardInterrupt();
  sync_completion_wait(&callback_called, ZX_TIME_INFINITE);
  sync_completion_reset(&callback_called);

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_SEND_CSD,
      .cmd_flags = SDMMC_SEND_CSD_FLAGS,
      .arg = 0x9c1dc1ed,
      .buffers_count = 0,
  };
  uint32_t unused_response[4];
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, unused_response));

  driver_test().driver()->SdmmcAckInBandInterrupt();

  // Verify that the card interrupt remains enabled after other interrupts have been disabled, such
  // as after a commend.
  driver_test().driver()->TriggerCardInterrupt();
  sync_completion_wait(&callback_called, ZX_TIME_INFINITE);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, DmaRequest64Bit) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  for (int i = 0; i < 4; i++) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(512 * 16, 0, &vmo));
    EXPECT_OK(driver_test().driver()->SdmmcRegisterVmo(i, 3, std::move(vmo), 64 * i, 512 * 12,
                                                       SDMMC_VMO_RIGHT_READ));
  }

  const sdmmc_buffer_region_t buffers[4] = {
      {
          .buffer =
              {
                  .vmo_id = 1,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 16,
          .size = 512,
      },
      {
          .buffer =
              {
                  .vmo_id = 0,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 32,
          .size = 512 * 3,
      },
      {
          .buffer =
              {
                  .vmo_id = 3,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 48,
          .size = 512 * 10,
      },
      {
          .buffer =
              {
                  .vmo_id = 2,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 80,
          .size = 512 * 7,
      },
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 3,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(AdmaSystemAddress::Get(0).ReadFrom(driver_test().driver()->mmio_).reg_value(),
            zx_system_get_page_size());
  EXPECT_EQ(AdmaSystemAddress::Get(1).ReadFrom(driver_test().driver()->mmio_).reg_value(), 0u);

  const auto* const descriptors =
      reinterpret_cast<Sdhci::AdmaDescriptor96*>(driver_test().driver()->iobuf_virt());

  uint64_t address;
  memcpy(&address, &descriptors[0].address, sizeof(address));
  EXPECT_EQ(descriptors[0].attr, 0b100'001u);
  EXPECT_EQ(address, zx_system_get_page_size() + 80);
  EXPECT_EQ(descriptors[0].length, 512u);

  memcpy(&address, &descriptors[1].address, sizeof(address));
  EXPECT_EQ(descriptors[1].attr, 0b100'001u);
  EXPECT_EQ(address, zx_system_get_page_size() + 32);
  EXPECT_EQ(descriptors[1].length, 512 * 3);

  // Buffer is greater than one page and gets split across two descriptors.
  memcpy(&address, &descriptors[2].address, sizeof(address));
  EXPECT_EQ(descriptors[2].attr, 0b100'001u);
  EXPECT_EQ(address, zx_system_get_page_size() + 240);
  EXPECT_EQ(descriptors[2].length, zx_system_get_page_size() - 240);

  memcpy(&address, &descriptors[3].address, sizeof(address));
  EXPECT_EQ(descriptors[3].attr, 0b100'001u);
  EXPECT_EQ(address, zx_system_get_page_size());
  EXPECT_EQ(descriptors[3].length, (512 * 10) - zx_system_get_page_size() + 240);

  memcpy(&address, &descriptors[4].address, sizeof(address));
  EXPECT_EQ(descriptors[4].attr, 0b100'011);
  EXPECT_EQ(address, zx_system_get_page_size() + 208);
  EXPECT_EQ(descriptors[4].length, 512 * 7);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, DmaRequest32Bit) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(0)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  for (int i = 0; i < 4; i++) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(512 * 16, 0, &vmo));
    EXPECT_OK(driver_test().driver()->SdmmcRegisterVmo(i, 3, std::move(vmo), 64 * i, 512 * 12,
                                                       SDMMC_VMO_RIGHT_WRITE));
  }

  const sdmmc_buffer_region_t buffers[4] = {
      {
          .buffer =
              {
                  .vmo_id = 1,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 16,
          .size = 512,
      },
      {
          .buffer =
              {
                  .vmo_id = 0,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 32,
          .size = 512 * 3,
      },
      {
          .buffer =
              {
                  .vmo_id = 3,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 48,
          .size = 512 * 10,
      },
      {
          .buffer =
              {
                  .vmo_id = 2,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 80,
          .size = 512 * 7,
      },
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 3,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(AdmaSystemAddress::Get(0).ReadFrom(driver_test().driver()->mmio_).reg_value(),
            zx_system_get_page_size());
  EXPECT_EQ(AdmaSystemAddress::Get(1).ReadFrom(driver_test().driver()->mmio_).reg_value(), 0u);

  const auto* const descriptors =
      reinterpret_cast<Sdhci::AdmaDescriptor64*>(driver_test().driver()->iobuf_virt());

  EXPECT_EQ(descriptors[0].attr, 0b100'001u);
  EXPECT_EQ(descriptors[0].address, zx_system_get_page_size() + 80);
  EXPECT_EQ(descriptors[0].length, 512u);

  EXPECT_EQ(descriptors[1].attr, 0b100'001u);
  EXPECT_EQ(descriptors[1].address, zx_system_get_page_size() + 32);
  EXPECT_EQ(descriptors[1].length, 512 * 3);

  // Buffer is greater than one page and gets split across two descriptors.
  EXPECT_EQ(descriptors[2].attr, 0b100'001u);
  EXPECT_EQ(descriptors[2].address, zx_system_get_page_size() + 240);
  EXPECT_EQ(descriptors[2].length, zx_system_get_page_size() - 240);

  EXPECT_EQ(descriptors[3].attr, 0b100'001u);
  EXPECT_EQ(descriptors[3].address, zx_system_get_page_size());
  EXPECT_EQ(descriptors[3].length, (512 * 10) - zx_system_get_page_size() + 240);

  EXPECT_EQ(descriptors[4].attr, 0b100'011);
  EXPECT_EQ(descriptors[4].address, zx_system_get_page_size() + 208);
  EXPECT_EQ(descriptors[4].length, 512 * 7);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, DmaSplitOneBoundary) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(0)
        .WriteTo(&env.mmio());
  });

  constexpr zx_paddr_t kDescriptorAddress = 0xc000'0000;
  const zx_paddr_t kStartAddress = 0xa7ff'ffff & ~PageMask();

  ASSERT_OK(StartDriver(
      {
          kDescriptorAddress,
          kStartAddress,
          kStartAddress + zx_system_get_page_size(),
          kStartAddress + (zx_system_get_page_size() * 2),
          0xb000'0000,
      },
      fuchsia_hardware_sdhci::Quirk::kUseDmaBoundaryAlignment, 0x0800'0000));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size() * 4, 0, &vmo));
  ASSERT_OK(driver_test().driver()->SdmmcRegisterVmo(
      0, 0, std::move(vmo), 0, zx_system_get_page_size() * 4, SDMMC_VMO_RIGHT_WRITE));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo_id = 0,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      // The first buffer should be split across the 128M boundary.
      .offset = zx_system_get_page_size() - 4,
      // Two pages plus 256 bytes.
      .size = (zx_system_get_page_size() * 2) + 256,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 16,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(AdmaSystemAddress::Get(0).ReadFrom(driver_test().driver()->mmio_).reg_value(),
            kDescriptorAddress);
  EXPECT_EQ(AdmaSystemAddress::Get(1).ReadFrom(driver_test().driver()->mmio_).reg_value(), 0u);

  const Sdhci::AdmaDescriptor64* const descriptors =
      reinterpret_cast<Sdhci::AdmaDescriptor64*>(driver_test().driver()->iobuf_virt());

  EXPECT_EQ(descriptors[0].attr, 0b100'001u);
  EXPECT_EQ(descriptors[0].address, 0xa7ff'fffc);
  EXPECT_EQ(descriptors[0].length, 4);

  EXPECT_EQ(descriptors[1].attr, 0b100'001u);
  EXPECT_EQ(descriptors[1].address, 0xa800'0000);
  EXPECT_EQ(descriptors[1].length, zx_system_get_page_size() * 2);

  EXPECT_EQ(descriptors[2].attr, 0b100'011);
  EXPECT_EQ(descriptors[2].address, 0xb000'0000);
  EXPECT_EQ(descriptors[2].length, 256 - 4);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, DmaSplitManyBoundaries) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(0)
        .WriteTo(&env.mmio());
  });

  constexpr zx_paddr_t kDescriptorAddress = 0xc000'0000;
  ASSERT_OK(StartDriver(
      {
          kDescriptorAddress,
          0xabcd'0000,
      },
      fuchsia_hardware_sdhci::Quirk::kUseDmaBoundaryAlignment, 0x100));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  ASSERT_OK(driver_test().driver()->SdmmcRegisterVmo(
      0, 0, std::move(vmo), 0, zx_system_get_page_size(), SDMMC_VMO_RIGHT_WRITE));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo_id = 0,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = 128,
      .size = 16 * 64,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 16,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(AdmaSystemAddress::Get(0).ReadFrom(driver_test().driver()->mmio_).reg_value(),
            kDescriptorAddress);
  EXPECT_EQ(AdmaSystemAddress::Get(1).ReadFrom(driver_test().driver()->mmio_).reg_value(), 0u);

  const Sdhci::AdmaDescriptor64* const descriptors =
      reinterpret_cast<Sdhci::AdmaDescriptor64*>(driver_test().driver()->iobuf_virt());

  EXPECT_EQ(descriptors[0].attr, 0b100'001u);
  EXPECT_EQ(descriptors[0].address, 0xabcd'0080);
  EXPECT_EQ(descriptors[0].length, 128);

  EXPECT_EQ(descriptors[1].attr, 0b100'001u);
  EXPECT_EQ(descriptors[1].address, 0xabcd'0100);
  EXPECT_EQ(descriptors[1].length, 256);

  EXPECT_EQ(descriptors[2].attr, 0b100'001u);
  EXPECT_EQ(descriptors[2].address, 0xabcd'0200);
  EXPECT_EQ(descriptors[2].length, 256);

  EXPECT_EQ(descriptors[3].attr, 0b100'001u);
  EXPECT_EQ(descriptors[3].address, 0xabcd'0300);
  EXPECT_EQ(descriptors[3].length, 256);

  EXPECT_EQ(descriptors[4].attr, 0b100'011);
  EXPECT_EQ(descriptors[4].address, 0xabcd'0400);
  EXPECT_EQ(descriptors[4].length, 128);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, DmaNoBoundaries) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(0)
        .WriteTo(&env.mmio());
  });

  constexpr zx_paddr_t kDescriptorAddress = 0xc000'0000;
  const zx_paddr_t kStartAddress = 0xa7ff'ffff & ~PageMask();

  ASSERT_OK(StartDriver({
      kDescriptorAddress,
      kStartAddress,
      kStartAddress + zx_system_get_page_size(),
      kStartAddress + (zx_system_get_page_size() * 2),
      0xb000'0000,
  }));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size() * 4, 0, &vmo));
  ASSERT_OK(driver_test().driver()->SdmmcRegisterVmo(
      0, 0, std::move(vmo), 0, zx_system_get_page_size() * 4, SDMMC_VMO_RIGHT_WRITE));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo_id = 0,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = zx_system_get_page_size() - 4,
      .size = (zx_system_get_page_size() * 2) + 256,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 16,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(AdmaSystemAddress::Get(0).ReadFrom(driver_test().driver()->mmio_).reg_value(),
            kDescriptorAddress);
  EXPECT_EQ(AdmaSystemAddress::Get(1).ReadFrom(driver_test().driver()->mmio_).reg_value(), 0u);

  const Sdhci::AdmaDescriptor64* const descriptors =
      reinterpret_cast<Sdhci::AdmaDescriptor64*>(driver_test().driver()->iobuf_virt());

  EXPECT_EQ(descriptors[0].attr, 0b100'001u);
  EXPECT_EQ(descriptors[0].address, 0xa7ff'fffc);
  EXPECT_EQ(descriptors[0].length, (zx_system_get_page_size() * 2) + 4);

  EXPECT_EQ(descriptors[1].attr, 0b100'011);
  EXPECT_EQ(descriptors[1].address, 0xb000'0000);
  EXPECT_EQ(descriptors[1].length, 256 - 4);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, CommandSettingsMultiBlock) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver(fuchsia_hardware_sdhci::Quirk::kStripResponseCrcPreserveOrder));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  EXPECT_OK(driver_test().driver()->SdmmcRegisterVmo(
      0, 0, std::move(vmo), 0, zx_system_get_page_size(), SDMMC_VMO_RIGHT_READ));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo_id = 0,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = 0,
      .size = 1024,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234'abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };

  Response::Get(0).FromValue(0).set_reg_value(0xabcd'1234).WriteTo(driver_test().driver()->mmio_);
  Response::Get(1).FromValue(0).set_reg_value(0xa5a5'a5a5).WriteTo(driver_test().driver()->mmio_);
  Response::Get(2).FromValue(0).set_reg_value(0x1122'3344).WriteTo(driver_test().driver()->mmio_);
  Response::Get(3).FromValue(0).set_reg_value(0xaabb'ccdd).WriteTo(driver_test().driver()->mmio_);

  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(response[0], 0xabcd'1234u);
  EXPECT_EQ(response[1], 0u);
  EXPECT_EQ(response[2], 0u);
  EXPECT_EQ(response[3], 0u);

  const Command command = Command::Get().ReadFrom(driver_test().driver()->mmio_);
  EXPECT_EQ(command.response_type(), Command::kResponseType48Bits);
  EXPECT_TRUE(command.command_crc_check());
  EXPECT_TRUE(command.command_index_check());
  EXPECT_TRUE(command.data_present());
  EXPECT_EQ(command.command_type(), Command::kCommandTypeNormal);
  EXPECT_EQ(command.command_index(), SDMMC_WRITE_MULTIPLE_BLOCK);

  const TransferMode transfer_mode = TransferMode::Get().ReadFrom(driver_test().driver()->mmio_);
  EXPECT_TRUE(transfer_mode.dma_enable());
  EXPECT_TRUE(transfer_mode.block_count_enable());
  EXPECT_EQ(transfer_mode.auto_cmd_enable(), TransferMode::kAutoCmdDisable);
  EXPECT_FALSE(transfer_mode.read());
  EXPECT_TRUE(transfer_mode.multi_block());

  EXPECT_EQ(BlockSize::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), 512u);
  EXPECT_EQ(BlockCount::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), 2u);
  EXPECT_EQ(Argument::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), 0x1234'abcdu);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, CommandSettingsSingleBlock) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver(fuchsia_hardware_sdhci::Quirk::kStripResponseCrcPreserveOrder));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  EXPECT_OK(driver_test().driver()->SdmmcRegisterVmo(
      0, 0, std::move(vmo), 0, zx_system_get_page_size(), SDMMC_VMO_RIGHT_WRITE));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo_id = 0,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = 0,
      .size = 128,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_BLOCK,
      .cmd_flags = SDMMC_READ_BLOCK_FLAGS,
      .arg = 0x1234'abcd,
      .blocksize = 128,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };

  Response::Get(0).FromValue(0).set_reg_value(0xabcd'1234).WriteTo(driver_test().driver()->mmio_);
  Response::Get(1).FromValue(0).set_reg_value(0xa5a5'a5a5).WriteTo(driver_test().driver()->mmio_);
  Response::Get(2).FromValue(0).set_reg_value(0x1122'3344).WriteTo(driver_test().driver()->mmio_);
  Response::Get(3).FromValue(0).set_reg_value(0xaabb'ccdd).WriteTo(driver_test().driver()->mmio_);

  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(response[0], 0xabcd'1234u);
  EXPECT_EQ(response[1], 0u);
  EXPECT_EQ(response[2], 0u);
  EXPECT_EQ(response[3], 0u);

  const Command command = Command::Get().ReadFrom(driver_test().driver()->mmio_);
  EXPECT_EQ(command.response_type(), Command::kResponseType48Bits);
  EXPECT_TRUE(command.command_crc_check());
  EXPECT_TRUE(command.command_index_check());
  EXPECT_TRUE(command.data_present());
  EXPECT_EQ(command.command_type(), Command::kCommandTypeNormal);
  EXPECT_EQ(command.command_index(), SDMMC_READ_BLOCK);

  const TransferMode transfer_mode = TransferMode::Get().ReadFrom(driver_test().driver()->mmio_);
  EXPECT_TRUE(transfer_mode.dma_enable());
  EXPECT_EQ(transfer_mode.auto_cmd_enable(), TransferMode::kAutoCmdDisable);
  EXPECT_TRUE(transfer_mode.read());
  EXPECT_FALSE(transfer_mode.multi_block());
  // The controller ignores block count enable if multi block is cleared.

  EXPECT_EQ(BlockSize::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), 128u);
  EXPECT_EQ(BlockCount::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), 1u);
  EXPECT_EQ(Argument::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), 0x1234'abcdu);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, CommandSettingsBusyResponse) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver(fuchsia_hardware_sdhci::Quirk::kStripResponseCrcPreserveOrder));

  const sdmmc_req_t request = {
      .cmd_idx = 55,
      .cmd_flags = SDMMC_RESP_LEN_48B | SDMMC_CMD_TYPE_NORMAL | SDMMC_RESP_CRC_CHECK |
                   SDMMC_RESP_CMD_IDX_CHECK,
      .arg = 0x1234'abcd,
      .blocksize = 0,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = nullptr,
      .buffers_count = 0,
  };

  Response::Get(0).FromValue(0).set_reg_value(0xabcd'1234).WriteTo(driver_test().driver()->mmio_);
  Response::Get(1).FromValue(0).set_reg_value(0xa5a5'a5a5).WriteTo(driver_test().driver()->mmio_);
  Response::Get(2).FromValue(0).set_reg_value(0x1122'3344).WriteTo(driver_test().driver()->mmio_);
  Response::Get(3).FromValue(0).set_reg_value(0xaabb'ccdd).WriteTo(driver_test().driver()->mmio_);

  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(response[0], 0xabcd'1234u);
  EXPECT_EQ(response[1], 0u);
  EXPECT_EQ(response[2], 0u);
  EXPECT_EQ(response[3], 0u);

  const Command command = Command::Get().ReadFrom(driver_test().driver()->mmio_);
  EXPECT_EQ(command.response_type(), Command::kResponseType48BitsWithBusy);
  EXPECT_TRUE(command.command_crc_check());
  EXPECT_TRUE(command.command_index_check());
  EXPECT_FALSE(command.data_present());
  EXPECT_EQ(command.command_type(), Command::kCommandTypeNormal);
  EXPECT_EQ(command.command_index(), 55);

  const TransferMode transfer_mode = TransferMode::Get().ReadFrom(driver_test().driver()->mmio_);
  EXPECT_FALSE(transfer_mode.dma_enable());
  EXPECT_FALSE(transfer_mode.block_count_enable());
  EXPECT_EQ(transfer_mode.auto_cmd_enable(), TransferMode::kAutoCmdDisable);
  EXPECT_FALSE(transfer_mode.read());
  EXPECT_FALSE(transfer_mode.multi_block());

  EXPECT_EQ(BlockSize::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), 0u);
  EXPECT_EQ(BlockCount::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), 0u);
  EXPECT_EQ(Argument::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), 0x1234'abcdu);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, ZeroBlockSize) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  for (int i = 0; i < 4; i++) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(512 * 16, 0, &vmo));
    EXPECT_OK(driver_test().driver()->SdmmcRegisterVmo(i, 3, std::move(vmo), 64 * i, 512 * 12,
                                                       SDMMC_VMO_RIGHT_READ));
  }

  const sdmmc_buffer_region_t buffers[4] = {
      {
          .buffer =
              {
                  .vmo_id = 1,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 16,
          .size = 512,
      },
      {
          .buffer =
              {
                  .vmo_id = 0,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 32,
          .size = 512 * 3,
      },
      {
          .buffer =
              {
                  .vmo_id = 3,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 48,
          .size = 512 * 10,
      },
      {
          .buffer =
              {
                  .vmo_id = 2,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 80,
          .size = 512 * 7,
      },
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 0,
      .suppress_error_messages = false,
      .client_id = 3,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };
  uint32_t response[4] = {};
  EXPECT_NE(ZX_OK, driver_test().driver()->SdmmcRequest(&request, response));

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, NoBuffers) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(512 * 16, 0, &vmo));
  EXPECT_OK(driver_test().driver()->SdmmcRegisterVmo(1, 3, std::move(vmo), 0, 1024,
                                                     SDMMC_VMO_RIGHT_READ | SDMMC_VMO_RIGHT_WRITE));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo_id = 1,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = 0,
      .size = 512,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 0,
      .suppress_error_messages = false,
      .client_id = 3,
      .buffers_list = &buffer,
      .buffers_count = 0,
  };
  uint32_t response[4] = {};
  EXPECT_NE(ZX_OK, driver_test().driver()->SdmmcRequest(&request, response));

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, OwnedAndUnownedBuffers) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  zx::vmo vmos[4];
  for (int i = 0; i < 4; i++) {
    ASSERT_OK(zx::vmo::create(512 * 16, 0, &vmos[i]));
    if (i % 2 == 0) {
      EXPECT_OK(driver_test().driver()->SdmmcRegisterVmo(i, 3, std::move(vmos[i]), 64 * i, 512 * 12,
                                                         SDMMC_VMO_RIGHT_READ));
    }
  }

  const sdmmc_buffer_region_t buffers[4] = {
      {
          .buffer =
              {
                  .vmo = vmos[1].get(),
              },
          .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
          .offset = 16,
          .size = 512,
      },
      {
          .buffer =
              {
                  .vmo_id = 0,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 32,
          .size = 512 * 3,
      },
      {
          .buffer =
              {
                  .vmo = vmos[3].get(),
              },
          .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
          .offset = 48,
          .size = 512 * 10,
      },
      {
          .buffer =
              {
                  .vmo_id = 2,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 80,
          .size = 512 * 7,
      },
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 3,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };

  ExpectPmoCount(3);

  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  // Unowned buffers should have been unpinned.
  ExpectPmoCount(3);

  EXPECT_EQ(AdmaSystemAddress::Get(0).ReadFrom(driver_test().driver()->mmio_).reg_value(),
            zx_system_get_page_size());
  EXPECT_EQ(AdmaSystemAddress::Get(1).ReadFrom(driver_test().driver()->mmio_).reg_value(), 0u);

  const auto* const descriptors =
      reinterpret_cast<Sdhci::AdmaDescriptor96*>(driver_test().driver()->iobuf_virt());

  uint64_t address;
  memcpy(&address, &descriptors[0].address, sizeof(address));
  EXPECT_EQ(descriptors[0].attr, 0b100'001u);
  EXPECT_EQ(address, zx_system_get_page_size() + 16);
  EXPECT_EQ(descriptors[0].length, 512u);

  memcpy(&address, &descriptors[1].address, sizeof(address));
  EXPECT_EQ(descriptors[1].attr, 0b100'001u);
  EXPECT_EQ(address, zx_system_get_page_size() + 32);
  EXPECT_EQ(descriptors[1].length, 512 * 3);

  // Buffer is greater than one page and gets split across two descriptors.
  memcpy(&address, &descriptors[2].address, sizeof(address));
  EXPECT_EQ(descriptors[2].attr, 0b100'001u);
  EXPECT_EQ(address, zx_system_get_page_size() + 48);
  EXPECT_EQ(descriptors[2].length, zx_system_get_page_size() - 48);

  memcpy(&address, &descriptors[3].address, sizeof(address));
  EXPECT_EQ(descriptors[3].attr, 0b100'001u);
  EXPECT_EQ(address, zx_system_get_page_size());
  EXPECT_EQ(descriptors[3].length, (512 * 10) - zx_system_get_page_size() + 48);

  memcpy(&address, &descriptors[4].address, sizeof(address));
  EXPECT_EQ(descriptors[4].attr, 0b100'011);
  EXPECT_EQ(address, zx_system_get_page_size() + 208);
  EXPECT_EQ(descriptors[4].length, 512 * 7);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, CombineContiguousRegions) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(0)
        .WriteTo(&env.mmio());
  });

  constexpr zx_paddr_t kDescriptorAddress = 0xc000'0000;
  const zx_paddr_t kStartAddress = 0xa7ff'ffff & ~PageMask();

  ASSERT_OK(StartDriver({
      kDescriptorAddress,
      kStartAddress,
      kStartAddress + zx_system_get_page_size(),
      kStartAddress + (zx_system_get_page_size() * 2),
      kStartAddress + (zx_system_get_page_size() * 3),
      0xb000'0000,
  }));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create((zx_system_get_page_size() * 4) + 512, 0, &vmo));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 512,
      .size = zx_system_get_page_size() * 4,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };

  ExpectPmoCount(1);

  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  ExpectPmoCount(1);

  EXPECT_EQ(AdmaSystemAddress::Get(0).ReadFrom(driver_test().driver()->mmio_).reg_value(),
            kDescriptorAddress);
  EXPECT_EQ(AdmaSystemAddress::Get(1).ReadFrom(driver_test().driver()->mmio_).reg_value(), 0u);

  const Sdhci::AdmaDescriptor64* const descriptors =
      reinterpret_cast<Sdhci::AdmaDescriptor64*>(driver_test().driver()->iobuf_virt());

  EXPECT_EQ(descriptors[0].attr, 0b100'001u);
  EXPECT_EQ(descriptors[0].address, kStartAddress + 512);
  EXPECT_EQ(descriptors[0].length, (zx_system_get_page_size() * 4) - 512);

  EXPECT_EQ(descriptors[1].attr, 0b100'011);
  EXPECT_EQ(descriptors[1].address, 0xb000'0000);
  EXPECT_EQ(descriptors[1].length, 512u);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, DiscontiguousRegions) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  constexpr zx_paddr_t kDescriptorAddress = 0xc000'0000;
  constexpr zx_paddr_t kDiscontiguousPageOffset = 0x1'0000'0000;
  const zx_paddr_t kStartAddress = 0xa7ff'ffff & ~PageMask();

  ASSERT_OK(StartDriver({
      kDescriptorAddress,
      kStartAddress,
      kDiscontiguousPageOffset + kStartAddress,
      (2 * kDiscontiguousPageOffset) + kStartAddress,
      (3 * kDiscontiguousPageOffset) + kStartAddress,
      (4 * kDiscontiguousPageOffset) + kStartAddress,
      (4 * kDiscontiguousPageOffset) + kStartAddress + zx_system_get_page_size(),
      (4 * kDiscontiguousPageOffset) + kStartAddress + (2 * zx_system_get_page_size()),
      (5 * kDiscontiguousPageOffset) + kStartAddress,
      (6 * kDiscontiguousPageOffset) + kStartAddress,
      (7 * kDiscontiguousPageOffset) + kStartAddress,
      (7 * kDiscontiguousPageOffset) + kStartAddress + zx_system_get_page_size(),
      (8 * kDiscontiguousPageOffset) + kStartAddress,
  }));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size() * 12, 0, &vmo));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 512,
      .size = (zx_system_get_page_size() * 12) - 512 - 1024,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };

  ExpectPmoCount(1);

  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  ExpectPmoCount(1);

  EXPECT_EQ(AdmaSystemAddress::Get(0).ReadFrom(driver_test().driver()->mmio_).reg_value(),
            kDescriptorAddress);
  EXPECT_EQ(AdmaSystemAddress::Get(1).ReadFrom(driver_test().driver()->mmio_).reg_value(), 0u);

  const auto* const descriptors =
      reinterpret_cast<Sdhci::AdmaDescriptor96*>(driver_test().driver()->iobuf_virt());

  EXPECT_EQ(descriptors[0].attr, 0b100'001u);
  EXPECT_EQ(descriptors[0].get_address(), kStartAddress + 512);
  EXPECT_EQ(descriptors[0].length, zx_system_get_page_size() - 512);

  EXPECT_EQ(descriptors[1].attr, 0b100'001u);
  EXPECT_EQ(descriptors[1].get_address(), kDiscontiguousPageOffset + kStartAddress);
  EXPECT_EQ(descriptors[1].length, zx_system_get_page_size());

  EXPECT_EQ(descriptors[2].attr, 0b100'001u);
  EXPECT_EQ(descriptors[2].get_address(), (2 * kDiscontiguousPageOffset) + kStartAddress);
  EXPECT_EQ(descriptors[2].length, zx_system_get_page_size());

  EXPECT_EQ(descriptors[3].attr, 0b100'001u);
  EXPECT_EQ(descriptors[3].get_address(), (3 * kDiscontiguousPageOffset) + kStartAddress);
  EXPECT_EQ(descriptors[3].length, zx_system_get_page_size());

  EXPECT_EQ(descriptors[4].attr, 0b100'001u);
  EXPECT_EQ(descriptors[4].get_address(), (4 * kDiscontiguousPageOffset) + kStartAddress);
  EXPECT_EQ(descriptors[4].length, zx_system_get_page_size() * 3);

  EXPECT_EQ(descriptors[5].attr, 0b100'001u);
  EXPECT_EQ(descriptors[5].get_address(), (5 * kDiscontiguousPageOffset) + kStartAddress);
  EXPECT_EQ(descriptors[5].length, zx_system_get_page_size());

  EXPECT_EQ(descriptors[6].attr, 0b100'001u);
  EXPECT_EQ(descriptors[6].get_address(), (6 * kDiscontiguousPageOffset) + kStartAddress);
  EXPECT_EQ(descriptors[6].length, zx_system_get_page_size());

  EXPECT_EQ(descriptors[7].attr, 0b100'001u);
  EXPECT_EQ(descriptors[7].get_address(), (7 * kDiscontiguousPageOffset) + kStartAddress);
  EXPECT_EQ(descriptors[7].length, zx_system_get_page_size() * 2);

  EXPECT_EQ(descriptors[8].attr, 0b100'011);
  EXPECT_EQ(descriptors[8].get_address(), (8 * kDiscontiguousPageOffset) + kStartAddress);
  EXPECT_EQ(descriptors[8].length, zx_system_get_page_size() - 1024);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, RegionStartAndEndOffsets) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(0)
        .WriteTo(&env.mmio());
  });

  constexpr zx_paddr_t kDescriptorAddress = 0xc000'0000;
  const zx_paddr_t kStartAddress = 0xa7ff'ffff & ~PageMask();

  ASSERT_OK(StartDriver({
      kDescriptorAddress,
      kStartAddress,
      kStartAddress + zx_system_get_page_size(),
      kStartAddress + (zx_system_get_page_size() * 2),
      kStartAddress + (zx_system_get_page_size() * 3),
  }));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create((zx_system_get_page_size() * 4), 0, &vmo));

  sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 0,
      .size = zx_system_get_page_size(),
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };

  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  const Sdhci::AdmaDescriptor64* const descriptors =
      reinterpret_cast<Sdhci::AdmaDescriptor64*>(driver_test().driver()->iobuf_virt());

  EXPECT_EQ(descriptors[0].attr, 0b100'011);
  EXPECT_EQ(descriptors[0].address, kStartAddress);
  EXPECT_EQ(descriptors[0].length, zx_system_get_page_size());

  buffer.offset = 512;
  buffer.size = zx_system_get_page_size() - 512;

  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(descriptors[0].attr, 0b100'011);
  EXPECT_EQ(descriptors[0].address, kStartAddress + zx_system_get_page_size() + 512);
  EXPECT_EQ(descriptors[0].length, zx_system_get_page_size() - 512);

  buffer.offset = 0;
  buffer.size = zx_system_get_page_size() - 512;

  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(descriptors[0].attr, 0b100'011);
  EXPECT_EQ(descriptors[0].address, kStartAddress + (zx_system_get_page_size() * 2));
  EXPECT_EQ(descriptors[0].length, zx_system_get_page_size() - 512);

  buffer.offset = 512;
  buffer.size = zx_system_get_page_size() - 1024;

  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(descriptors[0].attr, 0b100'011);
  EXPECT_EQ(descriptors[0].address, kStartAddress + (zx_system_get_page_size() * 3) + 512);
  EXPECT_EQ(descriptors[0].length, zx_system_get_page_size() - 1024);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, BufferZeroSize) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(0)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(zx_system_get_page_size() * 4, 0, &vmo));
    EXPECT_OK(driver_test().driver()->SdmmcRegisterVmo(
        1, 0, std::move(vmo), 0, zx_system_get_page_size() * 4, SDMMC_VMO_RIGHT_READ));
  }

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size() * 4, 0, &vmo));

  {
    const sdmmc_buffer_region_t buffers[3] = {
        {
            .buffer =
                {
                    .vmo_id = 1,
                },
            .type = SDMMC_BUFFER_TYPE_VMO_ID,
            .offset = 0,
            .size = 512,
        },
        {
            .buffer =
                {
                    .vmo = vmo.get(),
                },
            .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
            .offset = 0,
            .size = 0,
        },
        {
            .buffer =
                {
                    .vmo_id = 1,
                },
            .type = SDMMC_BUFFER_TYPE_VMO_ID,
            .offset = 512,
            .size = 512,
        },
    };

    const sdmmc_req_t request = {
        .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
        .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
        .arg = 0x1234abcd,
        .blocksize = 512,
        .suppress_error_messages = false,
        .client_id = 0,
        .buffers_list = buffers,
        .buffers_count = std::size(buffers),
    };

    uint32_t response[4] = {};
    EXPECT_NE(ZX_OK, driver_test().driver()->SdmmcRequest(&request, response));
  }

  {
    const sdmmc_buffer_region_t buffers[3] = {
        {
            .buffer =
                {
                    .vmo = vmo.get(),
                },
            .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
            .offset = 0,
            .size = 512,
        },
        {
            .buffer =
                {
                    .vmo_id = 1,
                },
            .type = SDMMC_BUFFER_TYPE_VMO_ID,
            .offset = 0,
            .size = 0,
        },
        {
            .buffer =
                {
                    .vmo = vmo.get(),
                },
            .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
            .offset = 512,
            .size = 512,
        },
    };

    const sdmmc_req_t request = {
        .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
        .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
        .arg = 0x1234abcd,
        .blocksize = 512,
        .suppress_error_messages = false,
        .client_id = 0,
        .buffers_list = buffers,
        .buffers_count = std::size(buffers),
    };

    uint32_t response[4] = {};
    EXPECT_NE(ZX_OK, driver_test().driver()->SdmmcRequest(&request, response));
  }

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, TransferError) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  ASSERT_OK(StartDriver());

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(512, 0, &vmo));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 0,
      .size = 512,
  };
  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };

  driver_test().driver()->InjectTransferError();
  uint32_t response[4] = {};
  EXPECT_NE(ZX_OK, driver_test().driver()->SdmmcRequest(&request, response));

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, MaxTransferSize) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  std::vector<zx_paddr_t> bti_paddrs;
  bti_paddrs.push_back(0x1000'0000'0000'0000);

  for (size_t i = 0; i < 512; i++) {
    // 512 pages, fully discontiguous.
    bti_paddrs.push_back(zx_system_get_page_size() * (i + 1) * 2);
  }

  ASSERT_OK(StartDriver(std::move(bti_paddrs)));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(512, 0, &vmo));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 0,
      .size = 512 * zx_system_get_page_size(),
  };
  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };

  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  const Sdhci::AdmaDescriptor96* const descriptors =
      reinterpret_cast<Sdhci::AdmaDescriptor96*>(driver_test().driver()->iobuf_virt());

  EXPECT_EQ(descriptors[0].attr, 0b100'001u);
  EXPECT_EQ(descriptors[0].get_address(), zx_system_get_page_size() * 2);
  EXPECT_EQ(descriptors[0].length, zx_system_get_page_size());

  EXPECT_EQ(descriptors[511].attr, 0b100'011);
  EXPECT_EQ(descriptors[511].get_address(), zx_system_get_page_size() * 2 * 512);
  EXPECT_EQ(descriptors[511].length, zx_system_get_page_size());

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, TransferSizeExceeded) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  std::vector<zx_paddr_t> bti_paddrs;
  bti_paddrs.push_back(0x1000'0000'0000'0000);

  for (size_t i = 0; i < 513; i++) {
    bti_paddrs.push_back(zx_system_get_page_size() * (i + 1) * 2);
  }

  ASSERT_OK(StartDriver(std::move(bti_paddrs)));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(512, 0, &vmo));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 0,
      .size = 513 * zx_system_get_page_size(),
  };
  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };

  uint32_t response[4] = {};
  EXPECT_NE(ZX_OK, driver_test().driver()->SdmmcRequest(&request, response));

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, DmaSplitSizeAndAligntmentBoundaries) {
  driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
    Capabilities0::Get()
        .FromValue(0)
        .set_adma2_support(1)
        .set_v3_64_bit_system_address_support(1)
        .WriteTo(&env.mmio());
  });

  constexpr zx_paddr_t kDescriptorAddress = 0xc000'0000;
  std::vector<zx_paddr_t> paddrs;
  // Generate a single contiguous physical region.
  paddrs.push_back(kDescriptorAddress);
  for (zx_paddr_t p = 0x1'0001'8000; p < 0x1'0010'0000; p += zx_system_get_page_size()) {
    paddrs.push_back(p);
  }

  ASSERT_OK(StartDriver(std::move(paddrs), fuchsia_hardware_sdhci::Quirk::kUseDmaBoundaryAlignment,
                        0x2'0000));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(1024, 0, &vmo));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo = vmo.get(),
          },
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 0x1'8000,
      .size = 0x4'0000,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  EXPECT_EQ(AdmaSystemAddress::Get(0).ReadFrom(driver_test().driver()->mmio_).reg_value(),
            kDescriptorAddress);
  EXPECT_EQ(AdmaSystemAddress::Get(1).ReadFrom(driver_test().driver()->mmio_).reg_value(), 0u);

  const Sdhci::AdmaDescriptor96* const descriptors =
      reinterpret_cast<Sdhci::AdmaDescriptor96*>(driver_test().driver()->iobuf_virt());

  // Region split due to alignment.
  EXPECT_EQ(descriptors[0].attr, 0b100'001u);
  EXPECT_EQ(descriptors[0].get_address(), 0x1'0001'8000u);
  EXPECT_EQ(descriptors[0].length, 0x8000);

  // Region split due to both alignment and descriptor max size.
  EXPECT_EQ(descriptors[1].attr, 0b100'001u);
  EXPECT_EQ(descriptors[1].get_address(), 0x1'0002'0000u);
  EXPECT_EQ(descriptors[1].length, 0);  // Zero length -> 0x1'0000 bytes
  EXPECT_EQ(descriptors[2].attr, 0b100'001u);

  // Region split due to descriptor max size.
  EXPECT_EQ(descriptors[2].get_address(), 0x1'0003'0000u);
  EXPECT_EQ(descriptors[2].length, 0);

  EXPECT_EQ(descriptors[3].attr, 0b100'001u);
  EXPECT_EQ(descriptors[3].get_address(), 0x1'0004'0000u);
  EXPECT_EQ(descriptors[3].length, 0);

  EXPECT_EQ(descriptors[4].attr, 0b100'011);
  EXPECT_EQ(descriptors[4].get_address(), 0x1'0005'0000u);
  EXPECT_EQ(descriptors[4].length, 0x8000);

  ASSERT_OK(StopDriver());
}

TEST_F(SdhciTest, BufferedRead) {
  ASSERT_OK(StartDriver(fuchsia_hardware_sdhci::Quirk::kNoDma));

  constexpr uint32_t kTestWord = 0x1234'5678;
  BufferData::Get().FromValue(kTestWord).WriteTo(driver_test().driver()->mmio_);

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(512 * 8, 0, &vmo));

  const sdmmc_buffer_region_t buffer = {
      .buffer = {.vmo = vmo.get()},
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 512,
      .size = 512 * 6,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  ASSERT_OK(StopDriver());

  uint32_t actual;

  // Make sure the test word was written to the beginning and end of the buffer, but not outside the
  // range we wanted.

  EXPECT_OK(vmo.read(&actual, 512 - sizeof(actual), sizeof(actual)));
  EXPECT_NE(actual, kTestWord);

  EXPECT_OK(vmo.read(&actual, 512, sizeof(actual)));
  EXPECT_EQ(actual, kTestWord);

  EXPECT_OK(vmo.read(&actual, (512 * 7) - sizeof(actual), sizeof(actual)));
  EXPECT_EQ(actual, kTestWord);

  EXPECT_OK(vmo.read(&actual, (512 * 7), sizeof(actual)));
  EXPECT_NE(actual, kTestWord);
}

TEST_F(SdhciTest, BufferedWrite) {
  ASSERT_OK(StartDriver(fuchsia_hardware_sdhci::Quirk::kNoDma));

  constexpr uint32_t kTestWord = 0x1234'5678;

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(512 * 8, 0, &vmo));
  EXPECT_OK(vmo.write(&kTestWord, (512 * 7) - sizeof(kTestWord), sizeof(kTestWord)));

  const sdmmc_buffer_region_t buffer = {
      .buffer = {.vmo = vmo.get()},
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = 512,
      .size = 512 * 6,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };
  uint32_t response[4] = {};
  EXPECT_OK(driver_test().driver()->SdmmcRequest(&request, response));

  // The data port should hold the last word from the buffer.
  EXPECT_EQ(BufferData::Get().ReadFrom(driver_test().driver()->mmio_).reg_value(), kTestWord);

  ASSERT_OK(StopDriver());
}

}  // namespace sdhci

FUCHSIA_DRIVER_EXPORT(sdhci::TestSdhci);
