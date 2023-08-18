// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <unistd.h>

#include <mock-mmio-reg/mock-mmio-reg.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <zxtest/zxtest.h>

#include "adc-buttons-device.h"

namespace adc_buttons_device {

class TestSarAdc : public AmlSaradcDevice {
 public:
  TestSarAdc(fdf::MmioBuffer adc_mmio, fdf::MmioBuffer ao_mmio, zx::interrupt irq)
      : AmlSaradcDevice(std::move(adc_mmio), std::move(ao_mmio), std::move(irq)) {}
  void HwInit() override {}
  void Shutdown() override {}

  zx_status_t GetSample(uint32_t channel, uint32_t* outval) override {
    if (channel != ch_) {
      return ZX_ERR_INVALID_ARGS;
    }
    *outval = value_;
    return ZX_OK;
  }
  void SetReadValue(uint32_t ch, uint32_t value) {
    ch_ = ch;
    value_ = value;
  }

 private:
  uint32_t ch_;
  uint32_t value_;
};

class AdcButtonsDeviceTest : public zxtest::Test {
 public:
  void SetUp() override {
    constexpr size_t kRegSize = A311D_SARADC_LENGTH / sizeof(uint32_t);  // in 32 bits chunks.
    ddk_mock::MockMmioRegRegion mock0(sizeof(uint32_t), kRegSize);
    ddk_mock::MockMmioRegRegion mock1(sizeof(uint32_t), kRegSize);
    zx::interrupt irq;
    auto test_adc =
        std::make_unique<TestSarAdc>(mock0.GetMmioBuffer(), mock1.GetMmioBuffer(), std::move(irq));
    adc_ = test_adc.get();
    adc_->SetReadValue(kChannel, 0);

    const std::map<uint32_t, std::vector<fuchsia_buttons::Button>> kConfig = {
        {kChannel,
         {fuchsia_buttons::Button()
              .types(std::vector<fuchsia_input_report::ConsumerControlButton>{
                  fuchsia_input_report::ConsumerControlButton::kFunction})
              .button_config(
                  fuchsia_buttons::ButtonConfig::WithAdc(fuchsia_buttons::AdcButtonConfig()
                                                             .channel_idx(kChannel)
                                                             .release_threshold(kReleaseThreshold)
                                                             .press_threshold(kPressThreshold)))}}};
    const std::set<fuchsia_input_report::ConsumerControlButton> kButtons = {
        fuchsia_input_report::ConsumerControlButton::kFunction};
    device_ = std::make_unique<AdcButtonsDevice>(loop_.dispatcher(), std::move(test_adc),
                                                 kPollingRateUsec, std::move(kConfig),
                                                 std::move(kButtons));

    loop_.StartThread("adc-buttons-test-thread");
    auto endpoints = fidl::CreateEndpoints<fuchsia_input_report::InputDevice>();
    ASSERT_OK(endpoints.status_value());
    fidl::BindServer(loop_.dispatcher(), std::move(endpoints->server), device_.get());
    client_.Bind(std::move(endpoints->client));
  }

  void TearDown() override {
    device_->Shutdown();
    loop_.Shutdown();
  }

  uint32_t polling_rate_usec() { return device_->polling_rate_usec_; }

 private:
  std::unique_ptr<AdcButtonsDevice> device_;

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};

 protected:
  static constexpr uint32_t kPollingRateUsec = 1'000;
  static constexpr uint32_t kChannel = 2;
  static constexpr uint32_t kReleaseThreshold = 30;
  static constexpr uint32_t kPressThreshold = 10;

  TestSarAdc* adc_;
  fidl::WireSyncClient<fuchsia_input_report::InputDevice> client_;
};

TEST_F(AdcButtonsDeviceTest, GetDescriptorTest) {
  auto result = client_->GetDescriptor();
  ASSERT_TRUE(result.ok());

  EXPECT_FALSE(result->descriptor.has_keyboard());
  EXPECT_FALSE(result->descriptor.has_mouse());
  EXPECT_FALSE(result->descriptor.has_sensor());
  EXPECT_FALSE(result->descriptor.has_touch());

  ASSERT_TRUE(result->descriptor.has_device_info());
  EXPECT_EQ(result->descriptor.device_info().vendor_id,
            static_cast<uint32_t>(fuchsia_input_report::wire::VendorId::kGoogle));
  EXPECT_EQ(result->descriptor.device_info().product_id,
            static_cast<uint32_t>(fuchsia_input_report::wire::VendorGoogleProductId::kAdcButtons));
  EXPECT_EQ(result->descriptor.device_info().polling_rate, polling_rate_usec());

  ASSERT_TRUE(result->descriptor.has_consumer_control());
  ASSERT_TRUE(result->descriptor.consumer_control().has_input());
  ASSERT_TRUE(result->descriptor.consumer_control().input().has_buttons());
  EXPECT_EQ(result->descriptor.consumer_control().input().buttons().count(), 1);
  EXPECT_EQ(result->descriptor.consumer_control().input().buttons()[0],
            fuchsia_input_report::wire::ConsumerControlButton::kFunction);
}

TEST_F(AdcButtonsDeviceTest, ReadInputReportsTest) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_input_report::InputReportsReader>();
  ASSERT_OK(endpoints);
  auto result = client_->GetInputReportsReader(std::move(endpoints->server));
  ASSERT_TRUE(result.ok());
  // Ensure that the reader has been registered with the client before moving on.
  ASSERT_TRUE(client_->GetDescriptor().ok());
  auto reader =
      fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(endpoints->client));
  EXPECT_TRUE(reader.is_valid());

  adc_->SetReadValue(kChannel, 20);
  // Wait for the device to pick this up.
  usleep(2 * polling_rate_usec());

  {
    auto result = reader->ReadInputReports();
    ASSERT_OK(result.status());
    ASSERT_FALSE(result.value().is_error());
    auto& reports = result.value().value()->reports;

    ASSERT_EQ(1, reports.count());
    auto report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    EXPECT_EQ(consumer_control.pressed_buttons().count(), 1);
    EXPECT_EQ(consumer_control.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kFunction);
  };

  adc_->SetReadValue(kChannel, 40);
  // Wait for the device to pick this up.
  usleep(2 * polling_rate_usec());

  {
    auto result = reader->ReadInputReports();
    ASSERT_OK(result.status());
    ASSERT_FALSE(result.value().is_error());
    auto& reports = result.value().value()->reports;

    ASSERT_EQ(1, reports.count());
    auto report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_consumer_control());
    auto& consumer_control = report.consumer_control();

    ASSERT_TRUE(consumer_control.has_pressed_buttons());
    EXPECT_EQ(consumer_control.pressed_buttons().count(), 0);
  };
}

}  // namespace adc_buttons_device
