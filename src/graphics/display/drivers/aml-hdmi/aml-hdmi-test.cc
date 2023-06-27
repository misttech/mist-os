// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/aml-hdmi/aml-hdmi.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/hdmi/base.h>

#include <queue>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <gtest/gtest.h>
#include <mock-mmio-range/mock-mmio-range.h>

#include "src/lib/testing/predicates/status.h"

namespace aml_hdmi {

namespace {

using HdmiClient = fidl::WireSyncClient<fuchsia_hardware_hdmi::Hdmi>;
using fuchsia_hardware_hdmi::wire::ColorDepth;
using fuchsia_hardware_hdmi::wire::ColorFormat;

enum class HdmiDwFn {
  kConfigHdmitx,
  kSetupInterrupts,
  kReset,
  kSetupScdc,
  kResetFc,
  kSetFcScramblerCtrl,
};

class AmlHdmiTest;

class FakeHdmiDw : public hdmi_dw::HdmiDw {
 public:
  explicit FakeHdmiDw(HdmiIpBase* base, AmlHdmiTest* test) : HdmiDw(base), test_(test) {}

  void ConfigHdmitx(const fuchsia_hardware_hdmi::wire::DisplayMode& mode,
                    const hdmi_dw::hdmi_param_tx& p) override;
  void SetupInterrupts() override;
  void Reset() override;
  void SetupScdc(bool is4k) override;
  void ResetFc() override;
  void SetFcScramblerCtrl(bool is4k) override;

 private:
  AmlHdmiTest* test_;
};

// Stubbed implementation that drops all register I/O.
class StubHdmiIpBase : public HdmiIpBase {
 public:
  StubHdmiIpBase() = default;
  ~StubHdmiIpBase() override = default;

  void WriteIpReg(uint32_t addr, uint32_t data) override {}
  uint32_t ReadIpReg(uint32_t addr) override { return 0; }
};

class FakeAmlHdmiDevice : public AmlHdmiDevice {
 public:
  static std::unique_ptr<FakeAmlHdmiDevice> Create(AmlHdmiTest* test, fdf::MmioBuffer mmio) {
    auto device = std::make_unique<FakeAmlHdmiDevice>(test, std::move(mmio));
    return device;
  }

  explicit FakeAmlHdmiDevice(AmlHdmiTest* test, fdf::MmioBuffer mmio)
      : AmlHdmiDevice(nullptr, std::move(mmio),
                      std::make_unique<FakeHdmiDw>(&stub_hdmi_ip_base_, test)) {
    loop_.StartThread("fake-aml-hdmi-test-thread");
  }

  void BindServer(fidl::ServerEnd<fuchsia_hardware_hdmi::Hdmi> server) {
    binding_ = fidl::BindServer(loop_.dispatcher(), std::move(server), this);
  }

 private:
  // Absorbs all register
  StubHdmiIpBase stub_hdmi_ip_base_;

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_hdmi::Hdmi>> binding_;
};

class AmlHdmiTest : public testing::Test {
 public:
  void SetUp() override {
    dut_ = FakeAmlHdmiDevice::Create(this, mmio_range_.GetMmioBuffer());
    ASSERT_TRUE(dut_);

    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_hdmi::Hdmi>();
    ASSERT_TRUE(endpoints.is_ok());
    dut_->BindServer(std::move(endpoints->server));
    hdmi_client_.Bind(std::move(endpoints->client));
  }

  void TearDown() override {
    ASSERT_EQ(expected_dw_calls_.size(), 0u);
    mmio_range_.CheckAllAccessesReplayed();
  }

  void HdmiDwCall(HdmiDwFn func) {
    ASSERT_FALSE(expected_dw_calls_.empty());
    ASSERT_EQ(expected_dw_calls_.front(), func);
    expected_dw_calls_.pop();
  }
  void ExpectHdmiDwConfigHdmitx() { expected_dw_calls_.push(HdmiDwFn::kConfigHdmitx); }
  void ExpectHdmiDwSetupInterrupts() { expected_dw_calls_.push(HdmiDwFn::kSetupInterrupts); }
  void ExpectHdmiDwReset() { expected_dw_calls_.push(HdmiDwFn::kReset); }
  void ExpectHdmiDwSetupScdc() { expected_dw_calls_.push(HdmiDwFn::kSetupScdc); }
  void ExpectHdmiDwResetFc() { expected_dw_calls_.push(HdmiDwFn::kResetFc); }
  void ExpectHdmiDwSetFcScramblerCtrl() { expected_dw_calls_.push(HdmiDwFn::kSetFcScramblerCtrl); }

 protected:
  constexpr static int kMmioRangeSize = 0x10000;
  ddk_mock::MockMmioRange mmio_range_{kMmioRangeSize, ddk_mock::MockMmioRange::Size::k32};

  std::unique_ptr<FakeAmlHdmiDevice> dut_;
  HdmiClient hdmi_client_;

  std::queue<HdmiDwFn> expected_dw_calls_;
};

void FakeHdmiDw::ConfigHdmitx(const fuchsia_hardware_hdmi::wire::DisplayMode& mode,
                              const hdmi_dw::hdmi_param_tx& p) {
  test_->HdmiDwCall(HdmiDwFn::kConfigHdmitx);
}

void FakeHdmiDw::SetupInterrupts() { test_->HdmiDwCall(HdmiDwFn::kSetupInterrupts); }

void FakeHdmiDw::Reset() { test_->HdmiDwCall(HdmiDwFn::kReset); }

void FakeHdmiDw::SetupScdc(bool is4k) { test_->HdmiDwCall(HdmiDwFn::kSetupScdc); }

void FakeHdmiDw::ResetFc() { test_->HdmiDwCall(HdmiDwFn::kResetFc); }

void FakeHdmiDw::SetFcScramblerCtrl(bool is4k) { test_->HdmiDwCall(HdmiDwFn::kSetFcScramblerCtrl); }

TEST_F(AmlHdmiTest, ReadAmlogicRegister) {
  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = 0x12 * 4 + 0x8000, .value = 0x1234},
  }));
  auto pval_aml = hdmi_client_->ReadReg(0x12);
  ASSERT_OK(pval_aml.status());
  EXPECT_EQ(pval_aml.value().val, 0x1234u);
}

TEST_F(AmlHdmiTest, ReadDesignwareRegister) {
  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = 0x03, .value = 0x21, .size = ddk_mock::MockMmioRange::Size::k8},
  }));
  auto pval_dwc = hdmi_client_->ReadReg((0x10UL << 24) + 0x03);
  ASSERT_OK(pval_dwc.status());
  EXPECT_EQ(pval_dwc.value().val, 0x21u);
}

TEST_F(AmlHdmiTest, WriteAmlogicRegister) {
  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = 0x05 * 4 + 0x8000, .value = 0x4321, .write = true},
  }));
  auto pres_aml = hdmi_client_->WriteReg(0x05, 0x4321);
  ASSERT_OK(pres_aml.status());
}

TEST_F(AmlHdmiTest, WriteDesignwareRegister) {
  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = 0x420, .value = 0x15, .write = true, .size = ddk_mock::MockMmioRange::Size::k8},
  }));
  auto pres_dwc = hdmi_client_->WriteReg((0x10UL << 24) + 0x420, 0x2415);
  ASSERT_OK(pres_dwc.status());
}

// Register addresses from the A311D datasheet section 10.2.3.44 "HDCP2.2 IP
// Register Access".
constexpr int kHdmiTxTopSwResetOffset = 0x00 * 4 + 0x8000;
constexpr int kHdmiTxTopClkCntlOffset = 0x01 * 4 + 0x8000;
constexpr int kHdmiTxTopIntrMaskn = 0x03 * 4 + 0x8000;
constexpr int kHdmiTxTopIntrStatClr = 0x05 * 4 + 0x8000;
constexpr int kHdmiTxTopBistCntl = 0x06 * 4 + 0x8000;
constexpr int kHdmiTxTopTmdsClkPttn01 = 0x0a * 4 + 0x8000;
constexpr int kHdmiTxTopTmdsClkPttn23 = 0x0b * 4 + 0x8000;
constexpr int kHdmiTxTopTmdsClkPttnCntl = 0x0c * 4 + 0x8000;

TEST_F(AmlHdmiTest, ResetTest) {
  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kHdmiTxTopSwResetOffset, .value = 0, .write = true},
      {.address = kHdmiTxTopClkCntlOffset, .value = 0xff, .write = true},
  }));
  auto pres = hdmi_client_->Reset(1);
  ASSERT_OK(pres.status());
}

TEST_F(AmlHdmiTest, ModeSetTest) {
  fidl::Arena allocator;
  fuchsia_hardware_hdmi::wire::StandardDisplayMode standard_display_mode{
      .pixel_clock_10khz = 0,
      .h_addressable = 0,
      .h_front_porch = 0,
      .h_sync_pulse = 0,
      .h_blanking = 0,
      .v_addressable = 0,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 0,
  };
  fuchsia_hardware_hdmi::wire::ColorParam color{
      .input_color_format = ColorFormat::kCfRgb,
      .output_color_format = ColorFormat::kCfRgb,
      .color_depth = ColorDepth::kCd24B,
  };
  fuchsia_hardware_hdmi::wire::DisplayMode mode(allocator);
  mode.set_mode(allocator, standard_display_mode);
  mode.set_color(color);

  mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kHdmiTxTopBistCntl, .value = 1 << 12, .write = true},
      {.address = kHdmiTxTopIntrStatClr, .value = 0x1f, .write = true},
      {.address = kHdmiTxTopIntrMaskn, .value = 0x9f, .write = true},
      {.address = kHdmiTxTopTmdsClkPttn01, .value = 0x001f'001f, .write = true},
      {.address = kHdmiTxTopTmdsClkPttn23, .value = 0x001f'001f, .write = true},
      {.address = kHdmiTxTopTmdsClkPttnCntl, .value = 0x01, .write = true},
      {.address = kHdmiTxTopTmdsClkPttnCntl, .value = 0x02, .write = true},
  }));

  ExpectHdmiDwConfigHdmitx();
  ExpectHdmiDwSetupInterrupts();
  ExpectHdmiDwReset();
  ExpectHdmiDwSetFcScramblerCtrl();
  ExpectHdmiDwSetupScdc();
  ExpectHdmiDwResetFc();
  auto pres = hdmi_client_->ModeSet(1, mode);
  ASSERT_OK(pres.status());
}

}  // namespace

}  // namespace aml_hdmi
