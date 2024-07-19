// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.input/cpp/wire_test_base.h>
#include <lib/driver/testing/cpp/fixture/driver_test_fixture.h>
#include <lib/hid/acer12.h>
#include <lib/hid/ambient-light.h>
#include <lib/hid/boot.h>
#include <lib/hid/buttons.h>
#include <lib/hid/gt92xx.h>
#include <lib/hid/paradise.h>
#include <lib/hid/usages.h>

#include <ddk/metadata/buttons.h>
#include <gtest/gtest.h>
#include <sdk/lib/inspect/testing/cpp/inspect.h>
#include <src/lib/testing/predicates/status.h>

#include "src/ui/input/drivers/hid-input-report/driver.h"

namespace hid_input_report_dev {

namespace fhidbus = fuchsia_hardware_hidbus;
namespace finput = fuchsia_hardware_input;

namespace {

template <typename T>
std::vector<uint8_t> ToBinaryVector(T* data, size_t size) {
  const uint8_t* begin = reinterpret_cast<const uint8_t*>(data);
  return std::vector(begin, begin + size);
}

template <typename T, typename = std::enable_if_t<!std::is_pointer_v<T>>>
std::vector<uint8_t> ToBinaryVector(const T& t) {
  const uint8_t* begin = reinterpret_cast<const uint8_t*>(&t);
  return ToBinaryVector(begin, sizeof(T));
}

}  // namespace

class FakeHidDevice : public fidl::WireServer<finput::Controller>,
                      public fidl::testing::WireTestBase<finput::Device> {
 public:
  class DeviceReportsReader : public fidl::WireServer<finput::DeviceReportsReader> {
   public:
    explicit DeviceReportsReader(fidl::ServerEnd<finput::DeviceReportsReader> server,
                                 libsync::Completion& unbound)
        : binding_(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server), this,
                   [&unbound](fidl::UnbindInfo) { unbound.Signal(); }) {}

    ~DeviceReportsReader() override { binding_.Close(ZX_ERR_PEER_CLOSED); }

    void ReadReports(ReadReportsCompleter::Sync& completer) override {
      ASSERT_FALSE(waiting_read_.has_value());
      waiting_read_.emplace(completer.ToAsync());
      wait_for_read_.Signal();
    }

    void SendReport(std::vector<uint8_t> report, zx::time timestamp) {
      fidl::Arena arena;
      std::vector<fhidbus::wire::Report> reports = {
          fhidbus::wire::Report::Builder(arena)
              .timestamp(timestamp.get())
              .buf(fidl::VectorView<uint8_t>::FromExternal(report.data(), report.size()))
              .Build()};
      waiting_read_->ReplySuccess(
          fidl::VectorView<fhidbus::wire::Report>::FromExternal(reports.data(), reports.size()));
      waiting_read_.reset();
    }

    libsync::Completion wait_for_read_;

   private:
    fidl::ServerBinding<finput::DeviceReportsReader> binding_;
    std::optional<ReadReportsCompleter::Async> waiting_read_;
  };

  static constexpr uint32_t kVendorId = 0xabc;
  static constexpr uint32_t kProductId = 123;
  static constexpr uint32_t kVersion = 5;

  zx_status_t Serve(fdf::OutgoingDirectory& to_driver_vfs) {
    finput::Service::InstanceHandler instance_handler({
        .controller =
            [this](fidl::ServerEnd<finput::Controller> server) {
              EXPECT_FALSE(binding_);
              binding_.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server),
                               this, fidl::kIgnoreBindingClosure);
            },
    });
    return to_driver_vfs.AddService<finput::Service>(std::move(instance_handler)).status_value();
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    ASSERT_TRUE(false);
  }

  void Query(QueryCompleter::Sync& completer) override {
    fidl::Arena arena;
    completer.ReplySuccess(fhidbus::wire::HidInfo::Builder(arena)
                               .vendor_id(kVendorId)
                               .product_id(kProductId)
                               .version(kVersion)
                               .Build());
  }

  void OpenSession(OpenSessionRequestView request, OpenSessionCompleter::Sync& completer) override {
    EXPECT_FALSE(device_binding_);
    device_binding_.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                            std::move(request->session), this, fidl::kIgnoreBindingClosure);
  }

  void GetDeviceReportsReader(GetDeviceReportsReaderRequestView request,
                              GetDeviceReportsReaderCompleter::Sync& completer) override {
    ASSERT_FALSE(reader_);
    reader_ = std::make_unique<DeviceReportsReader>(std::move(request->reader), unbound_);
    completer.ReplySuccess();
  }

  void GetReportDesc(GetReportDescCompleter::Sync& completer) override {
    completer.Reply(
        fidl::VectorView<uint8_t>::FromExternal(report_desc_.data(), report_desc_.size()));
  }

  void GetReport(GetReportRequestView request, GetReportCompleter::Sync& completer) override {
    // If the client is Getting a report with a specific ID, check that it matches
    // our saved report.
    if ((request->id != 0) && (report_.size() > 0)) {
      if (request->id != report_[0]) {
        completer.ReplyError(ZX_ERR_WRONG_TYPE);
        return;
      }
    }

    completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(report_.data(), report_.size()));
  }
  std::vector<uint8_t>& GetReport() { return report_; }

  void SetReport(SetReportRequestView request, SetReportCompleter::Sync& completer) override {
    report_ = ToBinaryVector(request->report.data(), request->report.count());
    completer.ReplySuccess();
  }
  void SetReport(std::vector<uint8_t> report) { report_ = std::move(report); }

  void SetReportDesc(std::vector<uint8_t> report_desc) { report_desc_ = std::move(report_desc); }

  void SendReport(std::vector<uint8_t> report, zx::time timestamp) const {
    ASSERT_TRUE(reader_);
    reader_->SendReport(std::move(report), timestamp);
  }

  std::unique_ptr<DeviceReportsReader> reader_;
  libsync::Completion unbound_;

 private:
  std::optional<fidl::ServerBinding<finput::Controller>> binding_;
  std::optional<fidl::ServerBinding<finput::Device>> device_binding_;

  std::vector<uint8_t> report_desc_;
  std::vector<uint8_t> report_;
};

class InputReportTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    EXPECT_OK(fake_hid_.Serve(to_driver_vfs));
    return zx::ok();
  }

  FakeHidDevice& fake_hid() { return fake_hid_; }

 private:
  FakeHidDevice fake_hid_;
};

class FixtureConfig final {
 public:
  using DriverType = InputReportDriver;
  using EnvironmentType = InputReportTestEnvironment;
};

class HidDevTest : public fdf_testing::BackgroundDriverTestFixture<FixtureConfig>,
                   public ::testing::Test {
 public:
  void TearDown() override {
    zx::result<> result = StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  fidl::WireSyncClient<fuchsia_input_report::InputDevice> GetSyncClient() {
    auto connect_result =
        RunInNodeContext<zx::result<zx::channel>>([](fdf_testing::TestNode& node) {
          return node.children().at("InputReport").ConnectToDevice();
        });
    EXPECT_TRUE(connect_result.is_ok());
    return fidl::WireSyncClient(
        fidl::ClientEnd<fuchsia_input_report::InputDevice>(std::move(connect_result.value())));
  }

  fidl::ClientEnd<fuchsia_input_report::InputReportsReader> GetReader() {
    return GetReader(GetSyncClient());
  }
  fidl::ClientEnd<fuchsia_input_report::InputReportsReader> GetReader(
      const fidl::WireSyncClient<fuchsia_input_report::InputDevice>& sync_client) {
    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();
    auto result = sync_client->GetInputReportsReader(std::move(endpoints.server));
    EXPECT_OK(result.status());
    sync_completion_t* next_reader_wait;
    RunInDriverContext([&next_reader_wait](InputReportDriver& driver) {
      next_reader_wait = &driver.input_report_for_testing().next_reader_wait();
    });
    EXPECT_OK(sync_completion_wait(next_reader_wait, ZX_TIME_INFINITE));
    sync_completion_reset(next_reader_wait);
    return std::move(endpoints.client);
  }

  void SendReport(std::vector<uint8_t> report, zx::time timestamp = zx::time::infinite()) {
    if (timestamp == zx::time::infinite()) {
      timestamp = zx::clock::get_monotonic();
    }

    libsync::Completion* wait_for_read;
    RunInEnvironmentTypeContext([&wait_for_read](InputReportTestEnvironment& env) {
      ASSERT_TRUE(env.fake_hid().reader_);
      wait_for_read = &env.fake_hid().reader_->wait_for_read_;
    });
    wait_for_read->Wait();
    wait_for_read->Reset();
    RunInEnvironmentTypeContext([&report, &timestamp](InputReportTestEnvironment& env) {
      ASSERT_TRUE(env.fake_hid().reader_);
      env.fake_hid().reader_->SendReport(std::move(report), timestamp);
    });
    wait_for_read->Wait();
  }
};

TEST_F(HidDevTest, HidLifetimeTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t boot_mouse_desc_size;
    const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
    env.fake_hid().SetReportDesc(ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());
}

class UnregisterHidDevTest : public fdf_testing::ForegroundDriverTestFixture<FixtureConfig>,
                             public ::testing::Test {};

TEST_F(UnregisterHidDevTest, InputReportUnregisterTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t boot_mouse_desc_size;
    const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
    env.fake_hid().SetReportDesc(ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  zx::result prepare_stop_result = StopDriver();
  EXPECT_EQ(ZX_OK, prepare_stop_result.status_value());

  libsync::Completion* unbound;
  RunInEnvironmentTypeContext(
      [&unbound](InputReportTestEnvironment& env) { unbound = &env.fake_hid().unbound_; });
  unbound->Wait();
}

TEST_F(HidDevTest, InputReportUnregisterTestBindFailed) {
  // We don't set a input report on `fake_hid` so the bind will fail.
  ASSERT_EQ(StartDriver().status_value(), ZX_ERR_INTERNAL);

  // Make sure that the InputReport class is not registered to the HID device.
  RunInEnvironmentTypeContext(
      [](InputReportTestEnvironment& env) { ASSERT_FALSE(env.fake_hid().reader_); });
}

TEST_F(HidDevTest, GetReportDescTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t boot_mouse_desc_size;
    const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
    env.fake_hid().SetReportDesc(ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  auto sync_client = GetSyncClient();
  fidl::WireResult<fuchsia_input_report::InputDevice::GetDescriptor> result =
      sync_client->GetDescriptor();
  ASSERT_OK(result.status());

  auto& desc = result->descriptor;
  ASSERT_TRUE(desc.has_mouse());
  ASSERT_TRUE(desc.mouse().has_input());

  fuchsia_input_report::wire::MouseInputDescriptor& mouse = desc.mouse().input();
  ASSERT_TRUE(mouse.has_movement_x());
  ASSERT_EQ(-127, mouse.movement_x().range.min);
  ASSERT_EQ(127, mouse.movement_x().range.max);

  ASSERT_TRUE(mouse.has_movement_y());
  ASSERT_EQ(-127, mouse.movement_y().range.min);
  ASSERT_EQ(127, mouse.movement_y().range.max);
}

TEST_F(HidDevTest, ReportDescInfoTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t boot_mouse_desc_size;
    const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
    env.fake_hid().SetReportDesc(ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  auto sync_client = GetSyncClient();
  fidl::WireResult<fuchsia_input_report::InputDevice::GetDescriptor> result =
      sync_client->GetDescriptor();
  ASSERT_OK(result.status());

  auto& desc = result.value().descriptor;
  ASSERT_TRUE(desc.has_device_information());
  ASSERT_EQ(desc.device_information().vendor_id(), FakeHidDevice::kVendorId);
  ASSERT_EQ(desc.device_information().product_id(), FakeHidDevice::kProductId);
  ASSERT_EQ(desc.device_information().version(), FakeHidDevice::kVersion);
}

TEST_F(HidDevTest, ReadInputReportsTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t boot_mouse_desc_size;
    const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
    env.fake_hid().SetReportDesc(ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  // GetReader() must be called before SendReport() because SendReport() only sends reports to
  // existing readers.
  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader(GetReader());
  SendReport(std::vector<uint8_t>{0xFF, 0x50, 0x70});

  auto result = reader->ReadInputReports();
  ASSERT_OK(result.status());
  ASSERT_FALSE(result->is_error());
  auto& reports = result->value()->reports;

  ASSERT_EQ(1UL, reports.count());

  auto& report = reports[0];
  ASSERT_TRUE(report.has_event_time());
  ASSERT_TRUE(report.has_mouse());
  auto& mouse = report.mouse();

  ASSERT_TRUE(mouse.has_movement_x());
  ASSERT_EQ(0x50, mouse.movement_x());

  ASSERT_TRUE(mouse.has_movement_y());
  ASSERT_EQ(0x70, mouse.movement_y());

  ASSERT_TRUE(mouse.has_pressed_buttons());
  const fidl::VectorView<uint8_t>& pressed_buttons = mouse.pressed_buttons();
  for (size_t i = 0; i < pressed_buttons.count(); i++) {
    ASSERT_EQ(i + 1, pressed_buttons[i]);
  }
}

TEST_F(HidDevTest, ReadInputReportsHangingGetTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t boot_mouse_desc_size;
    const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
    env.fake_hid().SetReportDesc(ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  fidl::WireClient<fuchsia_input_report::InputReportsReader> reader(
      GetReader(), fdf::Dispatcher::GetCurrent()->async_dispatcher());
  // Read the report. This will hang until a report is sent.
  reader->ReadInputReports().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_input_report::InputReportsReader::ReadInputReports>&
              response) {
        ASSERT_OK(response.status());
        ASSERT_FALSE(response->is_error());
        auto& reports = response->value()->reports;
        ASSERT_EQ(1UL, reports.count());

        auto& report = reports[0];
        ASSERT_TRUE(report.has_event_time());
        ASSERT_TRUE(report.has_mouse());
        auto& mouse = report.mouse();

        ASSERT_TRUE(mouse.has_movement_x());
        ASSERT_EQ(0x50, mouse.movement_x());

        ASSERT_TRUE(mouse.has_movement_y());
        ASSERT_EQ(0x70, mouse.movement_y());
        runtime().Quit();
      });
  runtime().RunUntilIdle();

  // Send the report.
  SendReport(std::vector<uint8_t>{0xFF, 0x50, 0x70});

  runtime().Run();
}

TEST_F(HidDevTest, CloseReaderWithOutstandingRead) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t boot_mouse_desc_size;
    const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
    env.fake_hid().SetReportDesc(ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  fidl::WireClient<fuchsia_input_report::InputReportsReader> reader(
      GetReader(), fdf::Dispatcher::GetCurrent()->async_dispatcher());

  // Queue a report.
  reader->ReadInputReports().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_input_report::InputReportsReader::ReadInputReports>&
              result) { ASSERT_TRUE(result.is_canceled()); });
  runtime().RunUntilIdle();

  // Unbind the reader now that the report is waiting.
  reader = {};
}

TEST_F(HidDevTest, SensorTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    const uint8_t* sensor_desc_ptr;
    size_t sensor_desc_size = get_ambient_light_report_desc(&sensor_desc_ptr);
    env.fake_hid().SetReportDesc(ToBinaryVector(sensor_desc_ptr, sensor_desc_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  auto sync_client = GetSyncClient();

  // Get the report descriptor.
  fidl::WireResult<fuchsia_input_report::InputDevice::GetDescriptor> result =
      sync_client->GetDescriptor();
  ASSERT_OK(result.status());
  fuchsia_input_report::wire::DeviceDescriptor& desc = result.value().descriptor;
  ASSERT_TRUE(desc.has_sensor());
  ASSERT_TRUE(desc.sensor().has_input());
  ASSERT_EQ(desc.sensor().input().count(), 1UL);
  fuchsia_input_report::wire::SensorInputDescriptor& sensor_desc = desc.sensor().input()[0];
  ASSERT_TRUE(sensor_desc.has_values());
  ASSERT_EQ(4UL, sensor_desc.values().count());

  ASSERT_EQ(sensor_desc.values()[0].type,
            fuchsia_input_report::wire::SensorType::kLightIlluminance);
  ASSERT_EQ(sensor_desc.values()[0].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);

  ASSERT_EQ(sensor_desc.values()[1].type, fuchsia_input_report::wire::SensorType::kLightRed);
  ASSERT_EQ(sensor_desc.values()[1].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);

  ASSERT_EQ(sensor_desc.values()[2].type, fuchsia_input_report::wire::SensorType::kLightBlue);
  ASSERT_EQ(sensor_desc.values()[2].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);

  ASSERT_EQ(sensor_desc.values()[3].type, fuchsia_input_report::wire::SensorType::kLightGreen);
  ASSERT_EQ(sensor_desc.values()[3].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);

  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader(GetReader(sync_client));

  // Create the report.
  ambient_light_input_rpt_t report_data = {};
  // Values are arbitrarily chosen.
  const int kIlluminanceTestVal = 10;
  const int kRedTestVal = 101;
  const int kBlueTestVal = 5;
  const int kGreenTestVal = 3;
  report_data.rpt_id = AMBIENT_LIGHT_RPT_ID_INPUT;
  report_data.illuminance = kIlluminanceTestVal;
  report_data.red = kRedTestVal;
  report_data.blue = kBlueTestVal;
  report_data.green = kGreenTestVal;

  SendReport(ToBinaryVector(report_data));

  // Get the report.
  auto report_result = reader->ReadInputReports();
  ASSERT_OK(report_result.status());

  const fidl::VectorView<fuchsia_input_report::wire::InputReport>& reports =
      report_result->value()->reports;
  ASSERT_EQ(1UL, reports.count());

  ASSERT_TRUE(reports[0].has_sensor());
  const fuchsia_input_report::wire::SensorInputReport& sensor_report = reports[0].sensor();
  EXPECT_TRUE(sensor_report.has_values());
  EXPECT_EQ(4UL, sensor_report.values().count());

  // Check the report.
  // These will always match the ordering in the descriptor.
  EXPECT_EQ(kIlluminanceTestVal, sensor_report.values()[0]);
  EXPECT_EQ(kRedTestVal, sensor_report.values()[1]);
  EXPECT_EQ(kBlueTestVal, sensor_report.values()[2]);
  EXPECT_EQ(kGreenTestVal, sensor_report.values()[3]);
}

TEST_F(HidDevTest, GetTouchInputReportTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t desc_len;
    const uint8_t* report_desc = get_paradise_touch_report_desc(&desc_len);
    env.fake_hid().SetReportDesc(ToBinaryVector(report_desc, desc_len));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  // Get an InputReportsReader.
  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader(GetReader());

  // Spoof send a report.
  paradise_touch_t touch_report = {};
  touch_report.rpt_id = PARADISE_RPT_ID_TOUCH;
  touch_report.contact_count = 1;
  touch_report.fingers[0].flags = 0xFF;
  touch_report.fingers[0].x = 100;
  touch_report.fingers[0].y = 200;
  touch_report.fingers[0].finger_id = 1;

  SendReport(ToBinaryVector(touch_report));

  // Get the report.
  auto report_result = reader->ReadInputReports();
  ASSERT_OK(report_result.status());

  const fidl::VectorView<fuchsia_input_report::wire::InputReport>& reports =
      report_result->value()->reports;
  ASSERT_EQ(1UL, reports.count());

  const auto& report = reports[0];
  const auto& touch = report.touch();
  ASSERT_TRUE(touch.has_contacts());
  ASSERT_EQ(1UL, touch.contacts().count());
  const auto& contact = touch.contacts()[0];

  ASSERT_TRUE(contact.has_position_x());
  ASSERT_EQ(2500, contact.position_x());

  ASSERT_TRUE(contact.has_position_y());
  ASSERT_EQ(5000, contact.position_y());
}

TEST_F(HidDevTest, GetTouchPadDescTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t desc_len;
    const uint8_t* report_desc = get_paradise_touchpad_v1_report_desc(&desc_len);
    env.fake_hid().SetReportDesc(ToBinaryVector(report_desc, desc_len));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  auto sync_client = GetSyncClient();
  fidl::WireResult<fuchsia_input_report::InputDevice::GetDescriptor> result =
      sync_client->GetDescriptor();
  ASSERT_OK(result.status());
  ASSERT_TRUE(result.value().descriptor.has_touch());
  ASSERT_TRUE(result.value().descriptor.touch().has_input());
  fuchsia_input_report::wire::TouchInputDescriptor& touch =
      result.value().descriptor.touch().input();

  ASSERT_EQ(fuchsia_input_report::wire::TouchType::kTouchpad, touch.touch_type());
}

TEST_F(HidDevTest, KeyboardTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t keyboard_descriptor_size;
    const uint8_t* keyboard_descriptor = get_boot_kbd_report_desc(&keyboard_descriptor_size);
    env.fake_hid().SetReportDesc(ToBinaryVector(keyboard_descriptor, keyboard_descriptor_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  // Get an InputReportsReader.
  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader(GetReader());

  // Spoof send a report.
  hid_boot_kbd_report keyboard_report = {};
  keyboard_report.usage[0] = HID_USAGE_KEY_A;
  keyboard_report.usage[1] = HID_USAGE_KEY_UP;
  keyboard_report.usage[2] = HID_USAGE_KEY_B;

  SendReport(ToBinaryVector(keyboard_report));

  // Get the report.
  auto report_result = reader->ReadInputReports();
  ASSERT_OK(report_result.status());

  const fidl::VectorView<fuchsia_input_report::wire::InputReport>& reports =
      report_result->value()->reports;
  ASSERT_EQ(1UL, reports.count());

  const auto& report = reports[0];
  const auto& keyboard = report.keyboard();
  ASSERT_EQ(3UL, keyboard.pressed_keys3().count());
  EXPECT_EQ(fuchsia_input::wire::Key::kA, keyboard.pressed_keys3()[0]);
  EXPECT_EQ(fuchsia_input::wire::Key::kUp, keyboard.pressed_keys3()[1]);
  EXPECT_EQ(fuchsia_input::wire::Key::kB, keyboard.pressed_keys3()[2]);
}

TEST_F(HidDevTest, KeyboardOutputReportTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t keyboard_descriptor_size;
    const uint8_t* keyboard_descriptor = get_boot_kbd_report_desc(&keyboard_descriptor_size);
    env.fake_hid().SetReportDesc(ToBinaryVector(keyboard_descriptor, keyboard_descriptor_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  auto sync_client = GetSyncClient();
  // Make an output report.
  fidl::Arena allocator;
  fidl::VectorView<fuchsia_input_report::wire::LedType> led_view(allocator, 2);
  led_view[0] = fuchsia_input_report::wire::LedType::kNumLock;
  led_view[1] = fuchsia_input_report::wire::LedType::kScrollLock;
  fuchsia_input_report::wire::KeyboardOutputReport fidl_keyboard(allocator);
  fidl_keyboard.set_enabled_leds(allocator, std::move(led_view));

  fuchsia_input_report::wire::OutputReport output_report(allocator);
  output_report.set_keyboard(allocator, std::move(fidl_keyboard));
  // Send the report.
  fidl::WireResult<fuchsia_input_report::InputDevice::SendOutputReport> response =
      sync_client->SendOutputReport(std::move(output_report));
  ASSERT_TRUE(response.ok());
  ASSERT_TRUE(response->is_ok());
  // Check the hid output report.
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    EXPECT_EQ(env.fake_hid().GetReport(), std::vector<uint8_t>{0b101});
  });
}

TEST_F(HidDevTest, ConsumerControlTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    const uint8_t* descriptor;
    size_t descriptor_size = get_buttons_report_desc(&descriptor);
    env.fake_hid().SetReportDesc(ToBinaryVector(descriptor, descriptor_size));

    // Create the initial report that will be queried on OpenSession.
    struct buttons_input_rpt report = {};
    report.rpt_id = BUTTONS_RPT_ID_INPUT;
    env.fake_hid().SetReport(ToBinaryVector(report));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  auto sync_client = GetSyncClient();

  // Get the report descriptor.
  fidl::WireResult<fuchsia_input_report::InputDevice::GetDescriptor> result =
      sync_client->GetDescriptor();
  ASSERT_OK(result.status());
  fuchsia_input_report::wire::DeviceDescriptor& desc = result.value().descriptor;
  ASSERT_TRUE(desc.has_consumer_control());
  ASSERT_TRUE(desc.consumer_control().has_input());
  fuchsia_input_report::wire::ConsumerControlInputDescriptor& consumer_control_desc =
      desc.consumer_control().input();
  ASSERT_TRUE(consumer_control_desc.has_buttons());
  ASSERT_EQ(5UL, consumer_control_desc.buttons().count());

  ASSERT_EQ(consumer_control_desc.buttons()[0],
            fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
  ASSERT_EQ(consumer_control_desc.buttons()[1],
            fuchsia_input_report::wire::ConsumerControlButton::kVolumeDown);
  ASSERT_EQ(consumer_control_desc.buttons()[2],
            fuchsia_input_report::wire::ConsumerControlButton::kFactoryReset);
  ASSERT_EQ(consumer_control_desc.buttons()[3],
            fuchsia_input_report::wire::ConsumerControlButton::kCameraDisable);
  ASSERT_EQ(consumer_control_desc.buttons()[4],
            fuchsia_input_report::wire::ConsumerControlButton::kMicMute);

  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader(GetReader(sync_client));

  // Create another report.
  struct buttons_input_rpt report = {};
  report.rpt_id = BUTTONS_RPT_ID_INPUT;
  fill_button_in_report(BUTTONS_ID_VOLUME_UP, true, &report);
  fill_button_in_report(BUTTONS_ID_FDR, true, &report);
  fill_button_in_report(BUTTONS_ID_MIC_MUTE, true, &report);

  SendReport(ToBinaryVector(report));

  // Get the report.
  auto report_result = reader->ReadInputReports();
  ASSERT_OK(report_result.status());

  const fidl::VectorView<fuchsia_input_report::wire::InputReport>& reports =
      report_result->value()->reports;
  ASSERT_EQ(2UL, reports.count());

  // Check the initial report.
  {
    ASSERT_TRUE(reports[0].has_consumer_control());
    const auto& report = reports[0].consumer_control();
    EXPECT_TRUE(report.has_pressed_buttons());
    EXPECT_EQ(0UL, report.pressed_buttons().count());
  }

  // Check the second report.
  {
    ASSERT_TRUE(reports[1].has_consumer_control());
    const auto& report = reports[1].consumer_control();
    EXPECT_TRUE(report.has_pressed_buttons());
    EXPECT_EQ(3UL, report.pressed_buttons().count());

    EXPECT_EQ(report.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
    EXPECT_EQ(report.pressed_buttons()[1],
              fuchsia_input_report::wire::ConsumerControlButton::kFactoryReset);
    EXPECT_EQ(report.pressed_buttons()[2],
              fuchsia_input_report::wire::ConsumerControlButton::kMicMute);
  }
}

TEST_F(HidDevTest, ConsumerControlTwoClientsTest) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    const uint8_t* descriptor;
    size_t descriptor_size = get_buttons_report_desc(&descriptor);
    env.fake_hid().SetReportDesc(ToBinaryVector(descriptor, descriptor_size));

    // Create the initial report that will be queried on OpenSession.
    struct buttons_input_rpt report = {};
    report.rpt_id = BUTTONS_RPT_ID_INPUT;
    env.fake_hid().SetReport(ToBinaryVector(report));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  // Open the device.
  auto client = GetSyncClient();

  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader1(GetReader(client));
  {
    auto report_result = reader1->ReadInputReports();
    ASSERT_OK(report_result.status());
    const fidl::VectorView<fuchsia_input_report::wire::InputReport>& reports =
        report_result->value()->reports;
    ASSERT_EQ(1UL, reports.count());

    ASSERT_TRUE(reports[0].has_consumer_control());
    const auto& report = reports[0].consumer_control();
    EXPECT_TRUE(report.has_pressed_buttons());
    EXPECT_EQ(0UL, report.pressed_buttons().count());
  }

  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader2(GetReader(client));
  {
    auto report_result = reader2->ReadInputReports();
    ASSERT_OK(report_result.status());
    const fidl::VectorView<fuchsia_input_report::wire::InputReport>& reports =
        report_result->value()->reports;
    ASSERT_EQ(1UL, reports.count());

    ASSERT_TRUE(reports[0].has_consumer_control());
    const auto& report = reports[0].consumer_control();
    EXPECT_TRUE(report.has_pressed_buttons());
    EXPECT_EQ(0UL, report.pressed_buttons().count());
  }

  // Create another report.
  struct buttons_input_rpt report = {};
  report.rpt_id = BUTTONS_RPT_ID_INPUT;
  fill_button_in_report(BUTTONS_ID_VOLUME_UP, true, &report);
  fill_button_in_report(BUTTONS_ID_FDR, true, &report);
  fill_button_in_report(BUTTONS_ID_MIC_MUTE, true, &report);

  SendReport(std::vector<uint8_t>(reinterpret_cast<uint8_t*>(&report),
                                  reinterpret_cast<uint8_t*>(&report) + sizeof(report)));

  {
    auto report_result = reader1->ReadInputReports();
    ASSERT_OK(report_result.status());
    const fidl::VectorView<fuchsia_input_report::wire::InputReport>& reports =
        report_result->value()->reports;
    ASSERT_EQ(1UL, reports.count());
    ASSERT_TRUE(reports[0].has_consumer_control());
    const auto& report = reports[0].consumer_control();
    EXPECT_TRUE(report.has_pressed_buttons());
    EXPECT_EQ(3UL, report.pressed_buttons().count());

    EXPECT_EQ(report.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
    EXPECT_EQ(report.pressed_buttons()[1],
              fuchsia_input_report::wire::ConsumerControlButton::kFactoryReset);
    EXPECT_EQ(report.pressed_buttons()[2],
              fuchsia_input_report::wire::ConsumerControlButton::kMicMute);
  }
  {
    auto report_result = reader2->ReadInputReports();
    ASSERT_OK(report_result.status());
    const fidl::VectorView<fuchsia_input_report::wire::InputReport>& reports =
        report_result->value()->reports;
    ASSERT_EQ(1UL, reports.count());
    ASSERT_TRUE(reports[0].has_consumer_control());
    const auto& report = reports[0].consumer_control();
    EXPECT_TRUE(report.has_pressed_buttons());
    EXPECT_EQ(3UL, report.pressed_buttons().count());

    EXPECT_EQ(report.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
    EXPECT_EQ(report.pressed_buttons()[1],
              fuchsia_input_report::wire::ConsumerControlButton::kFactoryReset);
    EXPECT_EQ(report.pressed_buttons()[2],
              fuchsia_input_report::wire::ConsumerControlButton::kMicMute);
  }
}

TEST_F(HidDevTest, TouchLatencyMeasurements) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    const uint8_t* report_desc;
    const size_t desc_len = get_gt92xx_report_desc(&report_desc);
    env.fake_hid().SetReportDesc(ToBinaryVector(report_desc, desc_len));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  using namespace inspect::testing;
  RunInDriverContext([](InputReportDriver& driver) {
    auto hierarchy = inspect::ReadFromVmo(driver.input_report_for_testing().InspectVmo());
    EXPECT_TRUE(hierarchy.is_ok());
  });

  // Send five reports, and verify that the inspect stats make sense.
  gt92xx_touch_t report = {};

  // Add some additional computed latency to make the results more realistic.
  const zx::time timestamp = zx::clock::get_monotonic() - zx::msec(15);

  SendReport(ToBinaryVector(report), timestamp);
  SendReport(ToBinaryVector(report), timestamp);
  SendReport(ToBinaryVector(report), timestamp);
  SendReport(ToBinaryVector(report), timestamp);
  SendReport(ToBinaryVector(report), timestamp - zx::msec(5));

  RunInDriverContext([](InputReportDriver& driver) {
    auto hierarchy = inspect::ReadFromVmo(driver.input_report_for_testing().InspectVmo());
    EXPECT_TRUE(hierarchy.is_ok());
    const inspect::Hierarchy* root = hierarchy.value().GetByPath({"hid-input-report-touch"});
    ASSERT_TRUE(root);

    const auto* latency_histogram =
        root->node().get_property<inspect::UintArrayValue>("latency_histogram_usecs");
    ASSERT_TRUE(latency_histogram);
    uint64_t latency_bucket_sum = 0;
    for (const inspect::UintArrayValue::HistogramBucket& bucket : latency_histogram->GetBuckets()) {
      latency_bucket_sum += bucket.count;
    }

    EXPECT_EQ(latency_bucket_sum, 5UL);

    const auto* average_latency =
        root->node().get_property<inspect::UintPropertyValue>("average_latency_usecs");
    ASSERT_TRUE(average_latency);

    const auto* max_latency =
        root->node().get_property<inspect::UintPropertyValue>("max_latency_usecs");
    ASSERT_TRUE(max_latency);

    EXPECT_GE(max_latency->value(), average_latency->value());
  });
}

TEST_F(HidDevTest, InspectDeviceTypes) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t desc_len;
    const uint8_t* report_desc = get_paradise_touch_report_desc(&desc_len);
    env.fake_hid().SetReportDesc(ToBinaryVector(report_desc, desc_len));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  RunInDriverContext([](InputReportDriver& driver) {
    auto hierarchy = inspect::ReadFromVmo(driver.input_report_for_testing().InspectVmo());
    EXPECT_TRUE(hierarchy.is_ok());
    const inspect::Hierarchy* root =
        hierarchy.value().GetByPath({"hid-input-report-touch,touch,mouse"});
    ASSERT_TRUE(root);

    const auto* device_types =
        root->node().get_property<inspect::StringPropertyValue>("device_types");
    ASSERT_TRUE(device_types);

    EXPECT_STREQ(device_types->value().c_str(), "touch,touch,mouse");
  });
}

TEST_F(HidDevTest, GetInputReport) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    const uint8_t* sensor_desc_ptr;
    size_t sensor_desc_size = get_ambient_light_report_desc(&sensor_desc_ptr);
    env.fake_hid().SetReportDesc(ToBinaryVector(sensor_desc_ptr, sensor_desc_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  auto sync_client = GetSyncClient();

  const int kIlluminanceTestVal = 10;
  const int kRedTestVal = 101;
  const int kBlueTestVal = 5;
  const int kGreenTestVal = 3;
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    ambient_light_input_rpt_t report_data = {};
    report_data.rpt_id = AMBIENT_LIGHT_RPT_ID_INPUT;
    report_data.illuminance = kIlluminanceTestVal;
    report_data.red = kRedTestVal;
    report_data.blue = kBlueTestVal;
    report_data.green = kGreenTestVal;
    env.fake_hid().SetReport(ToBinaryVector(report_data));
  });

  {
    const auto report_result =
        sync_client->GetInputReport(fuchsia_input_report::wire::DeviceType::kSensor);
    ASSERT_TRUE(report_result.ok());

    ASSERT_TRUE(report_result->is_ok());
    const auto& report = report_result->value()->report;

    ASSERT_TRUE(report.has_sensor());
    const fuchsia_input_report::wire::SensorInputReport& sensor_report = report.sensor();
    EXPECT_TRUE(sensor_report.has_values());
    EXPECT_EQ(4UL, sensor_report.values().count());

    EXPECT_EQ(kIlluminanceTestVal, sensor_report.values()[0]);
    EXPECT_EQ(kRedTestVal, sensor_report.values()[1]);
    EXPECT_EQ(kBlueTestVal, sensor_report.values()[2]);
    EXPECT_EQ(kGreenTestVal, sensor_report.values()[3]);
  }

  {
    // Requesting a different device type should fail.
    const auto result = sync_client->GetInputReport(fuchsia_input_report::wire::DeviceType::kTouch);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  }
}

TEST_F(HidDevTest, GetInputReportMultipleDevices) {
  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    size_t touch_desc_size;
    const uint8_t* touch_desc_ptr = get_acer12_touch_report_desc(&touch_desc_size);
    env.fake_hid().SetReportDesc(ToBinaryVector(touch_desc_ptr, touch_desc_size));
  });
  ASSERT_TRUE(StartDriver().is_ok());

  auto sync_client = GetSyncClient();

  RunInEnvironmentTypeContext([](InputReportTestEnvironment& env) {
    acer12_stylus_t report_data = {};
    report_data.rpt_id = ACER12_RPT_ID_STYLUS;
    report_data.status = 0;
    report_data.x = 1;
    report_data.y = 2;
    report_data.pressure = 3;
    env.fake_hid().SetReport(ToBinaryVector(report_data));
  });

  // There are two devices with this type (finger and stylus), so this should fail.
  const auto result = sync_client->GetInputReport(fuchsia_input_report::wire::DeviceType::kTouch);
  ASSERT_TRUE(result.ok());
  EXPECT_TRUE(result->is_error());
}

}  // namespace hid_input_report_dev
