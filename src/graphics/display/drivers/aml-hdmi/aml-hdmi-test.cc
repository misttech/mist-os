// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/aml-hdmi/aml-hdmi.h"

#include <lib/async-loop/cpp/loop.h>

#include <list>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <gtest/gtest.h>
#include <mock-mmio-range/mock-mmio-range.h>

#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/lib/testing/predicates/status.h"

namespace aml_hdmi {

namespace {

using HdmiClient = fidl::WireSyncClient<fuchsia_hardware_hdmi::Hdmi>;

enum class HdmiTransmitterControllerCall : uint8_t {
  kConfigHdmitx,
  kSetupInterrupts,
  kReset,
  kSetupScdc,
  kResetFc,
  kSetFcScramblerCtrl,
};

// TODO(fxbug.dev/136257): Consider replacing the mock class with a fake
// HdmiTransmitterController implementation instead.
class MockHdmiTransmitterController : public designware_hdmi::HdmiTransmitterController {
 public:
  MockHdmiTransmitterController() = default;
  ~MockHdmiTransmitterController() { EXPECT_TRUE(expected_calls_.empty()); }

  zx_status_t InitHw() override { return ZX_OK; }
  zx_status_t EdidTransfer(const i2c_impl_op_t* op_list, size_t op_count) override { return ZX_OK; }

  void ConfigHdmitx(const designware_hdmi::ColorParam& color_param,
                    const display::DisplayTiming& mode,
                    const designware_hdmi::hdmi_param_tx& p) override {
    OnHdmiTransmitterControllerCall(HdmiTransmitterControllerCall::kConfigHdmitx);
  }
  void SetupInterrupts() override {
    OnHdmiTransmitterControllerCall(HdmiTransmitterControllerCall::kSetupInterrupts);
  }
  void Reset() override { OnHdmiTransmitterControllerCall(HdmiTransmitterControllerCall::kReset); }
  void SetupScdc(bool is4k) override {
    OnHdmiTransmitterControllerCall(HdmiTransmitterControllerCall::kSetupScdc);
  }
  void ResetFc() override {
    OnHdmiTransmitterControllerCall(HdmiTransmitterControllerCall::kResetFc);
  }
  void SetFcScramblerCtrl(bool is4k) override {
    OnHdmiTransmitterControllerCall(HdmiTransmitterControllerCall::kSetFcScramblerCtrl);
  }

  void OnHdmiTransmitterControllerCall(HdmiTransmitterControllerCall call) {
    ASSERT_FALSE(expected_calls_.empty());
    EXPECT_EQ(expected_calls_.front(), call);
    expected_calls_.pop_front();
  }

  void ExpectCalls(const cpp20::span<const HdmiTransmitterControllerCall> expected_calls) {
    std::copy(expected_calls.begin(), expected_calls.end(), std::back_inserter(expected_calls_));
  }

  void PrintRegisters() override {}

 private:
  std::list<HdmiTransmitterControllerCall> expected_calls_;
};

class AmlHdmiTest : public testing::Test {
 public:
  void SetUp() override {
    loop_.StartThread("aml-hdmi-test-thread");

    auto mock_hdmitx_controller = std::make_unique<MockHdmiTransmitterController>();
    mock_hdmitx_controller_ = mock_hdmitx_controller.get();

    // TODO(fxbug.dev/123426): Use a fake SMC resource, when the
    // implementation lands.
    dut_ =
        std::make_unique<AmlHdmiDevice>(nullptr, top_level_mmio_range_.GetMmioBuffer(),
                                        std::move(mock_hdmitx_controller), /*smc=*/zx::resource{});
    ASSERT_TRUE(dut_);

    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_hdmi::Hdmi>();
    ASSERT_TRUE(endpoints.is_ok());

    binding_ = fidl::BindServer(loop_.dispatcher(), std::move(endpoints->server), dut_.get());
    hdmi_client_.Bind(std::move(endpoints->client));
  }

  void TearDown() override { top_level_mmio_range_.CheckAllAccessesReplayed(); }

 protected:
  constexpr static int kTopLevelMmioRangeSize = 0x8000;
  ddk_mock::MockMmioRange top_level_mmio_range_{kTopLevelMmioRangeSize,
                                                ddk_mock::MockMmioRange::Size::k32};

  std::unique_ptr<AmlHdmiDevice> dut_;

  // Owned by `dut_`.
  MockHdmiTransmitterController* mock_hdmitx_controller_ = nullptr;

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_hdmi::Hdmi>> binding_;

  HdmiClient hdmi_client_;
};

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
  const fuchsia_hardware_hdmi::wire::DisplayMode mode =
      fuchsia_hardware_hdmi::wire::DisplayMode::Builder(allocator)
          .mode(standard_display_mode)
          .color(color)
          .Build();

  top_level_mmio_range_.Expect(ddk_mock::MockMmioRange::AccessList({
      {.address = kHdmiTxTopBistCntl, .value = 1 << 12, .write = true},
      {.address = kHdmiTxTopIntrStatClr, .value = 0x1f, .write = true},
      {.address = kHdmiTxTopIntrMaskn, .value = 0x9f, .write = true},
      {.address = kHdmiTxTopTmdsClkPttn01, .value = 0x001f'001f, .write = true},
      {.address = kHdmiTxTopTmdsClkPttn23, .value = 0x001f'001f, .write = true},
      {.address = kHdmiTxTopTmdsClkPttnCntl, .value = 0x01, .write = true},
      {.address = kHdmiTxTopTmdsClkPttnCntl, .value = 0x02, .write = true},
  }));

  static constexpr HdmiTransmitterControllerCall kExpectedCalls[] = {
      HdmiTransmitterControllerCall::kConfigHdmitx,
      HdmiTransmitterControllerCall::kSetupInterrupts,
      HdmiTransmitterControllerCall::kReset,
      HdmiTransmitterControllerCall::kSetFcScramblerCtrl,
      HdmiTransmitterControllerCall::kSetupScdc,
      HdmiTransmitterControllerCall::kResetFc,
  };
  mock_hdmitx_controller_->ExpectCalls(kExpectedCalls);

  auto pres = hdmi_client_->ModeSet(1, mode);
  ASSERT_OK(pres.status());
}

}  // namespace

}  // namespace aml_hdmi
