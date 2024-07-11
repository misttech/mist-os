// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.input/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/hid/acer12.h>
#include <lib/hid/ambient-light.h>
#include <lib/hid/boot.h>
#include <lib/hid/buttons.h>
#include <lib/hid/gt92xx.h>
#include <lib/hid/paradise.h>
#include <lib/hid/usages.h>
#include <lib/inspect/testing/cpp/zxtest/inspect.h>
#include <lib/zx/channel.h>
#include <lib/zx/clock.h>
#include <lib/zx/eventpair.h>
#include <unistd.h>
#include <zircon/syscalls.h>

#include <ddk/metadata/buttons.h>
#include <zxtest/zxtest.h>

#include "driver_v1.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

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

class FakeHidDevice : public fidl::testing::WireTestBase<finput::Device> {
 public:
  class DeviceReportsReader : public fidl::WireServer<finput::DeviceReportsReader> {
   public:
    explicit DeviceReportsReader(FakeHidDevice* parent, async_dispatcher_t* dispatcher,
                                 fidl::ServerEnd<finput::DeviceReportsReader> server)
        : parent_(parent),
          binding_(dispatcher, std::move(server), this,
                   [this](fidl::UnbindInfo info) { parent_->reader_.reset(); }) {}

    ~DeviceReportsReader() override {
      if (waiting_read_) {
        waiting_read_->ReplyError(ZX_ERR_PEER_CLOSED);
        waiting_read_.reset();
      }
    }

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
    FakeHidDevice* parent_;
    fidl::ServerBinding<finput::DeviceReportsReader> binding_;
    std::optional<ReadReportsCompleter::Async> waiting_read_;
  };

  static constexpr uint32_t kVendorId = 0xabc;
  static constexpr uint32_t kProductId = 123;
  static constexpr uint32_t kVersion = 5;

  explicit FakeHidDevice(fidl::ServerEnd<finput::Device> server)
      : dispatcher_(async_get_default_dispatcher()),
        binding_(dispatcher_, std::move(server), this, fidl::kIgnoreBindingClosure) {}

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

  void GetDeviceReportsReader(GetDeviceReportsReaderRequestView request,
                              GetDeviceReportsReaderCompleter::Sync& completer) override {
    ASSERT_NULL(reader_);
    reader_ = std::make_unique<DeviceReportsReader>(this, dispatcher_, std::move(request->reader));
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

  void SendReport(std::vector<uint8_t> report, zx::time timestamp) {
    ASSERT_NOT_NULL(reader_);
    reader_->SendReport(std::move(report), timestamp);
  }

  libsync::Completion& wait_for_read() {
    EXPECT_NOT_NULL(reader_);
    return reader_->wait_for_read_;
  }

  std::unique_ptr<DeviceReportsReader> reader_;

 private:
  async_dispatcher_t* dispatcher_;
  fidl::ServerBinding<finput::Device> binding_;

  std::vector<uint8_t> report_desc_;
  std::vector<uint8_t> report_;
};

class HidDevTest : public zxtest::Test {
  void SetUp() override {
    hid_dev_loop_.StartThread("fake-hid-device-thread");
    auto [client, server] = fidl::Endpoints<finput::Device>::Create();
    fake_hid_.emplace(std::move(server));

    fake_parent_ = MockDevice::FakeRootParent();
    device_ = new InputReportDriver(fake_parent_.get(), std::move(client));
    fidl_loop_.StartThread("fidl-thread");
    // Each test is responsible for calling |device_->Bind()|.
  }

  void TearDown() override {
    if (background_dispatcher_) {
      libsync::Completion wait;
      async::PostTask((*background_dispatcher_)->async_dispatcher(), [this, &wait]() {
        device_async_remove(device_->zxdev());
        mock_ddk::ReleaseFlaggedDevices(fake_parent_.get());
        wait.Signal();
      });
      wait.Wait();
    } else {
      device_async_remove(device_->zxdev());
      mock_ddk::ReleaseFlaggedDevices(fake_parent_.get());
    }
  }

 protected:
  // Run device on background dispatcher so that we can wait for reports on foreground dispatcher.
  void BindOnBackground() {
    background_dispatcher_ = mock_ddk::GetDriverRuntime()->StartBackgroundDispatcher();
    libsync::Completion wait;
    async::PostTask((*background_dispatcher_)->async_dispatcher(), [this, &wait]() {
      device_->Bind();
      wait.Signal();
    });
    wait.Wait();
  }

  fidl::WireSyncClient<fuchsia_input_report::InputDevice> GetSyncClient() {
    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputDevice>::Create();
    fidl::BindServer(fidl_loop_.dispatcher(), std::move(endpoints.server), device_);
    return fidl::WireSyncClient(std::move(endpoints.client));
  }

  void SendReport(std::vector<uint8_t> report, zx::time timestamp = zx::time::infinite()) {
    if (timestamp == zx::time::infinite()) {
      timestamp = zx::clock::get_monotonic();
    }

    mock_ddk::GetDriverRuntime()->PerformBlockingWork([this]() {
      fake_hid_.SyncCall(&FakeHidDevice::wait_for_read).Wait();
      fake_hid_.SyncCall(&FakeHidDevice::wait_for_read).Reset();
    });
    fake_hid_.SyncCall(&FakeHidDevice::SendReport, std::move(report), timestamp);
    mock_ddk::GetDriverRuntime()->PerformBlockingWork(
        [this]() { fake_hid_.SyncCall(&FakeHidDevice::wait_for_read).Wait(); });
  }

  async::Loop fidl_loop_ = async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  std::shared_ptr<MockDevice> fake_parent_;
  async::Loop hid_dev_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<FakeHidDevice> fake_hid_{hid_dev_loop_.dispatcher()};
  std::optional<fdf::UnownedSynchronizedDispatcher> background_dispatcher_;
  InputReportDriver* device_;
};

TEST_F(HidDevTest, HidLifetimeTest) {
  size_t boot_mouse_desc_size;
  const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc,
                     ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));

  ASSERT_OK(device_->Bind());
}

TEST(HidDevTest, InputReportUnregisterTest) {
  auto fake_parent = MockDevice::FakeRootParent();
  auto [client, server] = fidl::Endpoints<finput::Device>::Create();
  async::Loop hid_dev_loop{&kAsyncLoopConfigNoAttachToCurrentThread};
  hid_dev_loop.StartThread("fake-hid-device-thread");
  async_patterns::TestDispatcherBound<FakeHidDevice> fake_hid{hid_dev_loop.dispatcher(),
                                                              std::in_place, std::move(server)};
  InputReportDriver* device_;

  device_ = new InputReportDriver(fake_parent.get(), std::move(client));

  size_t boot_mouse_desc_size;
  const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
  fake_hid.SyncCall(&FakeHidDevice::SetReportDesc,
                    ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));

  ASSERT_OK(device_->Bind());

  mock_ddk::ReleaseFlaggedDevices(fake_parent.get());
  fake_parent.reset();

  // Make sure that the InputReport class has unregistered from the HID device.
  fake_hid.SyncCall([](FakeHidDevice* hid) { ASSERT_NULL(hid->reader_); });
}

TEST(HidDevTest, InputReportUnregisterTestBindFailed) {
  auto fake_parent = MockDevice::FakeRootParent();
  auto [client, server] = fidl::Endpoints<finput::Device>::Create();
  async::Loop hid_dev_loop{&kAsyncLoopConfigNoAttachToCurrentThread};
  hid_dev_loop.StartThread("fake-hid-device-thread");
  async_patterns::TestDispatcherBound<FakeHidDevice> fake_hid{hid_dev_loop.dispatcher(),
                                                              std::in_place, std::move(server)};

  auto device = std::make_unique<InputReportDriver>(fake_parent.get(), std::move(client));

  // We don't set a input report on `fake_hid` so the bind will fail.
  ASSERT_EQ(device->Bind(), ZX_ERR_INTERNAL);

  // Make sure that the InputReport class is not registered to the HID device.
  fake_hid.SyncCall([](FakeHidDevice* hid) { ASSERT_NULL(hid->reader_); });
}

TEST_F(HidDevTest, GetReportDescTest) {
  size_t boot_mouse_desc_size;
  const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc,
                     ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));

  ASSERT_OK(device_->Bind());

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
  size_t boot_mouse_desc_size;
  const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc,
                     ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));

  ASSERT_OK(device_->Bind());

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
  size_t boot_mouse_desc_size;
  const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc,
                     ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));

  BindOnBackground();

  auto sync_client = GetSyncClient();

  // Get an InputReportsReader.
  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader;
  {
    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();

    auto result = sync_client->GetInputReportsReader(std::move(endpoints.server));
    ASSERT_OK(result.status());
    reader =
        fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(endpoints.client));
    ASSERT_OK(device_->input_report().WaitForNextReader(zx::duration::infinite()));
  }

  // Spoof send a report.
  SendReport(std::vector<uint8_t>{0xFF, 0x50, 0x70});

  // Get the report.
  auto result = reader->ReadInputReports();
  ASSERT_OK(result.status());
  ASSERT_FALSE(result->is_error());
  auto& reports = result->value()->reports;

  ASSERT_EQ(1, reports.count());

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
  size_t boot_mouse_desc_size;
  const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc,
                     ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));

  BindOnBackground();

  auto sync_client = GetSyncClient();

  // Get an InputReportsReader.

  async::Loop loop = async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  fidl::WireClient<fuchsia_input_report::InputReportsReader> reader;
  {
    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();

    auto result = sync_client->GetInputReportsReader(std::move(endpoints.server));
    ASSERT_OK(result.status());
    reader.Bind(std::move(endpoints.client), loop.dispatcher());
    ASSERT_OK(device_->input_report().WaitForNextReader(zx::duration::infinite()));
  }

  // Read the report. This will hang until a report is sent.
  reader->ReadInputReports().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_input_report::InputReportsReader::ReadInputReports>&
              response) {
        ASSERT_OK(response.status());
        ASSERT_FALSE(response->is_error());
        auto& reports = response->value()->reports;
        ASSERT_EQ(1, reports.count());

        auto& report = reports[0];
        ASSERT_TRUE(report.has_event_time());
        ASSERT_TRUE(report.has_mouse());
        auto& mouse = report.mouse();

        ASSERT_TRUE(mouse.has_movement_x());
        ASSERT_EQ(0x50, mouse.movement_x());

        ASSERT_TRUE(mouse.has_movement_y());
        ASSERT_EQ(0x70, mouse.movement_y());
        loop.Quit();
      });
  loop.RunUntilIdle();

  // Send the report.
  SendReport(std::vector<uint8_t>{0xFF, 0x50, 0x70});

  loop.Run();
}

TEST_F(HidDevTest, CloseReaderWithOutstandingRead) {
  size_t boot_mouse_desc_size;
  const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc,
                     ToBinaryVector(boot_mouse_desc, boot_mouse_desc_size));

  device_->Bind();

  auto sync_client = GetSyncClient();

  // Get an InputReportsReader.

  async::Loop loop = async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  fidl::WireClient<fuchsia_input_report::InputReportsReader> reader;
  {
    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();

    auto result = sync_client->GetInputReportsReader(std::move(endpoints.server));
    ASSERT_OK(result.status());
    reader.Bind(std::move(endpoints.client), loop.dispatcher());
    ASSERT_OK(device_->input_report().WaitForNextReader(zx::duration::infinite()));
  }

  // Queue a report.
  reader->ReadInputReports().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_input_report::InputReportsReader::ReadInputReports>&
              result) { ASSERT_TRUE(result.is_canceled()); });
  loop.RunUntilIdle();

  // Unbind the reader now that the report is waiting.
  reader = {};
}

TEST_F(HidDevTest, SensorTest) {
  const uint8_t* sensor_desc_ptr;
  size_t sensor_desc_size = get_ambient_light_report_desc(&sensor_desc_ptr);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc,
                     ToBinaryVector(sensor_desc_ptr, sensor_desc_size));

  BindOnBackground();

  auto sync_client = GetSyncClient();

  // Get the report descriptor.
  fidl::WireResult<fuchsia_input_report::InputDevice::GetDescriptor> result =
      sync_client->GetDescriptor();
  ASSERT_OK(result.status());
  fuchsia_input_report::wire::DeviceDescriptor& desc = result.value().descriptor;
  ASSERT_TRUE(desc.has_sensor());
  ASSERT_TRUE(desc.sensor().has_input());
  ASSERT_EQ(desc.sensor().input().count(), 1);
  fuchsia_input_report::wire::SensorInputDescriptor& sensor_desc = desc.sensor().input()[0];
  ASSERT_TRUE(sensor_desc.has_values());
  ASSERT_EQ(4, sensor_desc.values().count());

  ASSERT_EQ(sensor_desc.values()[0].type,
            fuchsia_input_report::wire::SensorType::kLightIlluminance);
  ASSERT_EQ(sensor_desc.values()[0].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);

  ASSERT_EQ(sensor_desc.values()[1].type, fuchsia_input_report::wire::SensorType::kLightRed);
  ASSERT_EQ(sensor_desc.values()[1].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);

  ASSERT_EQ(sensor_desc.values()[2].type, fuchsia_input_report::wire::SensorType::kLightBlue);
  ASSERT_EQ(sensor_desc.values()[2].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);

  ASSERT_EQ(sensor_desc.values()[3].type, fuchsia_input_report::wire::SensorType::kLightGreen);
  ASSERT_EQ(sensor_desc.values()[3].axis.unit.type, fuchsia_input_report::wire::UnitType::kNone);

  // Get an InputReportsReader.
  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader;
  {
    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();

    auto result = sync_client->GetInputReportsReader(std::move(endpoints.server));
    ASSERT_OK(result.status());
    reader =
        fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(endpoints.client));
    ASSERT_OK(device_->input_report().WaitForNextReader(zx::duration::infinite()));
  }

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
  ASSERT_EQ(1, reports.count());

  ASSERT_TRUE(reports[0].has_sensor());
  const fuchsia_input_report::wire::SensorInputReport& sensor_report = reports[0].sensor();
  EXPECT_TRUE(sensor_report.has_values());
  EXPECT_EQ(4, sensor_report.values().count());

  // Check the report.
  // These will always match the ordering in the descriptor.
  EXPECT_EQ(kIlluminanceTestVal, sensor_report.values()[0]);
  EXPECT_EQ(kRedTestVal, sensor_report.values()[1]);
  EXPECT_EQ(kBlueTestVal, sensor_report.values()[2]);
  EXPECT_EQ(kGreenTestVal, sensor_report.values()[3]);
}

TEST_F(HidDevTest, GetTouchInputReportTest) {
  size_t desc_len;
  const uint8_t* report_desc = get_paradise_touch_report_desc(&desc_len);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc, ToBinaryVector(report_desc, desc_len));

  BindOnBackground();

  auto sync_client = GetSyncClient();

  // Get an InputReportsReader.
  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader;
  {
    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();

    auto result = sync_client->GetInputReportsReader(std::move(endpoints.server));
    ASSERT_OK(result.status());
    reader =
        fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(endpoints.client));
    ASSERT_OK(device_->input_report().WaitForNextReader(zx::duration::infinite()));
  }

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
  ASSERT_EQ(1, reports.count());

  const auto& report = reports[0];
  const auto& touch = report.touch();
  ASSERT_TRUE(touch.has_contacts());
  ASSERT_EQ(1, touch.contacts().count());
  const auto& contact = touch.contacts()[0];

  ASSERT_TRUE(contact.has_position_x());
  ASSERT_EQ(2500, contact.position_x());

  ASSERT_TRUE(contact.has_position_y());
  ASSERT_EQ(5000, contact.position_y());
}

TEST_F(HidDevTest, GetTouchPadDescTest) {
  size_t desc_len;
  const uint8_t* report_desc = get_paradise_touchpad_v1_report_desc(&desc_len);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc, ToBinaryVector(report_desc, desc_len));

  device_->Bind();

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
  size_t keyboard_descriptor_size;
  const uint8_t* keyboard_descriptor = get_boot_kbd_report_desc(&keyboard_descriptor_size);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc,
                     ToBinaryVector(keyboard_descriptor, keyboard_descriptor_size));
  BindOnBackground();

  auto sync_client = GetSyncClient();

  // Get an InputReportsReader.
  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader;
  {
    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();

    auto result = sync_client->GetInputReportsReader(std::move(endpoints.server));
    ASSERT_OK(result.status());
    reader =
        fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(endpoints.client));
    ASSERT_OK(device_->input_report().WaitForNextReader(zx::duration::infinite()));
  }

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
  ASSERT_EQ(1, reports.count());

  const auto& report = reports[0];
  const auto& keyboard = report.keyboard();
  ASSERT_EQ(3, keyboard.pressed_keys3().count());
  EXPECT_EQ(fuchsia_input::wire::Key::kA, keyboard.pressed_keys3()[0]);
  EXPECT_EQ(fuchsia_input::wire::Key::kUp, keyboard.pressed_keys3()[1]);
  EXPECT_EQ(fuchsia_input::wire::Key::kB, keyboard.pressed_keys3()[2]);
}

TEST_F(HidDevTest, KeyboardOutputReportTest) {
  size_t keyboard_descriptor_size;
  const uint8_t* keyboard_descriptor = get_boot_kbd_report_desc(&keyboard_descriptor_size);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc,
                     ToBinaryVector(keyboard_descriptor, keyboard_descriptor_size));
  device_->Bind();

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
  fake_hid_.SyncCall(
      [](FakeHidDevice* hid) { EXPECT_EQ(hid->GetReport(), std::vector<uint8_t>{0b101}); });
}

// TODO: Flaky. Will be re-enabled when converted to DFv2.
TEST_F(HidDevTest, DISABLED_ConsumerControlTest) {
  {
    const uint8_t* descriptor;
    size_t descriptor_size = get_buttons_report_desc(&descriptor);
    fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc, ToBinaryVector(descriptor, descriptor_size));
  }

  // Create the initial report that will be queried on OpenSession.
  fake_hid_.SyncCall([](FakeHidDevice* hid) {
    struct buttons_input_rpt report = {};
    report.rpt_id = BUTTONS_RPT_ID_INPUT;
    hid->SetReport(ToBinaryVector(report));
  });

  BindOnBackground();

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
  ASSERT_EQ(5, consumer_control_desc.buttons().count());

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

  // Get an InputReportsReader.
  fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader;
  {
    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();

    auto result = sync_client->GetInputReportsReader(std::move(endpoints.server));
    ASSERT_OK(result.status());
    reader =
        fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(std::move(endpoints.client));
    ASSERT_OK(device_->input_report().WaitForNextReader(zx::duration::infinite()));
  }

  // Create another report.
  {
    struct buttons_input_rpt report = {};
    report.rpt_id = BUTTONS_RPT_ID_INPUT;
    fill_button_in_report(BUTTONS_ID_VOLUME_UP, true, &report);
    fill_button_in_report(BUTTONS_ID_FDR, true, &report);
    fill_button_in_report(BUTTONS_ID_MIC_MUTE, true, &report);

    SendReport(ToBinaryVector(report));
  }

  // Get the report.
  auto report_result = reader->ReadInputReports();
  ASSERT_OK(report_result.status());

  const fidl::VectorView<fuchsia_input_report::wire::InputReport>& reports =
      report_result->value()->reports;
  ASSERT_EQ(2, reports.count());

  // Check the initial report.
  {
    ASSERT_TRUE(reports[0].has_consumer_control());
    const auto& report = reports[0].consumer_control();
    EXPECT_TRUE(report.has_pressed_buttons());
    EXPECT_EQ(0, report.pressed_buttons().count());
  }

  // Check the second report.
  {
    ASSERT_TRUE(reports[1].has_consumer_control());
    const auto& report = reports[1].consumer_control();
    EXPECT_TRUE(report.has_pressed_buttons());
    EXPECT_EQ(3, report.pressed_buttons().count());

    EXPECT_EQ(report.pressed_buttons()[0],
              fuchsia_input_report::wire::ConsumerControlButton::kVolumeUp);
    EXPECT_EQ(report.pressed_buttons()[1],
              fuchsia_input_report::wire::ConsumerControlButton::kFactoryReset);
    EXPECT_EQ(report.pressed_buttons()[2],
              fuchsia_input_report::wire::ConsumerControlButton::kMicMute);
  }
}

TEST_F(HidDevTest, ConsumerControlTwoClientsTest) {
  {
    const uint8_t* descriptor;
    size_t descriptor_size = get_buttons_report_desc(&descriptor);
    fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc, ToBinaryVector(descriptor, descriptor_size));
  }

  // Create the initial report that will be queried on OpenSession.
  fake_hid_.SyncCall([](FakeHidDevice* hid) {
    struct buttons_input_rpt report = {};
    report.rpt_id = BUTTONS_RPT_ID_INPUT;
    hid->SetReport(ToBinaryVector(report));
  });

  BindOnBackground();

  // Open the device.
  auto client = GetSyncClient();

  // Get an input reader and check reports.
  {
    // Get an InputReportsReader.
    fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader;
    {
      auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();

      auto result = client->GetInputReportsReader(std::move(endpoints.server));
      ASSERT_OK(result.status());
      reader = fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(
          std::move(endpoints.client));
    }

    auto report_result = reader->ReadInputReports();
    ASSERT_OK(report_result.status());
    const fidl::VectorView<fuchsia_input_report::wire::InputReport>& reports =
        report_result->value()->reports;
    ASSERT_EQ(1, reports.count());

    ASSERT_TRUE(reports[0].has_consumer_control());
    const auto& report = reports[0].consumer_control();
    EXPECT_TRUE(report.has_pressed_buttons());
    EXPECT_EQ(0, report.pressed_buttons().count());
  }

  {
    // Get an InputReportsReader.
    fidl::WireSyncClient<fuchsia_input_report::InputReportsReader> reader;
    {
      auto endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();

      auto result = client->GetInputReportsReader(std::move(endpoints.server));
      ASSERT_OK(result.status());
      reader = fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(
          std::move(endpoints.client));
      ASSERT_OK(device_->input_report().WaitForNextReader(zx::duration::infinite()));
    }

    auto report_result = reader->ReadInputReports();
    ASSERT_OK(report_result.status());
    const fidl::VectorView<fuchsia_input_report::wire::InputReport>& reports =
        report_result->value()->reports;
    ASSERT_EQ(1, reports.count());

    ASSERT_TRUE(reports[0].has_consumer_control());
    const auto& report = reports[0].consumer_control();
    EXPECT_TRUE(report.has_pressed_buttons());
    EXPECT_EQ(0, report.pressed_buttons().count());
  }
}

TEST_F(HidDevTest, TouchLatencyMeasurements) {
  const uint8_t* report_desc;
  const size_t desc_len = get_gt92xx_report_desc(&report_desc);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc, ToBinaryVector(report_desc, desc_len));

  device_->Bind();

  const zx::vmo& inspect_vmo = fake_parent_->GetLatestChild()->GetInspectVmo();
  ASSERT_TRUE(inspect_vmo.is_valid());

  // Send five reports, and verify that the inspect stats make sense.
  gt92xx_touch_t report = {};

  // Add some additional computed latency to make the results more realistic.
  const zx::time timestamp = zx::clock::get_monotonic() - zx::msec(15);

  SendReport(ToBinaryVector(report), timestamp);
  SendReport(ToBinaryVector(report), timestamp);
  SendReport(ToBinaryVector(report), timestamp);
  SendReport(ToBinaryVector(report), timestamp);
  SendReport(ToBinaryVector(report), timestamp - zx::msec(5));

  inspect::InspectTestHelper inspector;
  inspector.ReadInspect(inspect_vmo);

  const inspect::Hierarchy* root = inspector.hierarchy().GetByPath({"hid-input-report-touch"});
  ASSERT_NOT_NULL(root);

  const auto* latency_histogram =
      root->node().get_property<inspect::UintArrayValue>("latency_histogram_usecs");
  ASSERT_NOT_NULL(latency_histogram);
  uint64_t latency_bucket_sum = 0;
  for (const inspect::UintArrayValue::HistogramBucket& bucket : latency_histogram->GetBuckets()) {
    latency_bucket_sum += bucket.count;
  }

  EXPECT_EQ(latency_bucket_sum, 5);

  const auto* average_latency =
      root->node().get_property<inspect::UintPropertyValue>("average_latency_usecs");
  ASSERT_NOT_NULL(average_latency);

  const auto* max_latency =
      root->node().get_property<inspect::UintPropertyValue>("max_latency_usecs");
  ASSERT_NOT_NULL(max_latency);

  EXPECT_GE(max_latency->value(), average_latency->value());
}

TEST_F(HidDevTest, InspectDeviceTypes) {
  size_t desc_len;
  const uint8_t* report_desc = get_paradise_touch_report_desc(&desc_len);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc, ToBinaryVector(report_desc, desc_len));

  device_->Bind();

  const zx::vmo& inspect_vmo = fake_parent_->GetLatestChild()->GetInspectVmo();
  ASSERT_TRUE(inspect_vmo.is_valid());

  inspect::InspectTestHelper inspector;
  inspector.ReadInspect(inspect_vmo);

  const inspect::Hierarchy* root =
      inspector.hierarchy().GetByPath({"hid-input-report-touch,touch,mouse"});
  ASSERT_NOT_NULL(root);

  const auto* device_types =
      root->node().get_property<inspect::StringPropertyValue>("device_types");
  ASSERT_NOT_NULL(device_types);

  EXPECT_STREQ(device_types->value().c_str(), "touch,touch,mouse");
}

TEST_F(HidDevTest, GetInputReport) {
  const uint8_t* sensor_desc_ptr;
  size_t sensor_desc_size = get_ambient_light_report_desc(&sensor_desc_ptr);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc,
                     ToBinaryVector(sensor_desc_ptr, sensor_desc_size));

  device_->Bind();

  auto sync_client = GetSyncClient();

  const int kIlluminanceTestVal = 10;
  const int kRedTestVal = 101;
  const int kBlueTestVal = 5;
  const int kGreenTestVal = 3;
  fake_hid_.SyncCall([](FakeHidDevice* hid) {
    ambient_light_input_rpt_t report_data = {};
    report_data.rpt_id = AMBIENT_LIGHT_RPT_ID_INPUT;
    report_data.illuminance = kIlluminanceTestVal;
    report_data.red = kRedTestVal;
    report_data.blue = kBlueTestVal;
    report_data.green = kGreenTestVal;
    hid->SetReport(ToBinaryVector(report_data));
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
    EXPECT_EQ(4, sensor_report.values().count());

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
  size_t touch_desc_size;
  const uint8_t* touch_desc_ptr = get_acer12_touch_report_desc(&touch_desc_size);
  fake_hid_.SyncCall(&FakeHidDevice::SetReportDesc,
                     ToBinaryVector(touch_desc_ptr, touch_desc_size));

  device_->Bind();

  auto sync_client = GetSyncClient();

  fake_hid_.SyncCall([](FakeHidDevice* hid) {
    acer12_stylus_t report_data = {};
    report_data.rpt_id = ACER12_RPT_ID_STYLUS;
    report_data.status = 0;
    report_data.x = 1;
    report_data.y = 2;
    report_data.pressure = 3;
    hid->SetReport(ToBinaryVector(report_data));
  });

  // There are two devices with this type (finger and stylus), so this should fail.
  const auto result = sync_client->GetInputReport(fuchsia_input_report::wire::DeviceType::kTouch);
  ASSERT_TRUE(result.ok());
  EXPECT_TRUE(result->is_error());
}

}  // namespace hid_input_report_dev
