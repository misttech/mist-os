// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/input/drivers/goldfish_sensor/input_device.h"

#include <fidl/fuchsia.hardware.goldfish.pipe/cpp/wire.h>
#include <fidl/fuchsia.hardware.goldfish/cpp/wire.h>
#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>
#include <lib/ddk/driver.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fidl/cpp/wire/wire_messaging.h>
#include <lib/zx/time.h>

#include <cmath>

#include <gtest/gtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace goldfish::sensor {

template <class DutType>
class InputDeviceTest : public ::testing::TestWithParam<bool> {
 public:
  void SetUp() override {
    auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(), [&]() {
      auto device = std::make_unique<DutType>(fake_parent_.get(), dispatcher_->async_dispatcher());
      ASSERT_EQ(device->DdkAdd("goldfish-sensor-input"), ZX_OK);
      // dut_ will be deleted by MockDevice when the test ends.
      dut_ = device.release();
    });
    EXPECT_EQ(result.status_value(), ZX_OK);

    ASSERT_EQ(fake_parent_->child_count(), 1u);

    auto endpoints = fidl::CreateEndpoints<fuchsia_input_report::InputDevice>();
    ASSERT_TRUE(endpoints.is_ok());

    binding_ = fidl::BindServer(dispatcher_->async_dispatcher(), std::move(endpoints->server),
                                fake_parent_->GetLatestChild()->GetDeviceContext<InputDevice>());

    device_client_.Bind(std::move(endpoints->client));
  }

  void TearDown() override {
    auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(), [&]() {
      device_async_remove(dut_->zxdev());
      mock_ddk::ReleaseFlaggedDevices(fake_parent_.get());
    });
    EXPECT_EQ(result.status_value(), ZX_OK);
  }

  DutType* dut() const { return static_cast<DutType*>(dut_); }

  bool HasMeasurementId() const { return GetParam(); }

 protected:
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  fdf::UnownedSynchronizedDispatcher dispatcher_ =
      fdf_testing::DriverRuntime::GetInstance()->StartBackgroundDispatcher();
  InputDevice* dut_;
  fidl::WireSyncClient<fuchsia_input_report::InputDevice> device_client_;
  std::optional<fidl::ServerBindingRef<fuchsia_input_report::InputDevice>> binding_;
};

class TestAccelerationInputDevice : public AccelerationInputDevice {
 public:
  explicit TestAccelerationInputDevice(zx_device_t* parent, async_dispatcher_t* dispatcher)
      : AccelerationInputDevice(parent, dispatcher, nullptr) {}

  ~TestAccelerationInputDevice() override = default;

  void GetInputReportsReader(GetInputReportsReaderRequestView request,
                             GetInputReportsReaderCompleter::Sync& completer) override {
    AccelerationInputDevice::GetInputReportsReader(request, completer);
  }
};

using AccelerationInputDeviceTest = InputDeviceTest<TestAccelerationInputDevice>;

TEST_P(AccelerationInputDeviceTest, ReadInputReports) {
  auto endpoint_result = fidl::CreateEndpoints<fuchsia_input_report::InputReportsReader>();
  ASSERT_TRUE(endpoint_result.is_ok());
  auto [client_end, server_end] = std::move(endpoint_result.value());

  auto reader_result = device_client_->GetInputReportsReader(std::move(server_end));
  ASSERT_TRUE(reader_result.ok());
  auto reader_client =
      fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(client_end));

  // The FIDL callback runs on another thread.
  // We will need to wait for the FIDL callback to finish before using |client|.
  auto descriptor = device_client_->GetDescriptor();
  ASSERT_TRUE(descriptor.ok());

  SensorReport rpt = {.name = "acceleration", .data = {Numeric(1.0), Numeric(2.0), Numeric(3.0)}};
  if (HasMeasurementId()) {
    rpt.data.push_back(Numeric(100L));
  }
  EXPECT_EQ(dut_->OnReport(rpt), ZX_OK);

  auto result = reader_client->ReadInputReports();
  ASSERT_TRUE(result.ok());
  auto* response = result.Unwrap();
  ASSERT_TRUE(response->is_ok());
  auto& reports = response->value()->reports;
  ASSERT_EQ(reports.count(), 1u);
  auto& report = response->value()->reports[0];
  ASSERT_TRUE(report.has_sensor());
  auto& sensor = response->value()->reports[0].sensor();
  ASSERT_TRUE(sensor.has_values() && sensor.values().count() == 3);
  EXPECT_EQ(sensor.values().at(0), 100);
  EXPECT_EQ(sensor.values().at(1), 200);
  EXPECT_EQ(sensor.values().at(2), 300);
}

TEST_F(AccelerationInputDeviceTest, Descriptor) {
  auto result = device_client_->GetDescriptor();
  ASSERT_TRUE(result.ok());
  auto* response = result.Unwrap();
  ASSERT_NE(response, nullptr);
  const auto& descriptor = response->descriptor;

  ASSERT_TRUE(descriptor.has_sensor());
  EXPECT_FALSE(descriptor.has_keyboard());
  EXPECT_FALSE(descriptor.has_mouse());
  EXPECT_FALSE(descriptor.has_touch());
  EXPECT_FALSE(descriptor.has_consumer_control());

  ASSERT_TRUE(descriptor.sensor().has_input());
  ASSERT_EQ(descriptor.sensor().input().count(), 1UL);
  ASSERT_TRUE(descriptor.sensor().input()[0].has_values());
  const auto& values = descriptor.sensor().input()[0].values();

  ASSERT_EQ(values.count(), 3u);
  EXPECT_EQ(values[0].type, fuchsia_input_report::wire::SensorType::kAccelerometerX);
  EXPECT_EQ(values[1].type, fuchsia_input_report::wire::SensorType::kAccelerometerY);
  EXPECT_EQ(values[2].type, fuchsia_input_report::wire::SensorType::kAccelerometerZ);

  EXPECT_EQ(values[0].axis.unit.type, fuchsia_input_report::wire::UnitType::kSiLinearAcceleration);
  EXPECT_EQ(values[1].axis.unit.type, fuchsia_input_report::wire::UnitType::kSiLinearAcceleration);
  EXPECT_EQ(values[2].axis.unit.type, fuchsia_input_report::wire::UnitType::kSiLinearAcceleration);

  EXPECT_EQ(values[0].axis.unit.exponent, -2);
  EXPECT_EQ(values[1].axis.unit.exponent, -2);
  EXPECT_EQ(values[2].axis.unit.exponent, -2);
}

TEST_P(AccelerationInputDeviceTest, InvalidInputReports) {
  const int64_t kMeasurementId = 100L;

  // Invalid number of elements.
  SensorReport invalid_report1 = {.name = "acceleration", .data = {Numeric(1.0), Numeric(2.0)}};
  EXPECT_EQ(dut_->OnReport(invalid_report1), ZX_ERR_INVALID_ARGS);

  // Invalid x.
  SensorReport invalid_report2 = {.name = "acceleration",
                                  .data = {"string", Numeric(2.0), Numeric(3.0)}};
  if (HasMeasurementId()) {
    invalid_report2.data.push_back(Numeric(kMeasurementId));
  }
  EXPECT_EQ(dut_->OnReport(invalid_report2), ZX_ERR_INVALID_ARGS);

  // Invalid y.
  SensorReport invalid_report3 = {.name = "acceleration",
                                  .data = {Numeric(2.0), "string", Numeric(3.0)}};
  if (HasMeasurementId()) {
    invalid_report3.data.push_back(Numeric(kMeasurementId));
  }
  EXPECT_EQ(dut_->OnReport(invalid_report3), ZX_ERR_INVALID_ARGS);

  // Invalid z.
  SensorReport invalid_report4 = {.name = "acceleration",
                                  .data = {Numeric(2.0), Numeric(3.0), "string"}};
  if (HasMeasurementId()) {
    invalid_report4.data.push_back(Numeric(kMeasurementId));
  }
  EXPECT_EQ(dut_->OnReport(invalid_report4), ZX_ERR_INVALID_ARGS);
}

INSTANTIATE_TEST_SUITE_P(HasMeasurementId, AccelerationInputDeviceTest, testing::Bool());

class TestGyroscopeInputDevice : public GyroscopeInputDevice {
 public:
  explicit TestGyroscopeInputDevice(zx_device_t* parent, async_dispatcher_t* dispatcher)
      : GyroscopeInputDevice(parent, dispatcher, nullptr) {}

  void GetInputReportsReader(GetInputReportsReaderRequestView request,
                             GetInputReportsReaderCompleter::Sync& completer) override {
    GyroscopeInputDevice::GetInputReportsReader(request, completer);
  }
};

using GyroscopeInputDeviceTest = InputDeviceTest<TestGyroscopeInputDevice>;

TEST_P(GyroscopeInputDeviceTest, ReadInputReports) {
  auto endpoint_result = fidl::CreateEndpoints<fuchsia_input_report::InputReportsReader>();
  ASSERT_TRUE(endpoint_result.is_ok());
  auto [client_end, server_end] = std::move(endpoint_result.value());

  auto reader_result = device_client_->GetInputReportsReader(std::move(server_end));
  ASSERT_TRUE(reader_result.ok());
  auto reader_client =
      fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(client_end));

  // The FIDL callback runs on another thread.
  // We will need to wait for the FIDL callback to finish before using |client|.
  auto descriptor = device_client_->GetDescriptor();
  ASSERT_TRUE(descriptor.ok());

  SensorReport rpt = {.name = "gyroscope",
                      .data = {Numeric(M_PI), Numeric(2.0 * M_PI), Numeric(3.0 * M_PI)}};
  if (HasMeasurementId()) {
    rpt.data.push_back(Numeric(100L));
  }

  EXPECT_EQ(dut_->OnReport(rpt), ZX_OK);

  auto result = reader_client->ReadInputReports();
  ASSERT_TRUE(result.ok());
  auto* response = result.Unwrap();
  ASSERT_TRUE(response->is_ok());
  auto& reports = response->value()->reports;
  ASSERT_EQ(reports.count(), 1u);
  auto& report = response->value()->reports[0];
  ASSERT_TRUE(report.has_sensor());
  auto& sensor = response->value()->reports[0].sensor();
  ASSERT_TRUE(sensor.has_values() && sensor.values().count() == 3);
  EXPECT_EQ(sensor.values().at(0), 18000);
  EXPECT_EQ(sensor.values().at(1), 36000);
  EXPECT_EQ(sensor.values().at(2), 54000);
}

TEST_F(GyroscopeInputDeviceTest, Descriptor) {
  auto result = device_client_->GetDescriptor();
  ASSERT_TRUE(result.ok());
  auto* response = result.Unwrap();

  ASSERT_NE(response, nullptr);
  const auto& descriptor = response->descriptor;

  ASSERT_TRUE(descriptor.has_sensor());
  EXPECT_FALSE(descriptor.has_keyboard());
  EXPECT_FALSE(descriptor.has_mouse());
  EXPECT_FALSE(descriptor.has_touch());
  EXPECT_FALSE(descriptor.has_consumer_control());

  ASSERT_TRUE(descriptor.sensor().has_input());
  ASSERT_EQ(descriptor.sensor().input().count(), 1UL);
  ASSERT_TRUE(descriptor.sensor().input()[0].has_values());
  const auto& values = descriptor.sensor().input()[0].values();

  ASSERT_EQ(values.count(), 3u);
  EXPECT_EQ(values[0].type, fuchsia_input_report::wire::SensorType::kGyroscopeX);
  EXPECT_EQ(values[1].type, fuchsia_input_report::wire::SensorType::kGyroscopeY);
  EXPECT_EQ(values[2].type, fuchsia_input_report::wire::SensorType::kGyroscopeZ);

  EXPECT_EQ(values[0].axis.unit.type,
            fuchsia_input_report::wire::UnitType::kEnglishAngularVelocity);
  EXPECT_EQ(values[1].axis.unit.type,
            fuchsia_input_report::wire::UnitType::kEnglishAngularVelocity);
  EXPECT_EQ(values[2].axis.unit.type,
            fuchsia_input_report::wire::UnitType::kEnglishAngularVelocity);

  EXPECT_EQ(values[0].axis.unit.exponent, -2);
  EXPECT_EQ(values[1].axis.unit.exponent, -2);
  EXPECT_EQ(values[2].axis.unit.exponent, -2);
}

TEST_P(GyroscopeInputDeviceTest, InvalidInputReports) {
  const int64_t kMeasurementId = 100L;

  // Invalid number of elements.
  SensorReport invalid_report1 = {.name = "gyroscope", .data = {Numeric(1.0), Numeric(2.0)}};
  EXPECT_EQ(dut_->OnReport(invalid_report1), ZX_ERR_INVALID_ARGS);

  // Invalid x.
  SensorReport invalid_report2 = {.name = "gyroscope",
                                  .data = {"string", Numeric(2.0), Numeric(3.0)}};
  if (HasMeasurementId()) {
    invalid_report2.data.push_back(Numeric(kMeasurementId));
  }
  EXPECT_EQ(dut_->OnReport(invalid_report2), ZX_ERR_INVALID_ARGS);

  // Invalid y.
  SensorReport invalid_report3 = {.name = "gyroscope",
                                  .data = {Numeric(2.0), "string", Numeric(3.0)}};
  if (HasMeasurementId()) {
    invalid_report3.data.push_back(Numeric(kMeasurementId));
  }
  EXPECT_EQ(dut_->OnReport(invalid_report3), ZX_ERR_INVALID_ARGS);

  // Invalid z.
  SensorReport invalid_report4 = {.name = "gyroscope",
                                  .data = {Numeric(2.0), Numeric(3.0), "string"}};
  if (HasMeasurementId()) {
    invalid_report4.data.push_back(Numeric(kMeasurementId));
  }
  EXPECT_EQ(dut_->OnReport(invalid_report4), ZX_ERR_INVALID_ARGS);
}

INSTANTIATE_TEST_SUITE_P(HasMeasurementId, GyroscopeInputDeviceTest, testing::Bool());

class TestRgbcLightInputDevice : public RgbcLightInputDevice {
 public:
  explicit TestRgbcLightInputDevice(zx_device_t* parent, async_dispatcher_t* dispatcher)
      : RgbcLightInputDevice(parent, dispatcher, nullptr) {}

  void GetInputReportsReader(GetInputReportsReaderRequestView request,
                             GetInputReportsReaderCompleter::Sync& completer) override {
    RgbcLightInputDevice::GetInputReportsReader(request, completer);
  }
};

using RgbcLightInputDeviceTest = InputDeviceTest<TestRgbcLightInputDevice>;

TEST_P(RgbcLightInputDeviceTest, ReadInputReports) {
  auto endpoint_result = fidl::CreateEndpoints<fuchsia_input_report::InputReportsReader>();
  ASSERT_TRUE(endpoint_result.is_ok());
  auto [client_end, server_end] = std::move(endpoint_result.value());

  auto reader_result = device_client_->GetInputReportsReader(std::move(server_end));
  ASSERT_TRUE(reader_result.ok());
  auto reader_client =
      fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(client_end));

  // The FIDL callback runs on another thread.
  // We will need to wait for the FIDL callback to finish before using |client|.
  auto descriptor = device_client_->GetDescriptor();
  ASSERT_TRUE(descriptor.ok());

  SensorReport rpt = {.name = "rgbclight",
                      .data = {Numeric(100L), Numeric(200L), Numeric(300L), Numeric(400L)}};
  if (HasMeasurementId()) {
    rpt.data.push_back(Numeric(100L));
  }
  EXPECT_EQ(dut_->OnReport(rpt), ZX_OK);

  auto result = reader_client->ReadInputReports();
  ASSERT_TRUE(result.ok());
  auto* response = result.Unwrap();
  ASSERT_TRUE(response->is_ok());
  auto& reports = response->value()->reports;
  ASSERT_EQ(reports.count(), 1u);
  auto& report = response->value()->reports[0];
  ASSERT_TRUE(report.has_sensor());
  auto& sensor = response->value()->reports[0].sensor();
  ASSERT_TRUE(sensor.has_values() && sensor.values().count() == 4);
  EXPECT_EQ(sensor.values().at(0), 100L);
  EXPECT_EQ(sensor.values().at(1), 200L);
  EXPECT_EQ(sensor.values().at(2), 300L);
  EXPECT_EQ(sensor.values().at(3), 400L);
}

TEST_F(RgbcLightInputDeviceTest, Descriptor) {
  auto result = device_client_->GetDescriptor();
  ASSERT_TRUE(result.ok());
  auto* response = result.Unwrap();
  ASSERT_NE(response, nullptr);
  const auto& descriptor = response->descriptor;

  ASSERT_TRUE(descriptor.has_sensor());
  EXPECT_FALSE(descriptor.has_keyboard());
  EXPECT_FALSE(descriptor.has_mouse());
  EXPECT_FALSE(descriptor.has_touch());
  EXPECT_FALSE(descriptor.has_consumer_control());

  ASSERT_TRUE(descriptor.sensor().has_input());
  ASSERT_EQ(descriptor.sensor().input().count(), 1UL);
  ASSERT_TRUE(descriptor.sensor().input()[0].has_values());
  const auto& values = descriptor.sensor().input()[0].values();

  ASSERT_EQ(values.count(), 4u);
  EXPECT_EQ(values[0].type, fuchsia_input_report::wire::SensorType::kLightRed);
  EXPECT_EQ(values[1].type, fuchsia_input_report::wire::SensorType::kLightGreen);
  EXPECT_EQ(values[2].type, fuchsia_input_report::wire::SensorType::kLightBlue);
  EXPECT_EQ(values[3].type, fuchsia_input_report::wire::SensorType::kLightIlluminance);

  EXPECT_EQ(values[0].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);
  EXPECT_EQ(values[1].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);
  EXPECT_EQ(values[2].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);
  EXPECT_EQ(values[3].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);
}

TEST_P(RgbcLightInputDeviceTest, InvalidInputReports) {
  const int64_t kMeasurementId = 100L;

  // Invalid number of elements.
  SensorReport invalid_report1 = {.name = "rgbc-light",
                                  .data = {Numeric(1.0), Numeric(2.0), Numeric(3.0)}};
  EXPECT_EQ(dut_->OnReport(invalid_report1), ZX_ERR_INVALID_ARGS);

  // Invalid r.
  SensorReport invalid_report2 = {.name = "rgbc-light",
                                  .data = {"string", Numeric(100L), Numeric(200L), Numeric(300L)}};
  if (HasMeasurementId()) {
    invalid_report2.data.push_back(Numeric(kMeasurementId));
  }
  EXPECT_EQ(dut_->OnReport(invalid_report2), ZX_ERR_INVALID_ARGS);

  // Invalid g.
  SensorReport invalid_report3 = {.name = "rgbc-light",
                                  .data = {Numeric(100L), "string", Numeric(200L), Numeric(300L)}};
  if (HasMeasurementId()) {
    invalid_report3.data.push_back(Numeric(kMeasurementId));
  }
  EXPECT_EQ(dut_->OnReport(invalid_report3), ZX_ERR_INVALID_ARGS);

  // Invalid b.
  SensorReport invalid_report4 = {.name = "rgbc-light",
                                  .data = {Numeric(100L), Numeric(200L), "string", Numeric(300L)}};
  if (HasMeasurementId()) {
    invalid_report4.data.push_back(Numeric(kMeasurementId));
  }
  EXPECT_EQ(dut_->OnReport(invalid_report4), ZX_ERR_INVALID_ARGS);

  // Invalid a.
  SensorReport invalid_report5 = {.name = "rgbc-light",
                                  .data = {Numeric(100L), Numeric(200L), Numeric(300L), "string"}};
  if (HasMeasurementId()) {
    invalid_report5.data.push_back(Numeric(kMeasurementId));
  }
  EXPECT_EQ(dut_->OnReport(invalid_report5), ZX_ERR_INVALID_ARGS);
}

INSTANTIATE_TEST_SUITE_P(HasMeasurementId, RgbcLightInputDeviceTest, testing::Bool());

}  // namespace goldfish::sensor
