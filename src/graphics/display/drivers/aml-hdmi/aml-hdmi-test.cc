// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/aml-hdmi/aml-hdmi.h"

#include <lib/async-loop/cpp/loop.h>

#include <queue>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <gtest/gtest.h>
#include <mock-mmio-range/mock-mmio-range.h>

#include "src/devices/testing/fake-mmio-reg/include/fake-mmio-reg/fake-mmio-reg.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/lib/testing/predicates/status.h"

namespace aml_hdmi {

namespace {

using HdmiClient = fidl::WireSyncClient<fuchsia_hardware_hdmi::Hdmi>;

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
  explicit FakeHdmiDw(AmlHdmiTest* test, fdf::MmioBuffer controller_mmio)
      : HdmiDw(std::move(controller_mmio)), test_(test) {}

  void ConfigHdmitx(const hdmi_dw::ColorParam& color_param, const display::DisplayTiming& mode,
                    const hdmi_dw::hdmi_param_tx& p) override;
  void SetupInterrupts() override;
  void Reset() override;
  void SetupScdc(bool is4k) override;
  void ResetFc() override;
  void SetFcScramblerCtrl(bool is4k) override;

 private:
  AmlHdmiTest* test_;
};

class FakeAmlHdmiDevice : public AmlHdmiDevice {
 public:
  static std::unique_ptr<FakeAmlHdmiDevice> Create(AmlHdmiTest* test,
                                                   fdf::MmioBuffer controller_ip_mmio,
                                                   fdf::MmioBuffer top_level_mmio,
                                                   zx::resource smc) {
    auto device = std::make_unique<FakeAmlHdmiDevice>(test, std::move(controller_ip_mmio),
                                                      std::move(top_level_mmio), std::move(smc));
    return device;
  }

  explicit FakeAmlHdmiDevice(AmlHdmiTest* test, fdf::MmioBuffer controller_ip_mmio,
                             fdf::MmioBuffer top_level_mmio, zx::resource smc)
      : AmlHdmiDevice(nullptr, std::move(top_level_mmio),
                      std::make_unique<FakeHdmiDw>(test, std::move(controller_ip_mmio)),
                      std::move(smc)) {
    loop_.StartThread("fake-aml-hdmi-test-thread");
  }

  void BindServer(fidl::ServerEnd<fuchsia_hardware_hdmi::Hdmi> server) {
    binding_ = fidl::BindServer(loop_.dispatcher(), std::move(server), this);
  }

 private:
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_hdmi::Hdmi>> binding_;
};

class AmlHdmiTest : public testing::Test {
 public:
  void SetUp() override {
    // TODO(fxbug.dev/123426): Use a fake SMC resource, when the
    // implementation lands.
    dut_ = FakeAmlHdmiDevice::Create(this, controller_ip_mmio_range_.GetMmioBuffer(),
                                     top_level_mmio_range_.GetMmioBuffer(), /*smc=*/zx::resource{});
    ASSERT_TRUE(dut_);

    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_hdmi::Hdmi>();
    ASSERT_TRUE(endpoints.is_ok());
    dut_->BindServer(std::move(endpoints->server));
    hdmi_client_.Bind(std::move(endpoints->client));
  }

  void TearDown() override {
    ASSERT_EQ(expected_dw_calls_.size(), 0u);
    top_level_mmio_range_.CheckAllAccessesReplayed();
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
  // This test doesn't check the register read / write operations on the
  // Controller IP region (which should be covered by hdmi-dw tests); so we
  // use a `FakeMmioRegRegion` for this region.
  constexpr static size_t kControllerIpMmioRegCount = 0x8000;
  ddk_fake::FakeMmioRegRegion controller_ip_mmio_range_{/*reg_size=*/1, kControllerIpMmioRegCount};

  constexpr static int kTopLevelMmioRangeSize = 0x8000;
  ddk_mock::MockMmioRange top_level_mmio_range_{kTopLevelMmioRangeSize,
                                                ddk_mock::MockMmioRange::Size::k32};

  std::unique_ptr<FakeAmlHdmiDevice> dut_;
  HdmiClient hdmi_client_;

  std::queue<HdmiDwFn> expected_dw_calls_;
};

void FakeHdmiDw::ConfigHdmitx(const hdmi_dw::ColorParam& color_param,
                              const display::DisplayTiming& mode, const hdmi_dw::hdmi_param_tx& p) {
  test_->HdmiDwCall(HdmiDwFn::kConfigHdmitx);
}

void FakeHdmiDw::SetupInterrupts() { test_->HdmiDwCall(HdmiDwFn::kSetupInterrupts); }

void FakeHdmiDw::Reset() { test_->HdmiDwCall(HdmiDwFn::kReset); }

void FakeHdmiDw::SetupScdc(bool is4k) { test_->HdmiDwCall(HdmiDwFn::kSetupScdc); }

void FakeHdmiDw::ResetFc() { test_->HdmiDwCall(HdmiDwFn::kResetFc); }

void FakeHdmiDw::SetFcScramblerCtrl(bool is4k) { test_->HdmiDwCall(HdmiDwFn::kSetFcScramblerCtrl); }

// Register addresses from the A311D datasheet section 10.2.3.44 "HDCP2.2 IP
// Register Access".
constexpr int kHdmiTxTopSwResetOffset = 0x00 * 4;
constexpr int kHdmiTxTopClkCntlOffset = 0x01 * 4;
constexpr int kHdmiTxTopIntrMaskn = 0x03 * 4;
constexpr int kHdmiTxTopIntrStatClr = 0x05 * 4;
constexpr int kHdmiTxTopBistCntl = 0x06 * 4;
constexpr int kHdmiTxTopTmdsClkPttn01 = 0x0a * 4;
constexpr int kHdmiTxTopTmdsClkPttn23 = 0x0b * 4;
constexpr int kHdmiTxTopTmdsClkPttnCntl = 0x0c * 4;

TEST_F(AmlHdmiTest, ResetTest) {
  top_level_mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kHdmiTxTopSwResetOffset, .value = 0, .write = true},
      {.address = kHdmiTxTopClkCntlOffset, .value = 0xff, .write = true},
  }));
  auto pres = hdmi_client_->Reset(1);
  ASSERT_OK(pres.status());
}

TEST_F(AmlHdmiTest, ModeSetTest) {
  fidl::Arena allocator;
  // TODO(fxbug.dev/136159): Use valid synthetic values for timings.
  fuchsia_hardware_hdmi::wire::StandardDisplayMode standard_display_mode{
      .pixel_clock_khz = 0,
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
      .input_color_format = fuchsia_hardware_hdmi::wire::ColorFormat::kCfRgb,
      .output_color_format = fuchsia_hardware_hdmi::wire::ColorFormat::kCfRgb,
      .color_depth = fuchsia_hardware_hdmi::wire::ColorDepth::kCd24B,
  };
  fuchsia_hardware_hdmi::wire::DisplayMode mode(allocator);
  mode.set_mode(allocator, standard_display_mode);
  mode.set_color(color);

  top_level_mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
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
