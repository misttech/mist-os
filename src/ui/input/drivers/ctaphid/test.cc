// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.fido.report/cpp/wire.h>
#include <fidl/fuchsia.hardware.input/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/sys/cpp/component_context.h>
#include <zircon/errors.h>

#include <src/devices/testing/mock-ddk/mock-device.h>
#include <zxtest/zxtest.h>

#include "ctaphid.h"

namespace ctaphid {

namespace fhidbus = fuchsia_hardware_hidbus;
namespace finput = fuchsia_hardware_input;

/// Exact report descriptor for a Yubico 5 series security key (note the 0xF1DO near the start).
const uint8_t skey_desc[] = {
    0x06, 0xd0, 0xf1,  // Usage Page ( FIDO_USAGE_PAGE )
    0x09, 0x01,        // Usage ( FIDO_USAGE_CTAPHID )
    0xA1, 0x01,        // Collection ( Application )
    0x09, 0x20,        //     HID_Usage ( FIDO_USAGE_DATA_IN )
    0x15, 0x00,        //     Usage Minimum ( 0x00 )
    0x26, 0xff,        //     Usage Maximum ( 0xff )
    0x00, 0x75, 0x08,  //     HID_ReportSize ( 8 ),
    0x95, 0x40,        //     HID_ReportCount ( HID_INPUT_REPORT_BYTES )
    0x81, 0x02,        //     HID_Input ( HID_Data | HID_Absolute | HID_Variable ),
    0x09, 0x21,        //     HID_Usage ( FIDO_USAGE_DATA_OUT ),
    0x15, 0x00,        //     Usage Minimum ( 0x00 )
    0x26, 0xff,        //     Usage Maximum ( 0xff )
    0x00, 0x75, 0x08,  //     HID_ReportSize ( 8 ),
    0x95, 0x40,        //     HID_ReportCount ( HID_INPUT_REPORT_BYTES )
    0x91, 0x02,        //     HID_Output ( HID_Data | HID_Absolute | HID_Variable ),
    0xc0,              // End Collection
};

class FakeCtapHidDevice : public fidl::testing::WireTestBase<finput::Device> {
 public:
  class DeviceReportsReader : public fidl::WireServer<finput::DeviceReportsReader> {
   public:
    explicit DeviceReportsReader(FakeCtapHidDevice* parent, async_dispatcher_t* dispatcher,
                                 fidl::ServerEnd<finput::DeviceReportsReader> server)
        : binding_(dispatcher, std::move(server), this,
                   [parent](fidl::UnbindInfo info) { parent->reader_.reset(); }) {}

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

    libsync::Completion& wait_for_read() { return wait_for_read_; }

   private:
    fidl::ServerBinding<finput::DeviceReportsReader> binding_;
    std::optional<ReadReportsCompleter::Async> waiting_read_;
    libsync::Completion wait_for_read_;
  };

  static constexpr uint32_t kVendorId = 0xabc;
  static constexpr uint32_t kProductId = 123;
  static constexpr uint32_t kVersion = 5;

  explicit FakeCtapHidDevice(fidl::ServerEnd<finput::Device> server)
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

  void SetReport(SetReportRequestView request, SetReportCompleter::Sync& completer) override {
    report_ = std::vector<uint8_t>(request->report.data(),
                                   request->report.data() + request->report.count());
    n_set_reports_received++;
    completer.ReplySuccess();
  }

  void SetReportDesc(std::vector<uint8_t> report_desc) { report_desc_ = std::move(report_desc); }

  void SendReport(std::vector<uint8_t> report) {
    ASSERT_NOT_NULL(reader_);
    reader_->SendReport(std::move(report), zx::clock::get_monotonic());
  }

  void reset_set_reports_counter() { n_set_reports_received = 0; }
  void reset_packets_received_counter() { n_packets_received = 0; }

  libsync::Completion& wait_for_read() {
    EXPECT_NOT_NULL(reader_);
    return reader_->wait_for_read();
  }

  std::unique_ptr<DeviceReportsReader> reader_;
  uint32_t n_set_reports_received = 0;
  uint32_t n_packets_received = 0;

 private:
  async_dispatcher_t* dispatcher_;
  fidl::ServerBinding<finput::Device> binding_;

  std::vector<uint8_t> report_desc_;

  std::vector<uint8_t> report_;
};

class CtapHidDevTest : public zxtest::Test {
 public:
  CtapHidDevTest()
      : loop_(&kAsyncLoopConfigNeverAttachToThread), mock_parent_(MockDevice::FakeRootParent()) {}
  void SetUp() override {
    ASSERT_OK(hid_dev_loop_.StartThread("fake-hid-device-thread"));
    auto [client, server] = fidl::Endpoints<finput::Device>::Create();
    fake_hid_device_.emplace(std::move(server));
    ctap_driver_device_ = new CtapHidDriver(mock_parent_.get(), std::move(client));

    fake_hid_device_.SyncCall(&FakeCtapHidDevice::SetReportDesc,
                              std::vector<uint8_t>(skey_desc, skey_desc + sizeof(skey_desc)));
    ASSERT_OK(ctap_driver_device_->Bind());
  }

 protected:
  static constexpr size_t kFidlReportBufferSize = 8192;

  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> SetupSyncClient() {
    EXPECT_OK(loop_.StartThread("test-loop-thread"));
    auto endpoints = fidl::Endpoints<fuchsia_fido_report::SecurityKeyDevice>::Create();
    binding_ =
        fidl::BindServer(loop_.dispatcher(), std::move(endpoints.server), ctap_driver_device_);
    return fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice>(
        std::move(endpoints.client));
  }

  fidl::WireClient<fuchsia_fido_report::SecurityKeyDevice> SetupAsyncClient() {
    auto endpoints = fidl::Endpoints<fuchsia_fido_report::SecurityKeyDevice>::Create();
    binding_ =
        fidl::BindServer(loop_.dispatcher(), std::move(endpoints.server), ctap_driver_device_);
    return fidl::WireClient<fuchsia_fido_report::SecurityKeyDevice>(std::move(endpoints.client),
                                                                    loop_.dispatcher());
  }

  fuchsia_fido_report::wire::Message BuildRequest(fidl::Arena<kFidlReportBufferSize>& allocator,
                                                  uint32_t channel,
                                                  fuchsia_fido_report::CtapHidCommand command,
                                                  std::vector<uint8_t>& data) {
    auto fidl_skey_request_builder_ = fuchsia_fido_report::wire::Message::Builder(allocator);
    fidl_skey_request_builder_.channel_id(channel);
    fidl_skey_request_builder_.command_id(command);
    fidl_skey_request_builder_.data(fidl::VectorView<uint8_t>::FromExternal(data));
    fidl_skey_request_builder_.payload_len(static_cast<uint16_t>(data.size()));
    auto result = fidl_skey_request_builder_.Build();
    return result;
  }

  void SendReport(std::vector<uint8_t> report) {
    mock_ddk::GetDriverRuntime()->PerformBlockingWork([this]() {
      fake_hid_device_.SyncCall(&FakeCtapHidDevice::wait_for_read).Wait();
      fake_hid_device_.SyncCall(&FakeCtapHidDevice::wait_for_read).Reset();
    });
    fake_hid_device_.SyncCall(&FakeCtapHidDevice::SendReport, std::move(report));
    mock_ddk::GetDriverRuntime()->PerformBlockingWork(
        [this]() { fake_hid_device_.SyncCall(&FakeCtapHidDevice::wait_for_read).Wait(); });
  }

  async::Loop loop_;
  async::Loop hid_dev_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};

  std::shared_ptr<MockDevice> mock_parent_;
  async_patterns::TestDispatcherBound<FakeCtapHidDevice> fake_hid_device_{
      hid_dev_loop_.dispatcher()};
  CtapHidDriver* ctap_driver_device_;

  std::optional<fidl::ServerBindingRef<fuchsia_fido_report::SecurityKeyDevice>> binding_;
};

TEST_F(CtapHidDevTest, HidLifetimeTest) {
  fake_hid_device_.SyncCall([](FakeCtapHidDevice* dev) { ASSERT_TRUE(dev->reader_); });

  // make sure the child device is there
  ASSERT_EQ(mock_parent_->child_count(), 1);
  auto* child = mock_parent_->GetLatestChild();

  child->ReleaseOp();

  // Make sure that the CtapHidDriver class has unregistered from the HID device.
  fake_hid_device_.SyncCall([](FakeCtapHidDevice* dev) { ASSERT_FALSE(dev->reader_); });
}

TEST_F(CtapHidDevTest, SendMessageWithEmptyPayloadTest) {
  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> sync_client = SetupSyncClient();

  fidl::Arena<kFidlReportBufferSize> allocator;
  std::vector<uint8_t> data_vec{};
  auto message_request =
      BuildRequest(allocator, 0xFFFFFFFF, fuchsia_fido_report::CtapHidCommand::kInit, data_vec);

  // Send the Command.
  fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
      sync_client->SendMessage(message_request);
  loop_.RunUntilIdle();

  ASSERT_EQ(result.status(), ZX_OK);
  // Check the hid driver received the correct number of packets.
  fake_hid_device_.SyncCall(
      [](FakeCtapHidDevice* dev) { ASSERT_EQ(dev->n_set_reports_received, 1); });
}

TEST_F(CtapHidDevTest, SendMessageSinglePacketTest) {
  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> sync_client = SetupSyncClient();

  fidl::Arena<kFidlReportBufferSize> allocator;
  std::vector<uint8_t> data_vec{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08};
  auto message_request =
      BuildRequest(allocator, 0xFFFFFFFF, fuchsia_fido_report::CtapHidCommand::kInit, data_vec);

  // Send the Command.
  fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
      sync_client->SendMessage(message_request);
  loop_.RunUntilIdle();

  ASSERT_EQ(result.status(), ZX_OK);
  // Check the hid driver received the correct number of packets.
  fake_hid_device_.SyncCall(
      [](FakeCtapHidDevice* dev) { ASSERT_EQ(dev->n_set_reports_received, 1); });
}

TEST_F(CtapHidDevTest, SendMessageMultiPacketTest) {
  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> sync_client = SetupSyncClient();

  fidl::Arena<kFidlReportBufferSize> allocator;
  std::vector<uint8_t> data_vec(1024, 1);
  auto message_request =
      BuildRequest(allocator, 0xFFFFFFFF, fuchsia_fido_report::CtapHidCommand::kInit, data_vec);

  // Send the Command.
  fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
      sync_client->SendMessage(message_request);
  loop_.RunUntilIdle();

  ASSERT_EQ(result.status(), ZX_OK);
  // The Driver should have split this command into multiple packets.
  // The number of packets used to send this message should be:
  // ciel((data_size - (ouput_packet_size-7)) / (output_packet_size - 5)) + 1
  // In this case, the output_packet_size is 64 and the data_size is 1024.
  fake_hid_device_.SyncCall(
      [](FakeCtapHidDevice* dev) { ASSERT_EQ(dev->n_set_reports_received, 18); });
}

TEST_F(CtapHidDevTest, SendMessageChannelAlreadyPendingTest) {
  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> sync_client = SetupSyncClient();

  uint32_t test_channel = 0x01020304;

  // Send a Command on test_channel.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08};
    auto message_request =
        BuildRequest(allocator, test_channel, fuchsia_fido_report::CtapHidCommand::kInit, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();

    ASSERT_EQ(result.status(), ZX_OK);
  }

  // Send another Command on the same channel. This should fail since we are pending on a response
  // from the key for the original request.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0xde, 0xad, 0xbe, 0xef};
    auto message_request =
        BuildRequest(allocator, test_channel, fuchsia_fido_report::CtapHidCommand::kMsg, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();

    ASSERT_TRUE(result->is_error());
    ASSERT_EQ(result->error_value(), ZX_ERR_UNAVAILABLE);
  }

  // Have the key reply to the first command.
  {
    std::vector<uint8_t> packet{
        // channel id
        static_cast<uint8_t>((test_channel >> 24) & 0xff),
        static_cast<uint8_t>((test_channel >> 16) & 0xff),
        static_cast<uint8_t>((test_channel >> 8) & 0xff), static_cast<uint8_t>(test_channel & 0xff),
        // command id with init packet bit set
        static_cast<uint8_t>(fuchsia_fido_report::CtapHidCommand::kInit) | (1u << 7),
        // payload len
        0x00, 0x01,
        // payload
        0x0f};
    SendReport(packet);
    loop_.RunUntilIdle();
  }

  // Send another Command on the same channel again. This should still fail since we still need to
  // get the response from the original request.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0xde, 0xad, 0xbe, 0xef};
    auto message_request =
        BuildRequest(allocator, test_channel, fuchsia_fido_report::CtapHidCommand::kMsg, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();

    ASSERT_TRUE(result->is_error());
    ASSERT_EQ(result->error_value(), ZX_ERR_UNAVAILABLE);
  }

  // Get the response to the original command.
  {
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::GetMessage> result =
        sync_client->GetMessage(test_channel);
    loop_.RunUntilIdle();
  }

  // Retry sending another Command on the same channel. This should now succeed since the first
  // transaction has completed.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0xde, 0xad, 0xbe, 0xef};
    auto message_request =
        BuildRequest(allocator, test_channel, fuchsia_fido_report::CtapHidCommand::kMsg, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();

    ASSERT_FALSE(result->is_error());
  }
}

TEST_F(CtapHidDevTest, SendMessageDeviceBusyTest) {
  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> sync_client = SetupSyncClient();

  uint32_t test_channel = 0x01020304;
  uint8_t test_payload_byte = 0x0f;
  uint32_t other_test_channel = 0x09080706;

  // Send a Command on test_channel.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08};
    auto message_request =
        BuildRequest(allocator, test_channel, fuchsia_fido_report::CtapHidCommand::kMsg, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();

    ASSERT_EQ(result.status(), ZX_OK);
  }

  // Send another Command on a different channel.
  // This should fail as we're still waiting for a response on the first request.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0x01};
    auto message_request = BuildRequest(allocator, other_test_channel,
                                        fuchsia_fido_report::CtapHidCommand::kMsg, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();

    ASSERT_TRUE(result->is_error());
    ASSERT_EQ(result->error_value(), ZX_ERR_UNAVAILABLE);
  }

  // Have the key reply to the first command.
  {
    std::vector<uint8_t> packet{
        // channel id
        static_cast<uint8_t>((test_channel >> 24) & 0xff),
        static_cast<uint8_t>((test_channel >> 16) & 0xff),
        static_cast<uint8_t>((test_channel >> 8) & 0xff), static_cast<uint8_t>(test_channel & 0xff),
        // command id with init packet bit set
        static_cast<uint8_t>(fuchsia_fido_report::CtapHidCommand::kInit) | (1u << 7),
        // payload len
        0x00, 0x01,
        // payload
        test_payload_byte};
    SendReport(packet);
    loop_.RunUntilIdle();
  }

  // Try again to send another Command on a different channel.
  // This should still fail as the first request's response still needs to be retrieved.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0x01};
    auto message_request = BuildRequest(allocator, other_test_channel,
                                        fuchsia_fido_report::CtapHidCommand::kMsg, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();

    ASSERT_TRUE(result->is_error());
    ASSERT_EQ(result->error_value(), ZX_ERR_UNAVAILABLE);
  }

  // Get the response to the first command, on test_channel.
  {
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::GetMessage> result =
        sync_client->GetMessage(test_channel);
    loop_.RunUntilIdle();
  }

  // Finally try to send another Command on a different channel.
  // This should now succeed as the first transaction is complete.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0x01};
    auto message_request = BuildRequest(allocator, other_test_channel,
                                        fuchsia_fido_report::CtapHidCommand::kMsg, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();

    ASSERT_FALSE(result->is_error());
    ASSERT_OK(result);
  }
}

TEST_F(CtapHidDevTest, ReceiveSinglePacketMessageTest) {
  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> sync_client = SetupSyncClient();

  uint32_t test_channel = 0x01020304;
  auto test_command = fuchsia_fido_report::CtapHidCommand::kInit;
  std::vector<uint8_t> test_payload{0xde, 0xad, 0xbe, 0xef};

  // Send a SendMessage so we are able to call GetMessage.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08};
    auto message_request = BuildRequest(allocator, test_channel, test_command, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();
  }

  // Send a packet from the key.
  {
    std::vector<uint8_t> packet{// channel id
                                static_cast<uint8_t>((test_channel >> 24) & 0xff),
                                static_cast<uint8_t>((test_channel >> 16) & 0xff),
                                static_cast<uint8_t>((test_channel >> 8) & 0xff),
                                static_cast<uint8_t>(test_channel & 0xff),
                                // command id with init packet bit set
                                static_cast<uint8_t>(fidl::ToUnderlying(test_command) | (1u << 7)),
                                // payload len
                                0x00, 0x04,
                                // payload
                                0xde, 0xad, 0xbe, 0xef};
    SendReport(packet);
    loop_.RunUntilIdle();
  }

  // Get and check the Message formed from the packet.
  {
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::GetMessage> result =
        sync_client->GetMessage(test_channel);
    loop_.RunUntilIdle();

    ASSERT_EQ(result.status(), ZX_OK);
    ASSERT_TRUE(result->value()->has_channel_id());
    ASSERT_EQ(result->value()->channel_id(), test_channel);
    ASSERT_EQ(result->value()->command_id(), test_command);
    ASSERT_EQ(result->value()->payload_len(), test_payload.size());
    for (size_t i = 0; i < test_payload.size(); i++) {
      ASSERT_EQ(result->value()->data().at(i), test_payload.at(i));
    }
  }
}

TEST_F(CtapHidDevTest, ReceiveMultiplePacketMessageTest) {
  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> sync_client = SetupSyncClient();

  uint32_t test_channel = 0x01020304;
  auto test_command = fuchsia_fido_report::CtapHidCommand::kInit;

  uint8_t init_payload_len = 64 - 7;
  uint8_t cont_payload1_len = 64 - 5;
  uint8_t cont_payload2_len = 32;
  std::vector<uint8_t> test_init_payload(init_payload_len, 0x0a);
  std::vector<uint8_t> test_cont_payload1(cont_payload1_len, 0x0b);
  std::vector<uint8_t> test_cont_payload2(cont_payload2_len, 0x0c);

  auto total_payload(test_init_payload);
  total_payload.insert(total_payload.end(), test_cont_payload1.begin(), test_cont_payload1.end());
  total_payload.insert(total_payload.end(), test_cont_payload2.begin(), test_cont_payload2.end());
  uint16_t total_payload_len = static_cast<uint16_t>(total_payload.size());

  // Send a SendMessage so we are able to call GetMessage.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08};
    auto message_request = BuildRequest(allocator, test_channel, test_command, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();
  }

  // Send the packets from the key.
  {
    // Init Payload
    std::vector<uint8_t> init_packet{
        // channel id
        static_cast<uint8_t>((test_channel >> 24) & 0xff),
        static_cast<uint8_t>((test_channel >> 16) & 0xff),
        static_cast<uint8_t>((test_channel >> 8) & 0xff), static_cast<uint8_t>(test_channel & 0xff),
        // command id with init packet bit set
        static_cast<uint8_t>(fidl::ToUnderlying(test_command) | (1u << 7)),
        // payload len
        static_cast<uint8_t>((total_payload_len >> 8) & 0xff),
        static_cast<uint8_t>(total_payload_len & 0xff)};
    init_packet.insert(init_packet.end(), test_init_payload.begin(), test_init_payload.end());
    SendReport(init_packet);
    loop_.RunUntilIdle();
    // Cont Payload 1
    std::vector<uint8_t> cont_packet1{// channel id
                                      static_cast<uint8_t>((test_channel >> 24) & 0xff),
                                      static_cast<uint8_t>((test_channel >> 16) & 0xff),
                                      static_cast<uint8_t>((test_channel >> 8) & 0xff),
                                      static_cast<uint8_t>(test_channel & 0xff),
                                      // packet sequence number
                                      0x00};
    cont_packet1.insert(cont_packet1.end(), test_cont_payload1.begin(), test_cont_payload1.end());
    SendReport(cont_packet1);
    loop_.RunUntilIdle();
    // Cont Payload 2
    std::vector<uint8_t> cont_packet2{// channel id
                                      static_cast<uint8_t>((test_channel >> 24) & 0xff),
                                      static_cast<uint8_t>((test_channel >> 16) & 0xff),
                                      static_cast<uint8_t>((test_channel >> 8) & 0xff),
                                      static_cast<uint8_t>(test_channel & 0xff),
                                      // packet sequence number
                                      0x01};
    cont_packet2.insert(cont_packet2.end(), test_cont_payload2.begin(), test_cont_payload2.end());
    SendReport(cont_packet2);
    loop_.RunUntilIdle();
  }

  // Get and check the Message formed from the packet.
  {
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::GetMessage> result =
        sync_client->GetMessage(test_channel);
    loop_.RunUntilIdle();

    ASSERT_EQ(result.status(), ZX_OK);
    ASSERT_EQ(result->value()->channel_id(), test_channel);
    ASSERT_EQ(result->value()->command_id(), test_command);
    for (size_t i = 0; i < total_payload.size(); i++) {
      ASSERT_EQ(result->value()->data().at(i), total_payload.at(i));
    }
  }
}

TEST_F(CtapHidDevTest, ReceivePacketMissingInitTest) {
  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> sync_client = SetupSyncClient();

  uint32_t test_channel = 0x01020304;
  auto test_command = fuchsia_fido_report::CtapHidCommand::kInit;
  std::vector<uint8_t> test_payload{0xde, 0xad, 0xbe, 0xef};

  // Send a SendMessage so we are able to call GetMessage.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08};
    auto message_request = BuildRequest(allocator, test_channel, test_command, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();
  }

  // Send a packet from the key.
  {
    std::vector<uint8_t> packet{// channel id
                                static_cast<uint8_t>((test_channel >> 24) & 0xff),
                                static_cast<uint8_t>((test_channel >> 16) & 0xff),
                                static_cast<uint8_t>((test_channel >> 8) & 0xff),
                                static_cast<uint8_t>(test_channel & 0xff),
                                // packet sequence number
                                0x00,
                                // payload
                                0xde, 0xad, 0xbe, 0xef};
    SendReport(packet);
    loop_.RunUntilIdle();
  }

  // Check the response was set to an incorrect packet sequence error.
  {
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::GetMessage> result =
        sync_client->GetMessage(test_channel);
    loop_.RunUntilIdle();

    ASSERT_EQ(result.status(), ZX_OK);
    ASSERT_EQ(result->value()->channel_id(), test_channel);
    ASSERT_EQ(result->value()->command_id(), fuchsia_fido_report::CtapHidCommand::kError);
    ASSERT_NE(result->value()->payload_len(), test_payload.size());
    ASSERT_EQ(result->value()->payload_len(), 1);
    ASSERT_NE(result->value()->data().at(0), 0x04);
  }
}

TEST_F(CtapHidDevTest, ReceivePacketMissingContTest) {
  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> sync_client = SetupSyncClient();

  uint32_t test_channel = 0x01020304;
  auto test_command = fuchsia_fido_report::CtapHidCommand::kInit;

  uint8_t init_payload_len = 64 - 7;
  uint8_t cont_payload_len = 32;
  uint8_t total_payload_len = init_payload_len + cont_payload_len + (64 - 5);
  std::vector<uint8_t> test_init_payload(init_payload_len, 0x0a);
  std::vector<uint8_t> test_cont_payload(cont_payload_len, 0x0b);

  // Send a SendMessage so we are able to call GetMessage.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08};
    auto message_request = BuildRequest(allocator, test_channel, test_command, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();
  }

  // Send an init packet from the key.
  {
    // Init Payload
    std::vector<uint8_t> init_packet{
        // channel id
        static_cast<uint8_t>((test_channel >> 24) & 0xff),
        static_cast<uint8_t>((test_channel >> 16) & 0xff),
        static_cast<uint8_t>((test_channel >> 8) & 0xff), static_cast<uint8_t>(test_channel & 0xff),
        // command id with init packet bit set
        static_cast<uint8_t>(fidl::ToUnderlying(test_command) | (1u << 7)),
        // payload len
        static_cast<uint8_t>((total_payload_len >> 8) & 0xff),
        static_cast<uint8_t>(total_payload_len & 0xff)};
    init_packet.insert(init_packet.end(), test_init_payload.begin(), test_init_payload.end());
    SendReport(init_packet);
    loop_.RunUntilIdle();
  }

  // Send a continuation packet from the key, skipping the first packet.
  {
    std::vector<uint8_t> cont_packet{// channel id
                                     static_cast<uint8_t>((test_channel >> 24) & 0xff),
                                     static_cast<uint8_t>((test_channel >> 16) & 0xff),
                                     static_cast<uint8_t>((test_channel >> 8) & 0xff),
                                     static_cast<uint8_t>(test_channel & 0xff),
                                     // packet sequence number
                                     0x00 + 1};
    cont_packet.insert(cont_packet.end(), test_cont_payload.begin(), test_cont_payload.end());
    SendReport(cont_packet);
    loop_.RunUntilIdle();
  }

  // Check the response was set to an incorrect packet sequence error.
  {
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::GetMessage> result =
        sync_client->GetMessage(test_channel);
    loop_.RunUntilIdle();

    ASSERT_EQ(result.status(), ZX_OK);
    ASSERT_EQ(result->value()->channel_id(), test_channel);
    ASSERT_EQ(result->value()->command_id(), fuchsia_fido_report::CtapHidCommand::kError);
    ASSERT_EQ(result->value()->payload_len(), 1);
    ASSERT_NE(result->value()->data().at(0), 0x04);
  }
}

TEST_F(CtapHidDevTest, GetMessageChannelTest) {
  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> sync_client = SetupSyncClient();

  uint32_t const test_channel = 0x01020304;
  auto const test_command = fuchsia_fido_report::CtapHidCommand::kMsg;
  uint8_t const test_payload_byte = 0x0f;

  // Send a SendMessage request.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec(1024, 1);
    auto message_request = BuildRequest(allocator, test_channel, test_command, data_vec);

    // Send the Command.
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();
  }

  // Set up a packet to be sent as a response.
  {
    std::vector<uint8_t> packet{// channel id
                                static_cast<uint8_t>((test_channel >> 24) & 0xff),
                                static_cast<uint8_t>((test_channel >> 16) & 0xff),
                                static_cast<uint8_t>((test_channel >> 8) & 0xff),
                                static_cast<uint8_t>(test_channel & 0xff),
                                // command id with init packet bit set
                                static_cast<uint8_t>(fidl::ToUnderlying(test_command) | (1u << 7)),
                                // payload len
                                0x00, 0x01,
                                // payload
                                test_payload_byte};
    SendReport(packet);
    loop_.RunUntilIdle();
  }

  // Make a Request to get a message with a different channel id.
  {
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::GetMessage> result_1 =
        sync_client->GetMessage(0xffffffff);
    loop_.RunUntilIdle();

    ASSERT_TRUE(result_1->is_error());
    ASSERT_EQ(result_1->error_value(), ZX_ERR_NOT_FOUND);
  }

  // Make a Request to get a message with the correct channel id.
  {
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::GetMessage> result_2 =
        sync_client->GetMessage(test_channel);
    loop_.RunUntilIdle();

    ASSERT_FALSE(result_2->is_error());
    ASSERT_TRUE(result_2->value()->has_channel_id());
    ASSERT_TRUE(result_2->value()->has_data());
    ASSERT_EQ(result_2->value()->channel_id(), test_channel);

    ASSERT_EQ(result_2->value()->payload_len(), 1);
    ASSERT_EQ(result_2->value()->data().at(0), test_payload_byte);
  }
}

TEST_F(CtapHidDevTest, GetMessageKeepAliveTest) {
  fidl::WireSyncClient<fuchsia_fido_report::SecurityKeyDevice> sync_client = SetupSyncClient();

  uint32_t test_channel = 0x01020304;
  auto test_command = fuchsia_fido_report::CtapHidCommand::kInit;
  uint8_t test_payload_byte = 0x0f;

  // Send a command.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08};
    auto message_request = BuildRequest(allocator, test_channel, test_command, data_vec);

    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage> result =
        sync_client->SendMessage(message_request);
    loop_.RunUntilIdle();
    ASSERT_EQ(result.status(), ZX_OK);
  }

  // Set up a KEEPALIVE packet to be sent from the device.
  {
    std::vector<uint8_t> packet{
        // channel id
        static_cast<uint8_t>((test_channel >> 24) & 0xff),
        static_cast<uint8_t>((test_channel >> 16) & 0xff),
        static_cast<uint8_t>((test_channel >> 8) & 0xff), static_cast<uint8_t>(test_channel & 0xff),
        // command id with init packet bit set
        static_cast<uint8_t>(fidl::ToUnderlying(fuchsia_fido_report::CtapHidCommand::kKeepalive)) |
            (1u << 7),
        // payload len
        0x00, 0x01,
        // payload
        test_payload_byte};
    SendReport(packet);
    loop_.RunUntilIdle();
  }

  // Make a Request to get a message. This should return the KEEPALIVE message.
  {
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::GetMessage> result =
        sync_client->GetMessage(test_channel);
    loop_.RunUntilIdle();

    ASSERT_EQ(result.status(), ZX_OK);
    ASSERT_EQ(result->value()->command_id(), fuchsia_fido_report::CtapHidCommand::kKeepalive);
  }

  // Set up the real packet matching the original command to be sent from the device.
  {
    std::vector<uint8_t> packet{// channel id
                                static_cast<uint8_t>((test_channel >> 24) & 0xff),
                                static_cast<uint8_t>((test_channel >> 16) & 0xff),
                                static_cast<uint8_t>((test_channel >> 8) & 0xff),
                                static_cast<uint8_t>(test_channel & 0xff),
                                // command id with init packet bit set
                                static_cast<uint8_t>(fidl::ToUnderlying(test_command) | (1u << 7)),
                                // payload len
                                0x00, 0x01,
                                // payload
                                test_payload_byte};
    SendReport(packet);
    loop_.RunUntilIdle();
  }

  // Make a Request to get a message again. This should return the final message.
  {
    fidl::WireResult<fuchsia_fido_report::SecurityKeyDevice::GetMessage> result =
        sync_client->GetMessage(test_channel);
    loop_.RunUntilIdle();

    ASSERT_EQ(result.status(), ZX_OK);
    ASSERT_EQ(result->value()->command_id(), test_command);
  }
}

TEST_F(CtapHidDevTest, HangingGetMessageTest) {
  // Set up an async client to test GetMessage. We'll need to make the fake_hid_device send a packet
  // up to the ctaphid driver after we've sent the GetMessage() request.
  fidl::WireClient<fuchsia_fido_report::SecurityKeyDevice> async_client = SetupAsyncClient();

  uint32_t test_channel = 0x01020304;
  auto test_command = fuchsia_fido_report::CtapHidCommand::kInit;
  uint8_t test_payload_byte = 0x0f;

  // Send a command.
  {
    fidl::Arena<kFidlReportBufferSize> allocator;
    std::vector<uint8_t> data_vec{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08};
    auto message_request = BuildRequest(allocator, test_channel, test_command, data_vec);

    async_client->SendMessage(message_request)
        .ThenExactlyOnce(
            [&](fidl::WireUnownedResult<fuchsia_fido_report::SecurityKeyDevice::SendMessage>&
                    result) {
              ASSERT_OK(result);
              ASSERT_FALSE(result->is_error());
            });
    loop_.RunUntilIdle();
  }

  // Make a Request to get a message. This should hang until a response is sent from the device.
  async_client->GetMessage(test_channel)
      .ThenExactlyOnce(
          [&](fidl::WireUnownedResult<fuchsia_fido_report::SecurityKeyDevice::GetMessage>& result) {
            ASSERT_OK(result.status());
            ASSERT_FALSE(result->is_error());

            ASSERT_TRUE(result->value()->channel_id());
            ASSERT_EQ(result->value()->channel_id(), test_channel);
            ASSERT_TRUE(fidl::ToUnderlying(result->value()->command_id()));
            ASSERT_EQ(result->value()->command_id(), test_command);
            ASSERT_TRUE(result->value()->payload_len());
            ASSERT_EQ(result->value()->payload_len(), 1);
            ASSERT_TRUE(result->value()->has_data());
            ASSERT_EQ(result->value()->data().at(0), test_payload_byte);

            loop_.Quit();
          });

  // Send a response from the device.
  {
    std::vector<uint8_t> packet{// channel id
                                static_cast<uint8_t>((test_channel >> 24) & 0xff),
                                static_cast<uint8_t>((test_channel >> 16) & 0xff),
                                static_cast<uint8_t>((test_channel >> 8) & 0xff),
                                static_cast<uint8_t>(test_channel & 0xff),
                                // command id with init packet bit set
                                static_cast<uint8_t>(fidl::ToUnderlying(test_command) | (1u << 7)),
                                // payload len
                                0x00, 0x01,
                                // payload
                                test_payload_byte};
    SendReport(packet);
    loop_.RunUntilIdle();
  }
}

}  // namespace ctaphid
