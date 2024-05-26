// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "hid.h"

#include <fidl/fuchsia.hardware.hidbus/cpp/wire_test_base.h>
#include <lib/driver/runtime/testing/cpp/sync_helpers.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/hid/ambient-light.h>
#include <lib/hid/boot.h>
#include <lib/hid/paradise.h>
#include <unistd.h>

#include <thread>
#include <vector>

#include <fbl/ref_ptr.h>
#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace hid_driver {

namespace fhidbus = fuchsia_hardware_hidbus;

class BanjoFakeHidbus : public ddk::HidbusProtocol<BanjoFakeHidbus> {
 public:
  BanjoFakeHidbus() : proto_({&hidbus_protocol_ops_, this}) {}

  hid_info_t Query() { return info_; }
  zx_status_t HidbusQuery(uint32_t options, hid_info_t* out_info) {
    *out_info = info_;
    return ZX_OK;
  }

  void SetHidInfo(hid_info_t info) { info_ = info; }

  void SetStartStatus(zx_status_t status) { start_status_ = status; }

  zx_status_t HidbusStart(const hidbus_ifc_protocol_t* ifc) {
    if (start_status_ != ZX_OK) {
      return start_status_;
    }
    ifc_ = *ifc;
    return ZX_OK;
  }

  void SendReport(const uint8_t* report_data, size_t report_size) {
    ASSERT_NE(ifc_.ops, nullptr);
    ifc_.ops->io_queue(ifc_.ctx, report_data, report_size, zx_clock_get_monotonic());
  }

  void SendReportWithTime(const uint8_t* report_data, size_t report_size, zx_time_t time) {
    ASSERT_NE(ifc_.ops, nullptr);
    ifc_.ops->io_queue(ifc_.ctx, report_data, report_size, time);
  }

  void HidbusStop() { ifc_.ops = nullptr; }

  zx_status_t HidbusGetDescriptor(hid_description_type_t desc_type, uint8_t* out_data_buffer,
                                  size_t data_size, size_t* out_data_actual) {
    if (data_size < report_desc_.size()) {
      return ZX_ERR_BUFFER_TOO_SMALL;
    }

    memcpy(out_data_buffer, report_desc_.data(), report_desc_.size());
    *out_data_actual = report_desc_.size();

    return ZX_OK;
  }

  void SetDescriptor(const uint8_t* desc, size_t desc_len) {
    report_desc_ = std::vector<uint8_t>(desc, desc + desc_len);
  }

  zx_status_t HidbusGetReport(hid_report_type_t rpt_type, uint8_t rpt_id, uint8_t* out_data_buffer,
                              size_t data_size, size_t* out_data_actual) {
    if (rpt_id != last_set_report_id_) {
      return ZX_ERR_INTERNAL;
    }
    if (data_size < last_set_report_.size()) {
      return ZX_ERR_BUFFER_TOO_SMALL;
    }

    memcpy(out_data_buffer, last_set_report_.data(), last_set_report_.size());
    *out_data_actual = last_set_report_.size();

    return ZX_OK;
  }

  zx_status_t HidbusSetReport(hid_report_type_t rpt_type, uint8_t rpt_id,
                              const uint8_t* data_buffer, size_t data_size) {
    last_set_report_id_ = rpt_id;
    auto data_bytes = reinterpret_cast<const uint8_t*>(data_buffer);
    last_set_report_ = std::vector<uint8_t>(data_bytes, data_bytes + data_size);

    return ZX_OK;
  }

  zx_status_t HidbusGetIdle(uint8_t rpt_id, uint8_t* out_duration) {
    *out_duration = 0;
    return ZX_OK;
  }

  zx_status_t HidbusSetIdle(uint8_t rpt_id, uint8_t duration) { return ZX_OK; }

  zx_status_t HidbusGetProtocol(hid_protocol_t* out_protocol) {
    *out_protocol = hid_protocol_;
    return ZX_OK;
  }

  zx_status_t HidbusSetProtocol(hid_protocol_t protocol) {
    hid_protocol_ = protocol;
    return ZX_OK;
  }
  void SetProtocol(hid_protocol_t proto) { hid_protocol_ = proto; }

  ddk::HidbusProtocolClient GetClient() { return ddk::HidbusProtocolClient(&proto_); }

 protected:
  std::vector<uint8_t> report_desc_;

  std::vector<uint8_t> last_set_report_;
  uint8_t last_set_report_id_;

  hid_protocol_t hid_protocol_ = HID_PROTOCOL_REPORT;
  hidbus_protocol_t proto_ = {};
  hid_info_t info_ = {};
  hidbus_ifc_protocol_t ifc_;
  zx_status_t start_status_ = ZX_OK;
};

class FidlFakeHidbus : public fidl::testing::WireTestBase<fhidbus::Hidbus> {
 public:
  FidlFakeHidbus() : loop_(&kAsyncLoopConfigNeverAttachToThread) {
    loop_.StartThread("hid-test-fake-hidbus-loop");
  }
  ~FidlFakeHidbus() {
    libsync::Completion wait;
    async::PostTask(loop_.dispatcher(), [this, &wait]() {
      binding_.RemoveAll();
      wait.Signal();
    });
    wait.Wait();
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    ASSERT_TRUE(false);
  }

  hid_info_t Query() {
    return {
        .dev_num = info_.dev_num(),
        .device_class = static_cast<hid_device_class_t>(info_.boot_protocol()),
        .boot_device = info_.boot_protocol() == fhidbus::HidBootProtocol::kNone,
        .vendor_id = info_.vendor_id(),
        .product_id = info_.product_id(),
        .version = info_.version(),
        .polling_rate = info_.polling_rate(),
    };
  }
  void Query(QueryCompleter::Sync& completer) override { completer.ReplySuccess(info_); }
  void Start(StartCompleter::Sync& completer) override {
    if (start_status_ != ZX_OK) {
      completer.ReplyError(start_status_);
      return;
    }
    completer.ReplySuccess();
  }
  void Stop(StopCompleter::Sync& completer) override {}
  void SetDescriptor(fhidbus::wire::HidbusSetDescriptorRequest* request,
                     SetDescriptorCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void GetDescriptor(fhidbus::wire::HidbusGetDescriptorRequest* request,
                     GetDescriptorCompleter::Sync& completer) override {
    completer.ReplySuccess(
        fidl::VectorView<uint8_t>::FromExternal(report_desc_.data(), report_desc_.size()));
  }
  void GetReport(fhidbus::wire::HidbusGetReportRequest* request,
                 GetReportCompleter::Sync& completer) override {
    if (request->rpt_id != last_set_report_id_) {
      completer.ReplyError(ZX_ERR_INTERNAL);
      return;
    }
    if (request->len < last_set_report_.size()) {
      completer.ReplyError(ZX_ERR_BUFFER_TOO_SMALL);
    }

    completer.ReplySuccess(
        fidl::VectorView<uint8_t>::FromExternal(last_set_report_.data(), last_set_report_.size()));
  }
  void SetReport(fhidbus::wire::HidbusSetReportRequest* request,
                 SetReportCompleter::Sync& completer) override {
    last_set_report_id_ = request->rpt_id;
    last_set_report_ =
        std::vector<uint8_t>(request->data.data(), request->data.data() + request->data.count());
    completer.ReplySuccess();
  }
  void GetIdle(fhidbus::wire::HidbusGetIdleRequest* request,
               GetIdleCompleter::Sync& completer) override {
    completer.ReplySuccess(0);
  }
  void SetIdle(fhidbus::wire::HidbusSetIdleRequest* request,
               SetIdleCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }
  void GetProtocol(GetProtocolCompleter::Sync& completer) override {
    completer.ReplySuccess(hid_protocol_);
  }
  void SetProtocol(fhidbus::wire::HidbusSetProtocolRequest* request,
                   SetProtocolCompleter::Sync& completer) override {
    hid_protocol_ = request->protocol;
    completer.ReplySuccess();
  }

  void SetProtocol(hid_protocol_t proto) {
    hid_protocol_ = static_cast<fhidbus::wire::HidProtocol>(proto);
  }
  void SetHidInfo(hid_info_t info) {
    info_ = fhidbus::wire::HidInfo::Builder(arena_)
                .dev_num(info.dev_num)
                .boot_protocol(static_cast<fhidbus::wire::HidBootProtocol>(info.device_class))
                .vendor_id(info.vendor_id)
                .product_id(info.product_id)
                .version(info.version)
                .polling_rate(info.polling_rate)
                .Build();
    ASSERT_EQ(info.boot_device, info_.boot_protocol() != fhidbus::HidBootProtocol::kNone);
  }
  void SetStartStatus(zx_status_t status) { start_status_ = status; }
  void SendReport(uint8_t* report_data, size_t report_size) {
    SendReportWithTime(report_data, report_size, zx_clock_get_monotonic());
  }
  void SendReportWithTime(uint8_t* report_data, size_t report_size, zx_time_t time) {
    binding_.ForEachBinding([&](const auto& binding) {
      auto result = fidl::WireSendEvent(binding)->OnReportReceived(
          fidl::VectorView<uint8_t>::FromExternal(report_data, report_size), time);
      ASSERT_TRUE(result.ok());
    });
  }
  void SetDescriptor(const uint8_t* desc, size_t desc_len) {
    report_desc_ = std::vector<uint8_t>(desc, desc + desc_len);
  }

  fidl::ClientEnd<fhidbus::Hidbus> GetClient() {
    auto endpoints = fidl::CreateEndpoints<fhidbus::Hidbus>();
    EXPECT_OK(endpoints);
    libsync::Completion wait;
    EXPECT_OK(async::PostTask(loop_.dispatcher(), [this, &endpoints, &wait]() {
      binding_.AddBinding(loop_.dispatcher(), std::move(endpoints->server), this,
                          fidl::kIgnoreBindingClosure);
      wait.Signal();
    }));
    wait.Wait();
    return std::move(endpoints->client);
  }

 protected:
  std::vector<uint8_t> report_desc_;

  std::vector<uint8_t> last_set_report_;
  uint8_t last_set_report_id_;

  fhidbus::wire::HidProtocol hid_protocol_ = fhidbus::wire::HidProtocol::kReport;
  fidl::Arena<> arena_;
  fhidbus::wire::HidInfo info_;
  zx_status_t start_status_ = ZX_OK;

 private:
  async::Loop loop_;
  fidl::ServerBindingGroup<fhidbus::Hidbus> binding_;
};

template <class FakeHidbus>
class HidDeviceTest : public zxtest::Test {
 public:
  HidDeviceTest() = default;
  void SetUp() override {
    // TODO(https://fxbug.dev/42075363): Migrate test to use dispatcher integration.
    fake_root_ = MockDevice::FakeRootParent();
    device_ = new HidDevice(fake_root_.get(), fake_hidbus_.GetClient());

    // Each test is responsible for calling Bind().
  }

  void TearDown() override {
    auto child = fake_root_->GetLatestChild();
    child->UnbindOp();
    EXPECT_EQ(ZX_OK, child->WaitUntilUnbindReplyCalled());
    EXPECT_TRUE(child->UnbindReplyCalled());
  }

  void SetupBootMouseDevice() {
    size_t desc_size;
    const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&desc_size);
    fake_hidbus_.SetDescriptor(boot_mouse_desc, desc_size);

    hid_info_t info = {};
    info.device_class = HID_DEVICE_CLASS_POINTER;
    info.boot_device = true;
    info.vendor_id = 0xabc;
    info.product_id = 123;
    info.version = 5;
    fake_hidbus_.SetHidInfo(info);
  }

  fbl::RefPtr<HidInstance> SetupInstanceDriver() {
    auto endpoints = fidl::Endpoints<fuchsia_hardware_input::Device>::Create();

    auto instance = device_->CreateInstance(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                            std::move(endpoints.server));
    EXPECT_OK(instance);

    sync_client_ = fidl::WireSyncClient(std::move(endpoints.client));

    RunSyncClientTask([&]() {
      auto result = sync_client_->GetReportsEvent();
      ASSERT_OK(result.status());
      ASSERT_OK(result.value().status);
      report_event_ = std::move(result.value().event);
    });

    return instance.is_ok() ? *instance : nullptr;
  }

  void ReadOneReport(uint8_t* report_data, size_t report_size, size_t* returned_size) {
    RunSyncClientTask([&]() {
      ASSERT_OK(report_event_.wait_one(DEV_STATE_READABLE, zx::time::infinite(), nullptr));

      auto result = sync_client_->ReadReport();
      ASSERT_OK(result.status());
      ASSERT_OK(result.value().status);
      ASSERT_TRUE(result.value().data.count() <= report_size);

      for (size_t i = 0; i < result.value().data.count(); i++) {
        report_data[i] = result.value().data[i];
      }
      *returned_size = result.value().data.count();
    });
  }

 protected:
  // Because this test is using a fidl::WireSyncClient, we need to run any ops on the client on
  // their own thread because the testing thread is shared with the fidl::Server<finput::Device>.
  static void RunSyncClientTask(fit::closure task) {
    async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
    loop.StartThread();
    zx::result result = fdf::RunOnDispatcherSync(loop.dispatcher(), std::move(task));
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  fidl::WireSyncClient<fuchsia_hardware_input::Device> sync_client_;
  zx::event report_event_;

  HidDevice* device_;

  std::shared_ptr<MockDevice> fake_root_;
  FakeHidbus fake_hidbus_;
};

class BanjoHidDeviceTest : public HidDeviceTest<BanjoFakeHidbus> {};
class FidlHidDeviceTest : public HidDeviceTest<FidlFakeHidbus> {};

#define TEST_BANJO_FIDL(TEST_NAME, TEST_BODY...)                                       \
  TEST_F(BanjoHidDeviceTest, TEST_NAME) TEST_BODY TEST_F(FidlHidDeviceTest, TEST_NAME) \
  TEST_BODY

TEST_BANJO_FIDL(LifeTimeTest, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());
})

TEST_BANJO_FIDL(TestQuery, {
  // Ids were chosen arbitrarily.
  constexpr uint16_t kVendorId = 0xacbd;
  constexpr uint16_t kProductId = 0xdcba;
  constexpr uint16_t kVersion = 0x1234;

  SetupBootMouseDevice();
  hid_info_t info = {};
  info.device_class = HID_DEVICE_CLASS_POINTER;
  info.boot_device = true;
  info.vendor_id = kVendorId;
  info.product_id = kProductId;
  info.version = kVersion;
  fake_hidbus_.SetHidInfo(info);

  ASSERT_OK(device_->Bind());

  SetupInstanceDriver();

  RunSyncClientTask([&]() {
    auto result = sync_client_->GetDeviceIds();
    ASSERT_OK(result.status());
    fuchsia_hardware_input::wire::DeviceIds ids = result.value().ids;

    ASSERT_EQ(kVendorId, ids.vendor_id);
    ASSERT_EQ(kProductId, ids.product_id);
    ASSERT_EQ(kVersion, ids.version);
  });
})

TEST_BANJO_FIDL(BootMouseSendReport, {
  SetupBootMouseDevice();
  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};
  ASSERT_OK(device_->Bind());

  SetupInstanceDriver();

  fake_hidbus_.SendReport(mouse_report, sizeof(mouse_report));

  uint8_t returned_report[3] = {};
  size_t actual;
  ReadOneReport(returned_report, sizeof(returned_report), &actual);

  ASSERT_EQ(actual, sizeof(returned_report));
  for (size_t i = 0; i < actual; i++) {
    ASSERT_EQ(returned_report[i], mouse_report[i]);
  }
})

TEST_BANJO_FIDL(BootMouseSendReportWithTime, {
  SetupBootMouseDevice();
  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};
  ASSERT_OK(device_->Bind());

  // Regsiter a device listener
  std::pair<sync_completion_t, zx_time_t> callback_data;
  callback_data.second = 0xabcd;

  hid_report_listener_protocol_ops_t listener_ops;
  listener_ops.receive_report = [](void* ctx, const uint8_t* report_list, size_t report_count,
                                   zx_time_t report_time) {
    auto callback_data = static_cast<std::pair<sync_completion_t, zx_time_t>*>(ctx);
    ASSERT_EQ(callback_data->second, report_time);
    sync_completion_signal(&callback_data->first);
  };
  hid_report_listener_protocol_t listener = {&listener_ops, &callback_data};
  device_->HidDeviceRegisterListener(&listener);

  fake_hidbus_.SendReportWithTime(mouse_report, sizeof(mouse_report), callback_data.second);
  RunSyncClientTask([&callback_data]() {
    sync_completion_wait_deadline(&callback_data.first, zx::time::infinite().get());
  });
  ASSERT_NO_FATAL_FAILURE();
})

TEST_BANJO_FIDL(BootMouseSendReportInPieces, {
  SetupBootMouseDevice();
  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};
  ASSERT_OK(device_->Bind());

  SetupInstanceDriver();

  fake_hidbus_.SendReport(&mouse_report[0], sizeof(uint8_t));
  fake_hidbus_.SendReport(&mouse_report[1], sizeof(uint8_t));
  fake_hidbus_.SendReport(&mouse_report[2], sizeof(uint8_t));

  uint8_t returned_report[3] = {};
  size_t actual;
  ReadOneReport(returned_report, sizeof(returned_report), &actual);

  ASSERT_EQ(actual, sizeof(returned_report));
  for (size_t i = 0; i < actual; i++) {
    ASSERT_EQ(returned_report[i], mouse_report[i]);
  }
})

TEST_BANJO_FIDL(BootMouseSendMultipleReports, {
  SetupBootMouseDevice();
  uint8_t double_mouse_report[] = {0xDE, 0xAD, 0xBE, 0x12, 0x34, 0x56};
  ASSERT_OK(device_->Bind());

  SetupInstanceDriver();

  fake_hidbus_.SendReport(double_mouse_report, sizeof(double_mouse_report));

  uint8_t returned_report[3] = {};
  size_t actual;

  // Read the first report.
  ReadOneReport(returned_report, sizeof(returned_report), &actual);
  ASSERT_EQ(actual, sizeof(returned_report));
  for (size_t i = 0; i < actual; i++) {
    ASSERT_EQ(returned_report[i], double_mouse_report[i]);
  }

  // Read the second report.
  ReadOneReport(returned_report, sizeof(returned_report), &actual);
  ASSERT_EQ(actual, sizeof(returned_report));
  for (size_t i = 0; i < actual; i++) {
    ASSERT_EQ(returned_report[i], double_mouse_report[i + 3]);
  }
})

TEST(HidDeviceTest, BanjoFailToRegister) {
  BanjoFakeHidbus fake_hidbus;
  auto fake_root = MockDevice::FakeRootParent();

  fake_hidbus.SetStartStatus(ZX_ERR_INTERNAL);
  HidDevice device(fake_root.get(), fake_hidbus.GetClient());

  ASSERT_EQ(device.Bind(), ZX_ERR_INTERNAL);
}

TEST(HidDeviceTest, FidlFailToRegister) {
  FidlFakeHidbus fake_hidbus;
  auto fake_root = MockDevice::FakeRootParent();

  fake_hidbus.SetHidInfo({});
  fake_hidbus.SetStartStatus(ZX_ERR_INTERNAL);
  HidDevice device(fake_root.get(), fake_hidbus.GetClient());

  ASSERT_EQ(device.Bind(), ZX_ERR_INTERNAL);
}

TEST_BANJO_FIDL(ReadReportSingleReport, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};

  SetupInstanceDriver();

  // Send the reports.
  zx_time_t time = 0xabcd;
  fake_hidbus_.SendReportWithTime(mouse_report, sizeof(mouse_report), time);

  RunSyncClientTask([&]() {
    auto result = sync_client_->ReadReport();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    ASSERT_EQ(time, result.value().time);
    ASSERT_EQ(sizeof(mouse_report), result.value().data.count());
    for (size_t i = 0; i < result.value().data.count(); i++) {
      EXPECT_EQ(mouse_report[i], result.value().data[i]);
    }
  });

  RunSyncClientTask([&]() {
    auto result = sync_client_->ReadReport();
    ASSERT_OK(result.status());
    ASSERT_EQ(result.value().status, ZX_ERR_SHOULD_WAIT);
  });
})

TEST_BANJO_FIDL(ReadReportDoubleReport, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  uint8_t double_mouse_report[] = {0xDE, 0xAD, 0xBE, 0x12, 0x34, 0x56};

  SetupInstanceDriver();

  // Send the reports.
  zx_time_t time = 0xabcd;
  fake_hidbus_.SendReportWithTime(double_mouse_report, sizeof(double_mouse_report), time);

  RunSyncClientTask([&]() {
    auto result = sync_client_->ReadReport();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    ASSERT_EQ(time, result.value().time);
    ASSERT_EQ(sizeof(hid_boot_mouse_report_t), result.value().data.count());
    for (size_t i = 0; i < result.value().data.count(); i++) {
      EXPECT_EQ(double_mouse_report[i], result.value().data[i]);
    }
  });

  RunSyncClientTask([&]() {
    auto result = sync_client_->ReadReport();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    ASSERT_EQ(time, result.value().time);
    ASSERT_EQ(sizeof(hid_boot_mouse_report_t), result.value().data.count());
    for (size_t i = 0; i < result.value().data.count(); i++) {
      EXPECT_EQ(double_mouse_report[i + sizeof(hid_boot_mouse_report_t)], result.value().data[i]);
    }
  });

  RunSyncClientTask([&]() {
    auto result = sync_client_->ReadReport();
    ASSERT_OK(result.status());
    ASSERT_EQ(result.value().status, ZX_ERR_SHOULD_WAIT);
  });
})

TEST_BANJO_FIDL(ReadReportsSingleReport, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};

  SetupInstanceDriver();

  // Send the reports.
  fake_hidbus_.SendReport(mouse_report, sizeof(mouse_report));

  RunSyncClientTask([&]() {
    auto result = sync_client_->ReadReports();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    ASSERT_EQ(sizeof(mouse_report), result.value().data.count());
    for (size_t i = 0; i < result.value().data.count(); i++) {
      EXPECT_EQ(mouse_report[i], result.value().data[i]);
    }
  });
})

TEST_BANJO_FIDL(ReadReportsDoubleReport, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  uint8_t double_mouse_report[] = {0xDE, 0xAD, 0xBE, 0x12, 0x34, 0x56};

  SetupInstanceDriver();

  // Send the reports.
  fake_hidbus_.SendReport(double_mouse_report, sizeof(double_mouse_report));

  RunSyncClientTask([&]() {
    auto result = sync_client_->ReadReports();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    ASSERT_EQ(sizeof(double_mouse_report), result.value().data.count());
    for (size_t i = 0; i < result.value().data.count(); i++) {
      EXPECT_EQ(double_mouse_report[i], result.value().data[i]);
    }
  });
})

TEST_BANJO_FIDL(ReadReportsBlockingWait, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  SetupInstanceDriver();

  // Send the reports, but delayed.
  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};
  std::thread report_thread([&]() {
    sleep(1);
    fake_hidbus_.SendReport(mouse_report, sizeof(mouse_report));
  });

  // Get the report.
  RunSyncClientTask([&]() {
    ASSERT_OK(report_event_.wait_one(DEV_STATE_READABLE, zx::time::infinite(), nullptr));

    auto result = sync_client_->ReadReports();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    ASSERT_EQ(sizeof(mouse_report), result.value().data.count());
    for (size_t i = 0; i < result.value().data.count(); i++) {
      EXPECT_EQ(mouse_report[i], result.value().data[i]);
    }

    report_thread.join();
  });
})

// Test that only whole reports get sent through.
TEST_BANJO_FIDL(ReadReportsOneAndAHalfReports, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  SetupInstanceDriver();

  // Send the report.
  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};
  fake_hidbus_.SendReport(mouse_report, sizeof(mouse_report));

  // Send a half of a report.
  uint8_t half_report[] = {0xDE, 0xAD};
  fake_hidbus_.SendReport(half_report, sizeof(half_report));

  RunSyncClientTask([&]() {
    auto result = sync_client_->ReadReports();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    ASSERT_EQ(sizeof(mouse_report), result.value().data.count());
    for (size_t i = 0; i < result.value().data.count(); i++) {
      EXPECT_EQ(mouse_report[i], result.value().data[i]);
    }
  });
})

// This tests that we can set the boot mode for a non-boot device, and that the device will
// have it's report descriptor set to the boot mode descriptor. For this, we will take an
// arbitrary descriptor and claim that it can be set to a boot-mode keyboard. We then
// test that the report descriptor we get back is for the boot keyboard.
// (The descriptor doesn't matter, as long as a device claims its a boot device it should
//  support this transformation in hardware).
TEST_BANJO_FIDL(SettingBootModeMouse, {
  size_t desc_size;
  const uint8_t* desc = get_paradise_touchpad_v1_report_desc(&desc_size);
  fake_hidbus_.SetDescriptor(desc, desc_size);

  hid_info_t info = {};
  info.device_class = HID_DEVICE_CLASS_POINTER;
  info.boot_device = true;
  fake_hidbus_.SetHidInfo(info);

  // Set the device to boot protocol.
  fake_hidbus_.SetProtocol(HID_PROTOCOL_BOOT);

  ASSERT_OK(device_->Bind());

  size_t boot_mouse_desc_size;
  const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&boot_mouse_desc_size);
  ASSERT_EQ(boot_mouse_desc_size, device_->GetReportDescLen());
  const uint8_t* received_desc = device_->GetReportDesc();
  for (size_t i = 0; i < boot_mouse_desc_size; i++) {
    ASSERT_EQ(boot_mouse_desc[i], received_desc[i]);
  }
})

// This tests that we can set the boot mode for a non-boot device, and that the device will
// have it's report descriptor set to the boot mode descriptor. For this, we will take an
// arbitrary descriptor and claim that it can be set to a boot-mode keyboard. We then
// test that the report descriptor we get back is for the boot keyboard.
// (The descriptor doesn't matter, as long as a device claims its a boot device it should
//  support this transformation in hardware).
TEST_BANJO_FIDL(SettingBootModeKbd, {
  size_t desc_size;
  const uint8_t* desc = get_paradise_touchpad_v1_report_desc(&desc_size);
  fake_hidbus_.SetDescriptor(desc, desc_size);

  hid_info_t info = {};
  info.device_class = HID_DEVICE_CLASS_KBD;
  info.boot_device = true;
  fake_hidbus_.SetHidInfo(info);

  // Set the device to boot protocol.
  fake_hidbus_.SetProtocol(HID_PROTOCOL_BOOT);

  ASSERT_OK(device_->Bind());

  size_t boot_kbd_desc_size;
  const uint8_t* boot_kbd_desc = get_boot_kbd_report_desc(&boot_kbd_desc_size);
  ASSERT_EQ(boot_kbd_desc_size, device_->GetReportDescLen());
  const uint8_t* received_desc = device_->GetReportDesc();
  for (size_t i = 0; i < boot_kbd_desc_size; i++) {
    ASSERT_EQ(boot_kbd_desc[i], received_desc[i]);
  }
})

TEST_BANJO_FIDL(GetHidDeviceInfo, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  hid_device_info_t info;
  device_->HidDeviceGetHidDeviceInfo(&info);

  auto hidbus_info = fake_hidbus_.Query();
  ASSERT_EQ(hidbus_info.vendor_id, info.vendor_id);
  ASSERT_EQ(hidbus_info.product_id, info.product_id);
  ASSERT_EQ(hidbus_info.version, info.version);
})

TEST_BANJO_FIDL(GetDescriptor, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  size_t known_size;
  const uint8_t* known_descriptor = get_boot_mouse_report_desc(&known_size);

  uint8_t report_descriptor[HID_MAX_DESC_LEN];
  size_t actual;
  ASSERT_OK(device_->HidDeviceGetDescriptor(report_descriptor, sizeof(report_descriptor), &actual));

  ASSERT_EQ(known_size, actual);
  ASSERT_BYTES_EQ(known_descriptor, report_descriptor, known_size);
})

TEST_BANJO_FIDL(RegisterListenerSendReport, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};

  struct ReportCtx {
    sync_completion_t* completion;
    uint8_t* known_report;
  };

  sync_completion_t seen_report;
  ReportCtx ctx;
  ctx.completion = &seen_report;
  ctx.known_report = mouse_report;

  hid_report_listener_protocol_ops_t ops;
  ops.receive_report = [](void* ctx, const uint8_t* report_list, size_t report_count,
                          zx_time_t time) {
    ASSERT_EQ(sizeof(mouse_report), report_count);
    auto report_ctx = reinterpret_cast<ReportCtx*>(ctx);
    ASSERT_BYTES_EQ(report_ctx->known_report, report_list, report_count);
    sync_completion_signal(report_ctx->completion);
  };

  hid_report_listener_protocol_t listener;
  listener.ctx = &ctx;
  listener.ops = &ops;

  ASSERT_OK(device_->HidDeviceRegisterListener(&listener));

  fake_hidbus_.SendReport(mouse_report, sizeof(mouse_report));

  RunSyncClientTask([&seen_report]() {
    ASSERT_OK(sync_completion_wait(&seen_report, zx::time::infinite().get()));
  });
  device_->HidDeviceUnregisterListener();
})

TEST_BANJO_FIDL(GetSetReport, {
  const uint8_t* desc;
  size_t desc_size = get_ambient_light_report_desc(&desc);
  fake_hidbus_.SetDescriptor(desc, desc_size);

  hid_info_t info = {};
  info.device_class = HID_DEVICE_CLASS_OTHER;
  info.boot_device = false;
  fake_hidbus_.SetHidInfo(info);

  ASSERT_OK(device_->Bind());

  ambient_light_feature_rpt_t feature_report = {};
  feature_report.rpt_id = AMBIENT_LIGHT_RPT_ID_FEATURE;
  // Below value are chosen arbitrarily.
  feature_report.state = 100;
  feature_report.interval_ms = 50;
  feature_report.threshold_high = 40;
  feature_report.threshold_low = 10;

  ASSERT_OK(device_->HidDeviceSetReport(HID_REPORT_TYPE_FEATURE, AMBIENT_LIGHT_RPT_ID_FEATURE,
                                        reinterpret_cast<uint8_t*>(&feature_report),
                                        sizeof(feature_report)));

  ambient_light_feature_rpt_t received_report = {};
  size_t actual;
  ASSERT_OK(device_->HidDeviceGetReport(HID_REPORT_TYPE_FEATURE, AMBIENT_LIGHT_RPT_ID_FEATURE,
                                        reinterpret_cast<uint8_t*>(&received_report),
                                        sizeof(received_report), &actual));

  ASSERT_EQ(sizeof(received_report), actual);
  ASSERT_BYTES_EQ(&feature_report, &received_report, actual);
})

// Tests that a device with too large reports don't cause buffer overruns.
TEST_BANJO_FIDL(GetReportBufferOverrun, {
  const uint8_t desc[] = {
      0x05, 0x01,                    // Usage Page (Generic Desktop Ctrls)
      0x09, 0x02,                    // Usage (Mouse)
      0xA1, 0x01,                    // Collection (Application)
      0x05, 0x09,                    //   Usage Page (Button)
      0x09, 0x30,                    //   Usage (0x30)
      0x97, 0x00, 0xF0, 0x00, 0x00,  //   Report Count (65279)
      0x75, 0x08,                    //   Report Size (8)
      0x25, 0x01,                    //   Logical Maximum (1)
      0x81, 0x02,  //   Input (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position)
      0xC0,        // End Collection

      // 22 bytes
  };
  size_t desc_size = sizeof(desc);
  fake_hidbus_.SetDescriptor(desc, desc_size);

  hid_info_t info = {};
  info.device_class = HID_DEVICE_CLASS_OTHER;
  info.boot_device = false;
  fake_hidbus_.SetHidInfo(info);

  ASSERT_OK(device_->Bind());

  std::vector<uint8_t> report(0xFF0000);
  size_t actual;
  ASSERT_EQ(
      device_->HidDeviceGetReport(HID_REPORT_TYPE_INPUT, 0, report.data(), report.size(), &actual),
      ZX_ERR_INTERNAL);
})

TEST_BANJO_FIDL(DeviceReportReaderSingleReport, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};

  SetupInstanceDriver();

  fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader> reader;
  {
    auto endpoints = fidl::Endpoints<fuchsia_hardware_input::DeviceReportsReader>::Create();

    RunSyncClientTask([&]() {
      auto result = sync_client_->GetDeviceReportsReader(std::move(endpoints.server));
      ASSERT_OK(result.status());
      reader = fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader>(
          std::move(endpoints.client));
    });
  }

  // Send the reports.
  fake_hidbus_.SendReport(mouse_report, sizeof(mouse_report));

  RunSyncClientTask([&]() {
    auto response = reader->ReadReports();
    ASSERT_OK(response.status());
    ASSERT_FALSE(response->is_error());
    auto result = response->value();
    ASSERT_EQ(result->reports.count(), 1);
    ASSERT_EQ(result->reports[0].data.count(), sizeof(mouse_report));
    for (size_t i = 0; i < result->reports[0].data.count(); i++) {
      EXPECT_EQ(mouse_report[i], result->reports[0].data[i]);
    }
  });
})

TEST_BANJO_FIDL(DeviceReportReaderDoubleReport, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};
  uint8_t mouse_report_two[] = {0xDE, 0xAD, 0xBE};

  auto instance = SetupInstanceDriver();

  fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader> reader;
  {
    auto endpoints = fidl::Endpoints<fuchsia_hardware_input::DeviceReportsReader>::Create();

    RunSyncClientTask([&]() {
      auto result = sync_client_->GetDeviceReportsReader(std::move(endpoints.server));
      ASSERT_OK(result.status());
      reader = fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader>(
          std::move(endpoints.client));
    });
  }

  // Send the reports.
  // Note: testing_write_to_fifo_called_ ensures that we wait for both reports to be written as
  // ReadReports only waits for one.
  // TODO(b/341791565): Refactor tests so that testing_write_to_fifo_called_ is not needed.
  fake_hidbus_.SendReport(mouse_report, sizeof(mouse_report));
  RunSyncClientTask([&instance]() {
    instance->testing_write_to_fifo_called_.Wait();
    instance->testing_write_to_fifo_called_.Reset();
  });
  fake_hidbus_.SendReport(mouse_report_two, sizeof(mouse_report_two));
  RunSyncClientTask([&instance]() { instance->testing_write_to_fifo_called_.Wait(); });

  RunSyncClientTask([&]() {
    auto response = reader->ReadReports();
    ASSERT_OK(response.status());
    ASSERT_FALSE(response->is_error());
    auto result = response->value();
    ASSERT_EQ(result->reports.count(), 2);
    ASSERT_EQ(result->reports[0].data.count(), sizeof(mouse_report));
    for (size_t i = 0; i < result->reports[0].data.count(); i++) {
      EXPECT_EQ(mouse_report[i], result->reports[0].data[i]);
    }
    ASSERT_EQ(result->reports[1].data.count(), sizeof(mouse_report));
    for (size_t i = 0; i < result->reports[1].data.count(); i++) {
      EXPECT_EQ(mouse_report[i], result->reports[1].data[i]);
    }
  });
})

TEST_BANJO_FIDL(DeviceReportReaderTwoClients, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};

  SetupInstanceDriver();

  fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader> reader1;
  fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader> reader2;
  {
    auto endpoints1 = fidl::Endpoints<fuchsia_hardware_input::DeviceReportsReader>::Create();

    RunSyncClientTask([&]() {
      auto result1 = sync_client_->GetDeviceReportsReader(std::move(endpoints1.server));
      ASSERT_OK(result1.status());
      reader1 = fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader>(
          std::move(endpoints1.client));
    });

    auto endpoints2 = fidl::Endpoints<fuchsia_hardware_input::DeviceReportsReader>::Create();

    RunSyncClientTask([&]() {
      auto result2 = sync_client_->GetDeviceReportsReader(std::move(endpoints2.server));
      ASSERT_OK(result2.status());
      reader2 = fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader>(
          std::move(endpoints2.client));
    });
  }

  // Send the report.
  fake_hidbus_.SendReport(mouse_report, sizeof(mouse_report));

  RunSyncClientTask([&]() {
    auto response = reader1->ReadReports();
    ASSERT_OK(response.status());
    ASSERT_FALSE(response->is_error());
    auto result = response->value();
    ASSERT_EQ(result->reports.count(), 1);
    ASSERT_EQ(result->reports[0].data.count(), sizeof(mouse_report));
    for (size_t i = 0; i < result->reports[0].data.count(); i++) {
      EXPECT_EQ(mouse_report[i], result->reports[0].data[i]);
    }
  });

  RunSyncClientTask([&]() {
    auto response = reader2->ReadReports();
    ASSERT_OK(response.status());
    ASSERT_FALSE(response->is_error());
    auto result = response->value();
    ASSERT_EQ(result->reports.count(), 1);
    ASSERT_EQ(result->reports[0].data.count(), sizeof(mouse_report));
    for (size_t i = 0; i < result->reports[0].data.count(); i++) {
      EXPECT_EQ(mouse_report[i], result->reports[0].data[i]);
    }
  });
})

// Test that only whole reports get sent through.
TEST_BANJO_FIDL(DeviceReportReaderOneAndAHalfReports, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  SetupInstanceDriver();

  fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader> reader;
  {
    auto endpoints = fidl::Endpoints<fuchsia_hardware_input::DeviceReportsReader>::Create();

    RunSyncClientTask([&]() {
      auto result = sync_client_->GetDeviceReportsReader(std::move(endpoints.server));
      ASSERT_OK(result.status());
      reader = fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader>(
          std::move(endpoints.client));
    });
  }

  // Send the report.
  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};
  fake_hidbus_.SendReport(mouse_report, sizeof(mouse_report));

  // Send a half of a report.
  uint8_t half_report[] = {0xDE, 0xAD};
  fake_hidbus_.SendReport(half_report, sizeof(half_report));

  RunSyncClientTask([&]() {
    auto response = reader->ReadReports();
    ASSERT_OK(response.status());
    ASSERT_FALSE(response->is_error());
    auto result = response->value();
    ASSERT_EQ(result->reports.count(), 1);
    ASSERT_EQ(sizeof(mouse_report), result->reports[0].data.count());
    for (size_t i = 0; i < result->reports[0].data.count(); i++) {
      EXPECT_EQ(mouse_report[i], result->reports[0].data[i]);
    }
  });
})

TEST_BANJO_FIDL(DeviceReportReaderHangingGet, {
  SetupBootMouseDevice();
  ASSERT_OK(device_->Bind());

  uint8_t mouse_report[] = {0xDE, 0xAD, 0xBE};

  SetupInstanceDriver();

  fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader> reader;
  {
    auto endpoints = fidl::Endpoints<fuchsia_hardware_input::DeviceReportsReader>::Create();

    RunSyncClientTask([&]() {
      auto result = sync_client_->GetDeviceReportsReader(std::move(endpoints.server));
      ASSERT_OK(result.status());
      reader = fidl::WireSyncClient<fuchsia_hardware_input::DeviceReportsReader>(
          std::move(endpoints.client));
    });
  }

  // Send the reports, but delayed.
  std::thread report_thread([&]() {
    sleep(1);
    fake_hidbus_.SendReport(mouse_report, sizeof(mouse_report));
  });

  RunSyncClientTask([&]() {
    auto response = reader->ReadReports();
    ASSERT_OK(response.status());
    ASSERT_FALSE(response->is_error());
    auto result = response->value();
    ASSERT_EQ(result->reports.count(), 1);
    ASSERT_EQ(result->reports[0].data.count(), sizeof(mouse_report));
    for (size_t i = 0; i < result->reports[0].data.count(); i++) {
      EXPECT_EQ(mouse_report[i], result->reports[0].data[i]);
    }

    report_thread.join();
  });
})

}  // namespace hid_driver
