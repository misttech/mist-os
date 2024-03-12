// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bt_hci_broadcom.h"

#include <fidl/fuchsia.hardware.bluetooth/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/async/cpp/wait.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>

#include <gtest/gtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "zircon/errors.h"

namespace bt_hci_broadcom {

namespace {

// Firmware binaries are a sequence of HCI commands containing the firmware as payloads. For
// testing, we use 1 HCI command with a 1 byte payload.
const std::vector<uint8_t> kFirmware = {
    0x01, 0x02,  // arbitrary "firmware opcode"
    0x01,        // parameter_total_size
    0x03         // payload
};
const char* kFirmwarePath = "BCM4345C5.hcd";

const std::array<uint8_t, 6> kMacAddress = {0x00, 0x01, 0x02, 0x03, 0x04, 0x05};

const std::array<uint8_t, 6> kCommandCompleteEvent = {
    0x0e,        // command complete event code
    0x04,        // parameter_total_size
    0x01,        // num_hci_command_packets
    0x00, 0x00,  // command opcode (hardcoded for simplicity since this isn't checked by the driver)
    0x00,        // return_code (success)
};

class FakeTransportDevice : public fidl::WireServer<fuchsia_hardware_bluetooth::Hci>,
                            public fdf::WireServer<fuchsia_hardware_serialimpl::Device> {
 public:
  explicit FakeTransportDevice(fdf_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  // Set a custom handler for commands. If null, command complete events will be automatically sent.
  void SetCommandHandler(fit::function<void(std::vector<uint8_t>)> command_callback) {
    command_callback_ = std::move(command_callback);
  }

  zx::channel& command_chan() { return command_channel_; }

  // fucshia_hardware_bluetooth::Hci request handler implementations:
  void OpenCommandChannel(OpenCommandChannelRequestView request,
                          OpenCommandChannelCompleter::Sync& completer) override {
    command_channel_ = std::move(request->channel);
    cmd_chan_wait_.set_object(command_channel_.get());
    cmd_chan_wait_.set_trigger(ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED);
    cmd_chan_wait_.Begin(fdf_dispatcher_get_async_dispatcher(dispatcher_));
    completer.ReplySuccess();
  }
  void OpenAclDataChannel(OpenAclDataChannelRequestView request,
                          OpenAclDataChannelCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void OpenScoDataChannel(OpenScoDataChannelRequestView request,
                          OpenScoDataChannelCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void ConfigureSco(ConfigureScoRequestView request,
                    ConfigureScoCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void ResetSco(ResetScoCompleter::Sync& completer) override {}
  void OpenIsoDataChannel(OpenIsoDataChannelRequestView request,
                          OpenIsoDataChannelCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void OpenSnoopChannel(OpenSnoopChannelRequestView request,
                        OpenSnoopChannelCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Hci> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    ZX_PANIC("Unknown method in HCI requests");
  }

  // fuchsia_hardware_serialimpl::Device FIDL request handler implementation.
  void GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) override {
    fuchsia_hardware_serial::wire::SerialPortInfo info = {
        .serial_class = fuchsia_hardware_serial::Class::kBluetoothHci,
        .serial_pid = PDEV_PID_BCM43458,
    };

    completer.buffer(arena).ReplySuccess(info);
  }
  void Config(ConfigRequestView request, fdf::Arena& arena,
              ConfigCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess();
  }
  void Enable(EnableRequestView request, fdf::Arena& arena,
              EnableCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess();
  }
  void Read(fdf::Arena& arena, ReadCompleter::Sync& completer) override {
    fidl::VectorView<uint8_t> data;
    completer.buffer(arena).ReplySuccess(data);
  }
  void Write(WriteRequestView request, fdf::Arena& arena,
             WriteCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess();
  }
  void CancelAll(fdf::Arena& arena, CancelAllCompleter::Sync& completer) override {}

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_serialimpl::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    ZX_PANIC("Unknown method in Serial requests");
  }

  void ServeHci(fidl::ServerEnd<fuchsia_io::Directory> server_end) {
    auto hci_handler = [&](fidl::ServerEnd<fuchsia_hardware_bluetooth::Hci> request) {
      fidl::BindServer(fdf_dispatcher_get_async_dispatcher(dispatcher_), std::move(request), this);
    };
    fuchsia_hardware_bluetooth::HciService::InstanceHandler handler(
        {.hci = std::move(hci_handler)});
    auto service_result = fdf_outgoing_.SyncCall([&](fdf::OutgoingDirectory* outgoing) {
      return outgoing->AddService<fuchsia_hardware_bluetooth::HciService>(std::move(handler));
    });
    ASSERT_EQ(ZX_OK, service_result.status_value());

    ASSERT_EQ(ZX_OK, fdf_outgoing_.SyncCall(&fdf::OutgoingDirectory::Serve, std::move(server_end))
                         .status_value());
  }

  void ServeSerial(fidl::ServerEnd<fuchsia_io::Directory> server_end) {
    auto serial_handler = [&](fdf::ServerEnd<fuchsia_hardware_serialimpl::Device> request) {
      fdf::BindServer(dispatcher_, std::move(request), this);
    };
    fuchsia_hardware_serialimpl::Service::InstanceHandler handler(
        {.device = std::move(serial_handler)});
    auto service_result = fdf_outgoing_.SyncCall([&](fdf::OutgoingDirectory* outgoing) {
      return outgoing->AddService<fuchsia_hardware_serialimpl::Service>(std::move(handler));
    });
    ASSERT_EQ(ZX_OK, service_result.status_value());

    ASSERT_EQ(ZX_OK, fdf_outgoing_.SyncCall(&fdf::OutgoingDirectory::Serve, std::move(server_end))
                         .status_value());
  }

 private:
  void OnCommandChannelSignal(async_dispatcher_t*, async::WaitBase* wait, zx_status_t status,
                              const zx_packet_signal_t* signal) {
    ASSERT_EQ(status, ZX_OK);
    if (signal->observed & ZX_CHANNEL_PEER_CLOSED) {
      command_channel_.reset();
      return;
    }
    ASSERT_TRUE(signal->observed & ZX_CHANNEL_READABLE);
    // Make buffer large enough to hold largest command packet.
    std::vector<uint8_t> bytes(
        sizeof(HciCommandHeader) +
        std::numeric_limits<decltype(HciCommandHeader::parameter_total_size)>::max());
    uint32_t actual_bytes = 0;
    zx_status_t read_status = command_channel_.read(
        /*flags=*/0, bytes.data(), /*handles=*/nullptr, static_cast<uint32_t>(bytes.size()),
        /*num_handles=*/0, &actual_bytes, /*actual_handles=*/nullptr);
    ASSERT_EQ(read_status, ZX_OK);
    bytes.resize(actual_bytes);

    cmd_chan_received_packets_.push_back(bytes);

    if (command_callback_) {
      command_callback_(std::move(bytes));
    } else {
      zx_status_t write_status = command_channel_.write(/*flags=*/0, kCommandCompleteEvent.data(),
                                                        kCommandCompleteEvent.size(),
                                                        /*handles=*/nullptr, /*num_handles=*/0);
      EXPECT_EQ(write_status, ZX_OK);
    }

    // The wait needs to be restarted.
    zx_status_t wait_begin_status = wait->Begin(fdf_dispatcher_get_async_dispatcher(dispatcher_));
    ASSERT_EQ(wait_begin_status, ZX_OK) << zx_status_get_string(wait_begin_status);
  }

  fit::function<void(std::vector<uint8_t>)> command_callback_;
  zx::channel command_channel_;
  std::vector<std::vector<uint8_t>> cmd_chan_received_packets_;
  async::WaitMethod<FakeTransportDevice, &FakeTransportDevice::OnCommandChannelSignal>
      cmd_chan_wait_{this};
  fdf_dispatcher_t* dispatcher_;
  async_patterns::TestDispatcherBound<fdf::OutgoingDirectory> fdf_outgoing_{
      fdf_dispatcher_get_async_dispatcher(dispatcher_), std::in_place};
};

class BtHciBroadcomTest : public ::testing::Test {
 public:
  BtHciBroadcomTest() : fake_transport_device_(transport_dispatcher_->get()) {}

  void SetUp() override {
    {
      // Serve fuchsia_hardware_bluetooth::Hci FIDL protocol from fake_transport_device_, and add
      // the protocol to the root device.
      auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
      ASSERT_FALSE(endpoints.is_error());
      fake_transport_device_.ServeHci(std::move(endpoints->server));
      root_device_->AddFidlService(fuchsia_hardware_bluetooth::HciService::Name,
                                   std::move(endpoints->client));
    }

    {
      // Serve fuchsia_hardware_serialimpl::Device FIDL protocol from fake_transport_device_, and
      // add the protocol to the root device.
      auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
      ASSERT_FALSE(endpoints.is_error());
      fake_transport_device_.ServeSerial(std::move(endpoints->server));
      root_device_->AddFidlService(fuchsia_hardware_serialimpl::Service::Name,
                                   std::move(endpoints->client));
    }

    ASSERT_EQ(BtHciBroadcom::Create(/*ctx=*/nullptr, root_device_.get(),
                                    fdf::Dispatcher::GetCurrent()->async_dispatcher()),
              ZX_OK);
    ASSERT_TRUE(dut());

    {
      // Connect to the Vendor protocol server implementation in the driver under test.
      auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_bluetooth::Vendor>();
      ASSERT_FALSE(endpoints.is_error());
      vendor_client_.Bind(std::move(endpoints->client));
      fidl::BindServer(vendor_dispatcher_->async_dispatcher(), std::move(endpoints->server),
                       dut()->GetDeviceContext<BtHciBroadcom>());
    }
  }

  void TearDown() override {
    mock_ddk::GetDriverRuntime()->RunUntilIdle();
    dut()->UnbindOp();
    EXPECT_EQ(dut()->UnbindReplyCallStatus(), ZX_OK);
    dut()->ReleaseOp();
  }

 protected:
  void SetFirmware() { dut()->SetFirmware(kFirmware, kFirmwarePath); }

  void SetMetadata() {
    root_dev()->SetMetadata(DEVICE_METADATA_MAC_ADDRESS, kMacAddress.data(), kMacAddress.size());
  }

  [[nodiscard]] zx_status_t InitDut() {
    dut()->InitOp();
    // Ensure delays fire.
    mock_ddk::GetDriverRuntime()->RunWithTimeoutOrUntil([]() -> bool { return false; });
    return dut()->InitReplyCallStatus();
  }

  MockDevice* root_dev() const { return root_device_.get(); }

  MockDevice* dut() const { return root_device_->GetLatestChild(); }

  FakeTransportDevice* transport() { return &fake_transport_device_; }

  fidl::WireSyncClient<fuchsia_hardware_bluetooth::Vendor> vendor_client_;

 private:
  std::shared_ptr<MockDevice> root_device_ = MockDevice::FakeRootParent();
  fdf::UnownedSynchronizedDispatcher vendor_dispatcher_ =
      mock_ddk::GetDriverRuntime()->StartBackgroundDispatcher();

  // Dispatcher that the fake transport device runs on.
  fdf::UnownedSynchronizedDispatcher transport_dispatcher_ =
      mock_ddk::GetDriverRuntime()->StartBackgroundDispatcher();

  FakeTransportDevice fake_transport_device_;
};

class BtHciBroadcomInitializedTest : public BtHciBroadcomTest {
 public:
  void SetUp() override {
    BtHciBroadcomTest::SetUp();
    SetFirmware();
    SetMetadata();
    ASSERT_EQ(InitDut(), ZX_OK);
  }
};

TEST_F(BtHciBroadcomInitializedTest, Lifecycle) {}

TEST_F(BtHciBroadcomTest, ReportLoadFirmwareError) {
  // Ensure reading metadata succeeds.
  SetMetadata();

  // No firmware has been set, so load_firmware() should fail during initialization.
  ASSERT_EQ(InitDut(), ZX_ERR_NOT_FOUND);
}

TEST_F(BtHciBroadcomTest, TooSmallFirmwareBuffer) {
  // Ensure reading metadata succeeds.
  SetMetadata();

  dut()->SetFirmware(std::vector<uint8_t>{0x00});
  ASSERT_EQ(InitDut(), ZX_ERR_INTERNAL);
}

TEST_F(BtHciBroadcomTest, ControllerReturnsEventSmallerThanEventHeader) {
  transport()->SetCommandHandler([this](const std::vector<uint8_t>& command) {
    zx_status_t write_status =
        transport()->command_chan().write(/*flags=*/0, kCommandCompleteEvent.data(),
                                          /*num_bytes=*/1,
                                          /*handles=*/nullptr, /*num_handles=*/0);
    EXPECT_EQ(write_status, ZX_OK);
  });

  SetFirmware();
  SetMetadata();
  ASSERT_NE(InitDut(), ZX_OK);
}

TEST_F(BtHciBroadcomTest, ControllerReturnsEventSmallerThanCommandComplete) {
  transport()->SetCommandHandler([this](const std::vector<uint8_t>& command) {
    zx_status_t write_status =
        transport()->command_chan().write(/*flags=*/0, kCommandCompleteEvent.data(),
                                          /*num_bytes=*/sizeof(HciEventHeader),
                                          /*handles=*/nullptr, /*num_handles=*/0);
    EXPECT_EQ(write_status, ZX_OK);
  });

  SetFirmware();
  SetMetadata();
  ASSERT_NE(InitDut(), ZX_OK);
}

TEST_F(BtHciBroadcomTest, ControllerReturnsBdaddrEventWithoutBdaddrParam) {
  // Set an invalid mac address in the metadata so that a ReadBdaddr command is sent to get fallback
  // address.
  root_dev()->SetMetadata(DEVICE_METADATA_MAC_ADDRESS, kMacAddress.data(), kMacAddress.size() - 1);
  SetFirmware();
  // Respond to ReadBdaddr command with a command complete (which doesn't include the bdaddr).
  transport()->SetCommandHandler([this](auto) {
    zx_status_t write_status =
        transport()->command_chan().write(/*flags=*/0, kCommandCompleteEvent.data(),
                                          /*num_bytes=*/kCommandCompleteEvent.size(),
                                          /*handles=*/nullptr, /*num_handles=*/0);
    EXPECT_EQ(write_status, ZX_OK);
  });

  // Ensure loading the firmware succeeds.
  SetFirmware();

  // Initialization should still succeed (an error will be logged, but it's not fatal).
  ASSERT_EQ(InitDut(), ZX_OK);
}

TEST_F(BtHciBroadcomTest, GetFeatures) {
  auto result = vendor_client_->GetFeatures();
  ASSERT_TRUE(result.ok());

  EXPECT_EQ(result->features, fuchsia_hardware_bluetooth::BtVendorFeatures::kSetAclPriorityCommand);
}

// TODO(b/326070040): Re-enable this test.  Issues with ddk / DFv1
TEST_F(BtHciBroadcomTest, DISABLED_VendorProtocolUnknownMethod) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_bluetooth::Vendor>();
  ASSERT_FALSE(endpoints.is_error());

  // Bind server end
  auto server_dispatcher = mock_ddk::GetDriverRuntime()->StartBackgroundDispatcher();
  fidl::BindServer(server_dispatcher->async_dispatcher(), std::move(endpoints->server),
                   dut()->GetDeviceContext<BtHciBroadcom>());

  // Bind client end (to the wrong protocol)
  fidl::ClientEnd<fuchsia_hardware_bluetooth::Hci> hci_end(endpoints->client.TakeChannel());
  fidl::WireSyncClient hci_client(std::move(hci_end));

  auto result = hci_client->ResetSco();

  ASSERT_TRUE(result.status() != ZX_OK);
  ASSERT_EQ(result.status(), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(BtHciBroadcomTest, HciProtocolUnknownMethod) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_bluetooth::Hci>();
  ASSERT_FALSE(endpoints.is_error());

  // Bind server end
  auto server_dispatcher = mock_ddk::GetDriverRuntime()->StartBackgroundDispatcher();
  fidl::BindServer(server_dispatcher->async_dispatcher(), std::move(endpoints->server),
                   dut()->GetDeviceContext<BtHciBroadcom>());

  // Bind client end (to the wrong protocol)
  fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor> vendor_end(endpoints->client.TakeChannel());
  fidl::WireSyncClient vendor_client(std::move(vendor_end));

  auto result = vendor_client->GetFeatures();

  ASSERT_TRUE(result.status() != ZX_OK);
  ASSERT_EQ(result.status(), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(BtHciBroadcomInitializedTest, EncodeSetAclPrioritySuccessWithParametersHighSink) {
  std::array<uint8_t, kBcmSetAclPriorityCmdSize> result_buffer;
  auto command = fuchsia_hardware_bluetooth::wire::BtVendorCommand::WithSetAclPriority({
      .connection_handle = 0xFF00,
      .priority = fuchsia_hardware_bluetooth::wire::BtVendorAclPriority::kHigh,
      .direction = fuchsia_hardware_bluetooth::wire::BtVendorAclDirection::kSink,
  });

  auto result = vendor_client_->EncodeCommand(command);
  ASSERT_TRUE(result.ok());
  ASSERT_FALSE(result->is_error());

  std::copy(result->value()->encoded.begin(), result->value()->encoded.end(),
            result_buffer.begin());
  const std::array<uint8_t, kBcmSetAclPriorityCmdSize> kExpectedBuffer = {
      0x1A,
      0xFD,  // OpCode
      0x04,  // size
      0x00,
      0xFF,                  // handle
      kBcmAclPriorityHigh,   // priority
      kBcmAclDirectionSink,  // direction
  };
  EXPECT_EQ(result_buffer, kExpectedBuffer);
}

TEST_F(BtHciBroadcomInitializedTest, EncodeSetAclPrioritySuccessWithParametersNormalSource) {
  std::array<uint8_t, kBcmSetAclPriorityCmdSize> result_buffer;
  auto command = fuchsia_hardware_bluetooth::wire::BtVendorCommand::WithSetAclPriority({
      .connection_handle = 0xFF00,
      .priority = fuchsia_hardware_bluetooth::wire::BtVendorAclPriority::kNormal,
      .direction = fuchsia_hardware_bluetooth::wire::BtVendorAclDirection::kSource,
  });
  auto result = vendor_client_->EncodeCommand(command);
  ASSERT_TRUE(result.ok());
  ASSERT_FALSE(result->is_error());

  std::copy(result->value()->encoded.begin(), result->value()->encoded.end(),
            result_buffer.begin());
  const std::array<uint8_t, kBcmSetAclPriorityCmdSize> kExpectedBuffer = {
      0x1A,
      0xFD,  // OpCode
      0x04,  // size
      0x00,
      0xFF,                    // handle
      kBcmAclPriorityNormal,   // priority
      kBcmAclDirectionSource,  // direction
  };
  EXPECT_EQ(result_buffer, kExpectedBuffer);
}

}  // namespace

}  // namespace bt_hci_broadcom
