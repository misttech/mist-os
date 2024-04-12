// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "adc-buttons.h"

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <zxtest/zxtest.h>

namespace adc_buttons_device {

struct IncomingNamespace {
  fdf_testing::TestNode node_{std::string("root")};
  fdf_testing::TestEnvironment env_{fdf::Dispatcher::GetCurrent()->get()};
  compat::DeviceServer device_server_;

  class FakeAdcServer : public fidl::Server<fuchsia_hardware_adc::Device> {
   public:
    void set_resolution(uint8_t resolution) { resolution_ = resolution; }
    void set_sample(uint32_t sample) { sample_ = sample; }
    void set_normalized_sample(float normalized_sample) { normalized_sample_ = normalized_sample; }

    void GetResolution(GetResolutionCompleter::Sync& completer) override {
      completer.Reply(fit::ok(resolution_));
    }
    void GetSample(GetSampleCompleter::Sync& completer) override {
      completer.Reply(fit::ok(sample_));
    }
    void GetNormalizedSample(GetNormalizedSampleCompleter::Sync& completer) override {
      completer.Reply(fit::ok(normalized_sample_));
    }

    fuchsia_hardware_adc::Service::InstanceHandler GetInstanceHandler() {
      return fuchsia_hardware_adc::Service::InstanceHandler({
          .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                            fidl::kIgnoreBindingClosure),
      });
    }

   private:
    uint8_t resolution_ = 0;
    uint32_t sample_ = 0;
    float normalized_sample_ = 0;

    fidl::ServerBindingGroup<fuchsia_hardware_adc::Device> bindings_;
  };
  FakeAdcServer fake_adc_server_;
};

class AdcButtonsDeviceTest : public zxtest::Test {
 public:
  void SetUp() override {
    fuchsia_driver_framework::DriverStartArgs start_args;
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      auto start_args_result = incoming->node_.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());
      start_args = std::move(start_args_result->start_args);

      auto init_result =
          incoming->env_.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());

      incoming->device_server_.Init(component::kDefaultInstance, "");

      // Serve metadata.
      auto func_types = std::vector<fuchsia_input_report::ConsumerControlButton>{
          fuchsia_input_report::ConsumerControlButton::kFunction};
      auto func_adc_config = fuchsia_buttons::AdcButtonConfig()
                                 .channel_idx(kChannel)
                                 .release_threshold(kReleaseThreshold)
                                 .press_threshold(kPressThreshold);
      auto func_config = fuchsia_buttons::ButtonConfig::WithAdc(std::move(func_adc_config));
      auto func_button = fuchsia_buttons::Button()
                             .types(std::move(func_types))
                             .button_config(std::move(func_config));
      std::vector<fuchsia_buttons::Button> buttons;
      buttons.emplace_back(std::move(func_button));

      auto metadata = fuchsia_buttons::Metadata()
                          .polling_rate_usec(kPollingRateUsec)
                          .buttons(std::move(buttons));

      fit::result metadata_bytes = fidl::Persist(metadata);
      ASSERT_TRUE(metadata_bytes.is_ok());
      auto status = incoming->device_server_.AddMetadata(
          DEVICE_METADATA_BUTTONS, metadata_bytes->data(), metadata_bytes->size());
      EXPECT_OK(status);
      status = incoming->device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                              &incoming->env_.incoming_directory());
      EXPECT_OK(status);

      // Serve fake_adc_server_.
      auto result = incoming->env_.incoming_directory().AddService<fuchsia_hardware_adc::Service>(
          std::move(incoming->fake_adc_server_.GetInstanceHandler()), "adc-2");
      ASSERT_TRUE(result.is_ok());
    });

    // Start dut_.
    auto result = runtime_.RunToCompletion(dut_.SyncCall(
        &fdf_testing::DriverUnderTest<adc_buttons::AdcButtons>::Start, std::move(start_args)));
    ASSERT_TRUE(result.is_ok());

    // Connect to InputDevice.
    zx::result connect_result = incoming_.SyncCall([](IncomingNamespace* incoming) {
      return incoming->node_.children().at("adc-buttons").ConnectToDevice();
    });
    ASSERT_OK(connect_result.status_value());
    client_.Bind(
        fidl::ClientEnd<fuchsia_input_report::InputDevice>(std::move(connect_result.value())));
  }

  void DrainInitialReport(fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>& reader) {
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
  }

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();

  static constexpr uint32_t kChannel = 2;
  static constexpr uint32_t kReleaseThreshold = 30;
  static constexpr uint32_t kPressThreshold = 10;

 protected:
  static constexpr uint32_t kPollingRateUsec = 1'000;

  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{
      env_dispatcher_->async_dispatcher(), std::in_place};
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<adc_buttons::AdcButtons>> dut_{
      driver_dispatcher_->async_dispatcher(), std::in_place};

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
  EXPECT_EQ(result->descriptor.device_info().polling_rate, kPollingRateUsec);

  ASSERT_TRUE(result->descriptor.has_consumer_control());
  ASSERT_TRUE(result->descriptor.consumer_control().has_input());
  ASSERT_TRUE(result->descriptor.consumer_control().input().has_buttons());
  EXPECT_EQ(result->descriptor.consumer_control().input().buttons().count(), 1);
  EXPECT_EQ(result->descriptor.consumer_control().input().buttons()[0],
            fuchsia_input_report::wire::ConsumerControlButton::kFunction);
}

TEST_F(AdcButtonsDeviceTest, ReadInputReportsTest) {
  auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();
  auto result = client_->GetInputReportsReader(std::move(endpoints.server));
  ASSERT_TRUE(result.ok());
  // Ensure that the reader has been registered with the client before moving on.
  ASSERT_TRUE(client_->GetDescriptor().ok());
  auto reader =
      fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(endpoints.client));
  EXPECT_TRUE(reader.is_valid());
  DrainInitialReport(reader);

  incoming_.SyncCall(
      [](IncomingNamespace* incoming) { incoming->fake_adc_server_.set_sample(20); });
  // Wait for the device to pick this up.
  usleep(2 * kPollingRateUsec);

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

  incoming_.SyncCall(
      [](IncomingNamespace* incoming) { incoming->fake_adc_server_.set_sample(40); });
  // Wait for the device to pick this up.
  usleep(2 * kPollingRateUsec);

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
