// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c-hid.h"

#include <endian.h>
#include <fidl/fuchsia.hardware.acpi/cpp/wire.h>
#include <fidl/fuchsia.hardware.interrupt/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async_patterns/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/trace/event.h>
#include <lib/device-protocol/i2c-channel.h>
#include <lib/fake-i2c/fake-i2c.h>
#include <lib/sync/completion.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/rights.h>
#include <zircon/types.h>

#include <vector>

#include <fbl/auto_lock.h>
#include <zxtest/zxtest.h>

#include "src/devices/lib/acpi/mock/mock-acpi.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace i2c_hid {

namespace fhidbus = fuchsia_hardware_hidbus;

// Ids were chosen arbitrarily.
static constexpr uint16_t kHidVendorId = 0xabcd;
static constexpr uint16_t kHidProductId = 0xdcba;
static constexpr uint16_t kHidVersion = 0x0123;

class FakeI2cHid : public fake_i2c::FakeI2c {
 public:
  // Sets the report descriptor. Must be called before binding the driver because
  // the driver reads |hiddesc_| on bind.
  void SetReportDescriptor(std::vector<uint8_t> report_desc) {
    fbl::AutoLock lock(&report_read_lock_);
    hiddesc_.wReportDescLength = htole16(report_desc.size());
    report_desc_ = std::move(report_desc);
  }

  // Calling this function will make the FakeI2cHid driver return an error when the I2cHidbus
  // tries to read the HidDescriptor. This so we can test that the I2cHidbus driver shuts
  // down correctly when it fails to read the HidDescriptor.
  void SetHidDescriptorFailure(zx_status_t status) { hiddesc_status_ = status; }

  void SendReport(std::vector<uint8_t> report) {
    SendReportWithLength(report, report.size() + sizeof(uint16_t));
  }

  // This lets us send a report with an incorrect length.
  void SendReportWithLength(std::vector<uint8_t> report, size_t len) {
    {
      fbl::AutoLock lock(&report_read_lock_);

      report_ = std::move(report);
      report_len_ = len;
      irq_.trigger(0, zx::clock::get_boot());
    }
    mock_ddk::GetDriverRuntime()->PerformBlockingWork(
        [this]() { sync_completion_wait_deadline(&report_read_, zx::time::infinite().get()); });
    sync_completion_reset(&report_read_);
  }

  void WaitUntilReset() {
    mock_ddk::GetDriverRuntime()->PerformBlockingWork([this]() {
      ASSERT_OK(sync_completion_wait_deadline(&is_reset_, zx::time::infinite().get()));
    });
  }

 private:
  zx_status_t TransactCommands(const uint8_t* write_buffer, size_t write_buffer_size,
                               uint8_t* read_buffer, size_t* read_buffer_size)
      __TA_REQUIRES(report_read_lock_) {
    if (write_buffer_size < 4) {
      return ZX_ERR_INTERNAL;
    }

    // Reset Command.
    if (write_buffer[3] == kResetCommand) {
      *read_buffer_size = 0;
      pending_reset_ = true;
      irq_.trigger(0, zx::clock::get_boot());
      return ZX_OK;
    }

    // Set Command. At the moment this fake doesn't test for the types of reports, we only ever
    // get/set |report_|.
    if (write_buffer[3] == kSetReportCommand) {
      if (write_buffer_size < 6) {
        return ZX_ERR_INTERNAL;
      }
      // Get the report size.
      uint16_t report_size = static_cast<uint16_t>(write_buffer[6] + (write_buffer[7] << 8));
      if (write_buffer_size < (6 + report_size)) {
        return ZX_ERR_INTERNAL;
      }
      // The report size includes the 2 bytes for the size, which we don't want when setting
      // the report.
      report_.resize(report_size - 2);
      memcpy(report_.data(), write_buffer + 8, report_.size());
      return ZX_OK;
    }

    // Get Command.
    if (write_buffer[3] == kGetReportCommand) {
      // Set the report size as the first two bytes.
      read_buffer[0] = static_cast<uint8_t>((report_.size() + 2) & 0xFF);
      read_buffer[1] = static_cast<uint8_t>(((report_.size() + 2) >> 8) & 0xFF);

      memcpy(read_buffer + 2, report_.data(), report_.size());
      *read_buffer_size = report_.size() + 2;
      return ZX_OK;
    }
    return ZX_ERR_INTERNAL;
  }

  zx_status_t Transact(const uint8_t* write_buffer, size_t write_buffer_size, uint8_t* read_buffer,
                       size_t* read_buffer_size) override {
    fbl::AutoLock lock(&report_read_lock_);
    // General Read.
    if (write_buffer_size == 0) {
      // Reading the Reset status.
      if (pending_reset_) {
        SetRead(kResetReport, sizeof(kResetReport), read_buffer, read_buffer_size);
        pending_reset_ = false;
        sync_completion_signal(&is_reset_);
        return ZX_OK;
      }
      // First two bytes are the report length.
      *reinterpret_cast<uint16_t*>(read_buffer) = htole16(report_len_);

      memcpy(read_buffer + sizeof(uint16_t), report_.data(), report_.size());
      *read_buffer_size = report_.size() + sizeof(uint16_t);
      sync_completion_signal(&report_read_);
      return ZX_OK;
    }
    // Reading the Hid descriptor.
    if (CompareWrite(write_buffer, write_buffer_size, kHidDescCommand, sizeof(kHidDescCommand))) {
      if (hiddesc_status_ != ZX_OK) {
        return hiddesc_status_;
      }
      SetRead(&hiddesc_, sizeof(hiddesc_), read_buffer, read_buffer_size);
      return ZX_OK;
    }
    // Reading the Hid Report descriptor.
    if (CompareWrite(write_buffer, write_buffer_size,
                     reinterpret_cast<const uint8_t*>(&kReportDescRegister),
                     sizeof(kReportDescRegister))) {
      SetRead(report_desc_.data(), report_desc_.size(), read_buffer, read_buffer_size);
      return ZX_OK;
    }

    // General commands.
    bool is_general_command = (write_buffer_size >= 2) && (write_buffer[0] == kHidCommand[0]) &&
                              (write_buffer[1] == kHidCommand[1]);
    if (is_general_command) {
      return TransactCommands(write_buffer, write_buffer_size, read_buffer, read_buffer_size);
    }
    return ZX_ERR_INTERNAL;
  }

  // Register values were just picked arbitrarily.
  static constexpr uint16_t kInputRegister = htole16(0x5);
  static constexpr uint16_t kOutputRegister = htole16(0x6);
  static constexpr uint16_t kCommandRegister = htole16(0x7);
  static constexpr uint16_t kDataRegister = htole16(0x8);
  static constexpr uint16_t kReportDescRegister = htole16(0x9);

  static constexpr uint16_t kMaxInputLength = 0x1000;

  static constexpr uint8_t kResetReport[2] = {0, 0};
  static constexpr uint8_t kHidDescCommand[2] = {0x01, 0x00};
  static constexpr uint8_t kHidCommand[2] = {static_cast<uint8_t>(kCommandRegister & 0xff),
                                             static_cast<uint8_t>(kCommandRegister >> 8)};

  I2cHidDesc hiddesc_ = []() {
    I2cHidDesc hiddesc = {};
    hiddesc.wHIDDescLength = htole16(sizeof(I2cHidDesc));
    hiddesc.wInputRegister = kInputRegister;
    hiddesc.wOutputRegister = kOutputRegister;
    hiddesc.wCommandRegister = kCommandRegister;
    hiddesc.wDataRegister = kDataRegister;
    hiddesc.wMaxInputLength = kMaxInputLength;
    hiddesc.wReportDescRegister = kReportDescRegister;
    hiddesc.wVendorID = htole16(kHidVendorId);
    hiddesc.wProductID = htole16(kHidProductId);
    hiddesc.wVersionID = htole16(kHidVersion);
    return hiddesc;
  }();
  zx_status_t hiddesc_status_ = ZX_OK;

  std::atomic<bool> pending_reset_ = false;
  sync_completion_t is_reset_;

  fbl::Mutex report_read_lock_;
  sync_completion_t report_read_;
  std::vector<uint8_t> report_desc_ __TA_GUARDED(report_read_lock_);
  std::vector<uint8_t> report_ __TA_GUARDED(report_read_lock_);
  size_t report_len_ __TA_GUARDED(report_read_lock_) = 0;
};

class I2cHidTest : public zxtest::Test {
 public:
  class InterruptServer : public fidl::WireServer<fuchsia_hardware_interrupt::Provider> {
   public:
    explicit InterruptServer(zx::interrupt irq) : irq_(std::move(irq)) {}

    void ResetIrq() { irq_.reset(); }

    // Creates a handler function that can be used to connect to this server.
    fidl::ProtocolHandler<::fuchsia_hardware_interrupt::Provider> Publish() {
      return bindings_.CreateHandler(this, async_get_default_dispatcher(),
                                     fidl::kIgnoreBindingClosure);
    }

   private:
    void Get(GetCompleter::Sync& completer) override {
      zx::interrupt clone;
      ASSERT_OK(irq_.duplicate(ZX_RIGHT_SAME_RIGHTS, &clone));
      completer.ReplySuccess(std::move(clone));
    }

    fidl::ServerBindingGroup<fuchsia_hardware_interrupt::Provider> bindings_;
    zx::interrupt irq_;
  };

  // A mock component that exposes `fuchsia.hardware.interrupt/Provider`.
  class MockComponent {
   public:
    MockComponent(zx::interrupt irq, fidl::ServerEnd<fuchsia_io::Directory> outgoing)
        : outgoing_(async_get_default_dispatcher()), server_(std::move(irq)) {
      ASSERT_OK(outgoing_
                    .AddService<fuchsia_hardware_interrupt::Service>(
                        fuchsia_hardware_interrupt::Service::InstanceHandler{{
                            .provider = server_.Publish(),
                        }})
                    .status_value());
      ASSERT_OK(outgoing_.Serve(std::move(outgoing)));
    }

    void ResetIrq() { server_.ResetIrq(); }

   private:
    component::OutgoingDirectory outgoing_;
    InterruptServer server_;
  };

  I2cHidTest() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {}
  void SetUp() override {
    parent_ = MockDevice::FakeRootParent();

    ASSERT_OK(loop_.StartThread("i2c-hid-test-thread"));

    acpi_device_.SetEvaluateObject(
        [](acpi::mock::Device::EvaluateObjectRequestView view,
           acpi::mock::Device::EvaluateObjectCompleter::Sync& completer) {
          fidl::Arena<> alloc;

          auto encoded = fuchsia_hardware_acpi::wire::EncodedObject::WithObject(
              alloc, fuchsia_hardware_acpi::wire::Object::WithIntegerVal(alloc, 0x01));

          completer.ReplySuccess(std::move(encoded));
          ASSERT_TRUE(completer.result_of_reply().ok());
        });

    zx::interrupt irq;
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq));

    zx::interrupt interrupt;
    ASSERT_OK(irq.duplicate(ZX_RIGHT_SAME_RIGHTS, &interrupt));
    fake_i2c_hid_.SetInterrupt(std::move(interrupt));

    auto io_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

    mock_component_.emplace(std::move(irq), std::move(io_endpoints.server));

    parent_->AddFidlService(fuchsia_hardware_interrupt::Service::Name,
                            std::move(io_endpoints.client), "irq001");

    auto client = acpi_device_.CreateClient(loop_.dispatcher());
    ASSERT_OK(client.status_value());
    device_ = new I2cHidbus(parent_.get(), std::move(client.value()));

    auto endpoints = fidl::Endpoints<fuchsia_hardware_i2c::Device>::Create();

    fidl::BindServer(loop_.dispatcher(), std::move(endpoints.server), &fake_i2c_hid_);

    i2c_ = std::move(endpoints.client);
    // Each test is responsible for calling Bind().
  }

  void TearDown() override {
    device_->DdkAsyncRemove();

    EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(device_->zxdev()));
  }

  fidl::WireSyncClient<fhidbus::Hidbus> ConnectFidl() {
    auto [client, server] = fidl::Endpoints<fhidbus::Hidbus>::Create();
    device_->binding().emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server),
                               device_, fidl::kIgnoreBindingClosure);
    return fidl::WireSyncClient<fhidbus::Hidbus>(std::move(client));
  }

  class HidbusEventHandler : public fidl::WireSyncEventHandler<fhidbus::Hidbus> {
   public:
    void OnReportReceived(fidl::WireEvent<fhidbus::Hidbus::OnReportReceived>* event) override {
      ASSERT_FALSE(expected_reports_.empty());
      auto expected = std::move(expected_reports_.front());
      expected_reports_.pop();
      auto report = event->buf();
      ASSERT_EQ(report.count(), expected.size());
      for (size_t i = 0; i < report.count(); i++) {
        EXPECT_EQ(report[i], expected[i]);
      }
    }

    void ExpectReport(std::vector<uint8_t>& report) { expected_reports_.push(report); }

   private:
    std::queue<std::vector<uint8_t>> expected_reports_;
  };

 protected:
  acpi::mock::Device acpi_device_;
  I2cHidbus* device_;
  std::shared_ptr<MockDevice> parent_;
  FakeI2cHid fake_i2c_hid_;
  fidl::ClientEnd<fuchsia_hardware_i2c::Device> i2c_;
  async::Loop loop_;
  async_patterns::DispatcherBound<MockComponent> mock_component_{loop_.dispatcher()};
};

TEST_F(I2cHidTest, HidTestBind) {
  ASSERT_OK(device_->Bind(std::move(i2c_)));
  device_->zxdev()->InitOp();
  ASSERT_OK(device_->zxdev()->WaitUntilInitReplyCalled(zx::time::infinite()));
  EXPECT_OK(device_->zxdev()->InitReplyCallStatus());
}

TEST_F(I2cHidTest, HidTestQuery) {
  ASSERT_OK(device_->Bind(std::move(i2c_)));
  device_->zxdev()->InitOp();
  fake_i2c_hid_.WaitUntilReset();

  auto hidbus = ConnectFidl();
  mock_ddk::GetDriverRuntime()->PerformBlockingWork([&hidbus]() {
    {
      auto result = hidbus->Start();
      ASSERT_TRUE(result.ok());
      ASSERT_TRUE(result->is_ok());
    }

    {
      auto result = hidbus->Query();
      ASSERT_TRUE(result.ok());
      ASSERT_TRUE(result->is_ok());
      auto info = result->value()->info;
      ASSERT_EQ(kHidVendorId, info.vendor_id());
      ASSERT_EQ(kHidProductId, info.product_id());
      ASSERT_EQ(kHidVersion, info.version());
    }
  });
}

TEST_F(I2cHidTest, HidTestReadReportDesc) {
  std::vector<uint8_t> report_desc(4);
  report_desc[0] = 1;
  report_desc[1] = 100;
  report_desc[2] = 255;
  report_desc[3] = 5;

  fake_i2c_hid_.SetReportDescriptor(report_desc);
  ASSERT_OK(device_->Bind(std::move(i2c_)));
  device_->zxdev()->InitOp();

  auto hidbus = ConnectFidl();
  mock_ddk::GetDriverRuntime()->PerformBlockingWork([&hidbus, &report_desc]() {
    auto result = hidbus->GetDescriptor(fhidbus::wire::HidDescriptorType::kReport);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    auto report = result->value()->data;
    ASSERT_EQ(report.count(), report_desc.size());
    for (size_t i = 0; i < report.count(); i++) {
      ASSERT_EQ(report[i], report_desc[i]);
    }
  });
}

TEST(I2cHidTest, HidTestReportDescFailureLifetimeTest) {
  I2cHidbus* device_;
  std::shared_ptr<MockDevice> parent = MockDevice::FakeRootParent();
  FakeI2cHid fake_i2c_hid_;
  ddk::I2cChannel channel_;

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  EXPECT_OK(loop.StartThread());

  auto i2c_endpoints = fidl::Endpoints<fuchsia_hardware_i2c::Device>::Create();

  fidl::BindServer(loop.dispatcher(), std::move(i2c_endpoints.server), &fake_i2c_hid_);

  auto endpoints = fidl::Endpoints<fuchsia_hardware_acpi::Device>::Create();
  endpoints.server.reset();
  device_ = new I2cHidbus(parent.get(),
                          acpi::Client::Create(fidl::WireSyncClient(std::move(endpoints.client))));
  channel_ = ddk::I2cChannel(std::move(i2c_endpoints.client));

  fake_i2c_hid_.SetHidDescriptorFailure(ZX_ERR_TIMED_OUT);
  ASSERT_OK(device_->Bind(std::move(channel_)));

  device_->zxdev()->InitOp();

  EXPECT_OK(device_->zxdev()->WaitUntilInitReplyCalled(zx::time::infinite()));
  EXPECT_NOT_OK(device_->zxdev()->InitReplyCallStatus());

  loop.Shutdown();

  device_async_remove(parent.get());
  mock_ddk::ReleaseFlaggedDevices(parent.get());
}

TEST_F(I2cHidTest, HidTestReadReport) {
  ASSERT_OK(device_->Bind(std::move(i2c_)));
  device_->zxdev()->InitOp();
  fake_i2c_hid_.WaitUntilReset();

  auto hidbus = ConnectFidl();
  mock_ddk::GetDriverRuntime()->PerformBlockingWork([&hidbus]() {
    auto result = hidbus->Start();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  });

  // Any arbitrary values or vector length could be used here.
  std::vector<uint8_t> rpt(4);
  rpt[0] = 1;
  rpt[1] = 100;
  rpt[2] = 255;
  rpt[3] = 5;
  fake_i2c_hid_.SendReport(rpt);

  HidbusEventHandler event_handler;
  event_handler.ExpectReport(rpt);
  mock_ddk::GetDriverRuntime()->PerformBlockingWork(
      [&hidbus, &event_handler]() { ASSERT_OK(hidbus.HandleOneEvent(event_handler)); });
}

TEST_F(I2cHidTest, HidTestBadReportLen) {
  ASSERT_OK(device_->Bind(std::move(i2c_)));
  device_->zxdev()->InitOp();
  fake_i2c_hid_.WaitUntilReset();

  auto hidbus = ConnectFidl();
  mock_ddk::GetDriverRuntime()->PerformBlockingWork([&hidbus]() {
    auto result = hidbus->Start();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  });

  // Send a report with a length that's too long.
  std::vector<uint8_t> too_long_rpt{0xAA};
  fake_i2c_hid_.SendReportWithLength(too_long_rpt, UINT16_MAX);

  // Send a normal report.
  std::vector<uint8_t> normal_rpt{0xBB};
  fake_i2c_hid_.SendReport(normal_rpt);

  HidbusEventHandler event_handler;
  // We should've only seen one report since the too long report will cause an error.
  // Double check that the returned report is the normal one.
  event_handler.ExpectReport(normal_rpt);
  mock_ddk::GetDriverRuntime()->PerformBlockingWork(
      [&hidbus, &event_handler]() { ASSERT_OK(hidbus.HandleOneEvent(event_handler)); });
}

TEST_F(I2cHidTest, HidTestReadReportNoIrq) {
  // Replace the device's interrupt with an invalid one.
  fake_i2c_hid_.SetInterrupt(zx::interrupt());
  mock_component_.AsyncCall(&MockComponent::ResetIrq);

  ASSERT_OK(device_->Bind(std::move(i2c_)));
  device_->zxdev()->InitOp();
  fake_i2c_hid_.WaitUntilReset();

  auto hidbus = ConnectFidl();
  mock_ddk::GetDriverRuntime()->PerformBlockingWork([&hidbus]() {
    auto result = hidbus->Start();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  });

  // Any arbitrary values or vector length could be used here.
  std::vector<uint8_t> rpt(4);
  rpt[0] = 1;
  rpt[1] = 100;
  rpt[2] = 255;
  rpt[3] = 5;
  fake_i2c_hid_.SendReport(rpt);

  HidbusEventHandler event_handler;
  event_handler.ExpectReport(rpt);
  mock_ddk::GetDriverRuntime()->PerformBlockingWork(
      [&hidbus, &event_handler]() { ASSERT_OK(hidbus.HandleOneEvent(event_handler)); });
}

TEST_F(I2cHidTest, HidTestDedupeReportsNoIrq) {
  // Replace the device's interrupt with an invalid one.
  fake_i2c_hid_.SetInterrupt(zx::interrupt());
  mock_component_.AsyncCall(&MockComponent::ResetIrq);

  ASSERT_OK(device_->Bind(std::move(i2c_)));
  device_->zxdev()->InitOp();
  fake_i2c_hid_.WaitUntilReset();

  auto hidbus = ConnectFidl();
  mock_ddk::GetDriverRuntime()->PerformBlockingWork([&hidbus]() {
    auto result = hidbus->Start();
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  });

  // Send three reports.
  std::vector<uint8_t> rpt1(4);
  rpt1[0] = 1;
  rpt1[1] = 100;
  rpt1[2] = 255;
  rpt1[3] = 5;
  fake_i2c_hid_.SendReport(rpt1);
  fake_i2c_hid_.SendReport(rpt1);
  fake_i2c_hid_.SendReport(rpt1);

  HidbusEventHandler event_handler;
  // We should've only seen one report since the repeats should have been deduped.
  event_handler.ExpectReport(rpt1);
  mock_ddk::GetDriverRuntime()->PerformBlockingWork(
      [&hidbus, &event_handler]() { ASSERT_OK(hidbus.HandleOneEvent(event_handler)); });

  // Send three different reports.
  std::vector<uint8_t> rpt2(4);
  rpt2[0] = 1;
  rpt2[1] = 200;
  rpt2[2] = 100;
  rpt2[3] = 6;
  fake_i2c_hid_.SendReport(rpt2);
  fake_i2c_hid_.SendReport(rpt2);
  fake_i2c_hid_.SendReport(rpt2);

  // We should've only seen one more report since the repeats should have been deduped.
  event_handler.ExpectReport(rpt2);
  mock_ddk::GetDriverRuntime()->PerformBlockingWork(
      [&hidbus, &event_handler]() { ASSERT_OK(hidbus.HandleOneEvent(event_handler)); });

  // Send a report with different length.
  std::vector<uint8_t> rpt3(5);
  rpt3[0] = 1;
  rpt3[1] = 200;
  rpt3[2] = 100;
  rpt3[3] = 6;
  rpt3[4] = 10;
  fake_i2c_hid_.SendReport(rpt3);

  event_handler.ExpectReport(rpt3);
  mock_ddk::GetDriverRuntime()->PerformBlockingWork(
      [&hidbus, &event_handler]() { ASSERT_OK(hidbus.HandleOneEvent(event_handler)); });
}

TEST_F(I2cHidTest, HidTestSetReport) {
  ASSERT_OK(device_->Bind(std::move(i2c_)));
  device_->zxdev()->InitOp();
  fake_i2c_hid_.WaitUntilReset();

  auto hidbus = ConnectFidl();
  mock_ddk::GetDriverRuntime()->PerformBlockingWork([&hidbus]() {
    // Any arbitrary values or vector length could be used here.
    uint8_t report_data[4] = {1, 100, 255, 5};

    {
      auto result = hidbus->SetReport(
          fhidbus::wire::ReportType::kFeature, 0x1,
          fidl::VectorView<uint8_t>::FromExternal(report_data, sizeof(report_data)));
      ASSERT_TRUE(result.ok());
      ASSERT_TRUE(result->is_ok());
    }

    {
      auto result = hidbus->GetReport(fhidbus::wire::ReportType::kFeature, 0x1, 4);
      ASSERT_TRUE(result.ok());
      ASSERT_TRUE(result->is_ok());
      auto report = result->value()->data;
      ASSERT_EQ(report.count(), 4);
      for (size_t i = 0; i < report.count(); i++) {
        EXPECT_EQ(report[i], report_data[i]);
      }
    }
  });
}

}  // namespace i2c_hid
