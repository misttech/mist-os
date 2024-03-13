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
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/sync/cpp/completion.h>

#include <gtest/gtest.h>

#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"
#include "src/storage/lib/vfs/cpp/vmo_file.h"

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

const std::vector<uint8_t> kMacAddress = {0x00, 0x01, 0x02, 0x03, 0x04, 0x05};

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
  explicit FakeTransportDevice(fdf::UnownedSynchronizedDispatcher* dispatcher)
      : dispatcher_(dispatcher) {}

  ~FakeTransportDevice() {
    libsync::Completion close_bindings;
    async::PostTask((*dispatcher_)->async_dispatcher(), [&]() {
      serial_binding_group_.RemoveAll();
      hci_binding_group_.RemoveAll();
      close_bindings.Signal();
    });
    close_bindings.Wait();
  }

  fuchsia_hardware_serialimpl::Service::InstanceHandler GetSerialInstanceHandler() {
    return fuchsia_hardware_serialimpl::Service::InstanceHandler({
        .device = serial_binding_group_.CreateHandler(this, (*dispatcher_)->get(),
                                                      fidl::kIgnoreBindingClosure),
    });
  }
  fuchsia_hardware_bluetooth::HciService::InstanceHandler GetHciInstanceHandler() {
    return fuchsia_hardware_bluetooth::HciService::InstanceHandler({
        .hci = hci_binding_group_.CreateHandler(this, (*dispatcher_)->async_dispatcher(),
                                                fidl::kIgnoreBindingClosure),
    });
  }

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
    cmd_chan_wait_.Begin((*dispatcher_)->async_dispatcher());
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
    zx_status_t wait_begin_status = wait->Begin((*dispatcher_)->async_dispatcher());
    ASSERT_EQ(wait_begin_status, ZX_OK) << zx_status_get_string(wait_begin_status);
  }

  fit::function<void(std::vector<uint8_t>)> command_callback_;
  zx::channel command_channel_;
  std::vector<std::vector<uint8_t>> cmd_chan_received_packets_;
  async::WaitMethod<FakeTransportDevice, &FakeTransportDevice::OnCommandChannelSignal>
      cmd_chan_wait_{this};
  fdf::UnownedSynchronizedDispatcher* dispatcher_;

  fdf::ServerBindingGroup<fuchsia_hardware_serialimpl::Device> serial_binding_group_;
  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::Hci> hci_binding_group_;
};

class TestEnvironmentLocal : public fdf_testing::TestEnvironment {
 public:
  ~TestEnvironmentLocal() {}

  zx::result<> Initialize(fidl::ServerEnd<fuchsia_io::Directory> incoming_directory_server_end) {
    zx::result result =
        fdf_testing::TestEnvironment::Initialize(std::move(incoming_directory_server_end));
    ZX_ASSERT(!result.is_error());

    firmware_dir_ = fbl::MakeRefCounted<fs::PseudoDir>();
    auto dir_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(dir_endpoints.is_ok());
    firmware_server_.SetDispatcher(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    // Serve our firmware directory (will start serving FIDL requests on dir_endpoints with
    // dispatcher on previous line)
    ZX_ASSERT(firmware_server_.ServeDirectory(firmware_dir_, std::move(dir_endpoints->server)) ==
              ZX_OK);
    // Attach the firmware directory endpoint to "pkg/lib"
    ZX_ASSERT(incoming_directory()
                  .component()
                  .AddDirectoryAt(std::move(dir_endpoints->client), "pkg/lib", "firmware")
                  .is_ok());

    device_server_ = std::make_unique<compat::DeviceServer>();
    return zx::ok();
  }

  void AddSerialService(fuchsia_hardware_serialimpl::Service::InstanceHandler&& handler) {
    zx::result result =
        incoming_directory().AddService<fuchsia_hardware_serialimpl::Service>(std::move(handler));
    EXPECT_TRUE(result.is_ok());
  }

  void AddHciService(fuchsia_hardware_bluetooth::HciService::InstanceHandler&& handler) {
    zx::result result =
        incoming_directory().AddService<fuchsia_hardware_bluetooth::HciService>(std::move(handler));
    EXPECT_TRUE(result.is_ok());
  }

  void AddFirmwareFile(const std::vector<uint8_t> firmware) {
    // Create vmo for firmware file.
    zx::vmo vmo;
    zx::vmo::create(4096, 0, &vmo);
    vmo.write(firmware.data(), 0, firmware.size());
    vmo.set_prop_content_size(firmware.size());

    //  Create firmware file, and add it to the "firmware" directory we added under pkg/lib.
    fbl::RefPtr<fs::VmoFile> firmware_file =
        fbl::MakeRefCounted<fs::VmoFile>(std::move(vmo), firmware.size());
    ZX_ASSERT(firmware_dir_->AddEntry(kFirmwarePath, firmware_file) == ZX_OK);
  }

  zx_status_t SetMetadata(uint32_t name, const std::vector<uint8_t> data, const size_t size) {
    device_server_->Init("default", "");
    // Serve metadata.
    EXPECT_EQ(ZX_OK, device_server_->AddMetadata(name, data.data(), size));
    return device_server_->Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                 &incoming_directory());
  }

  void DestructDeviceServer() { device_server_.reset(); }

 private:
  std::unique_ptr<compat::DeviceServer> device_server_;
  fbl::RefPtr<fs::PseudoDir> firmware_dir_;
  fs::SynchronousVfs firmware_server_;
};

class BtHciBroadcomTest : public ::testing::Test {
 public:
  BtHciBroadcomTest() : fake_transport_device_(&env_dispatcher_) {}

  void SetUp() override {
    // Create start args
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    ASSERT_EQ(ZX_OK, start_args.status_value());
    start_arg_result_ = std::move(start_args.value());

    // Start the test environment with incoming directory returned from the start args
    zx::result init_result = test_environment_.SyncCall(
        &TestEnvironmentLocal::Initialize, std::move(start_arg_result_.incoming_directory_server));
    EXPECT_EQ(ZX_OK, init_result.status_value());

    // Get service handler from the fake_transport_device_ object.
    auto serial_handler = fake_transport_device_.GetSerialInstanceHandler();

    test_environment_.SyncCall(&TestEnvironmentLocal::AddSerialService, std::move(serial_handler));
    // Get service handler from the fake_transport_device_ object.
    auto hci_handler = fake_transport_device_.GetHciInstanceHandler();

    test_environment_.SyncCall(&TestEnvironmentLocal::AddHciService, std::move(hci_handler));
  }

  void TearDown() override {
    runtime_.RunUntilIdle();

    zx::result prepare_stop_result = runtime_.RunToCompletion(
        driver_.SyncCall(&fdf_testing::DriverUnderTest<BtHciBroadcom>::PrepareStop));
    EXPECT_EQ(ZX_OK, prepare_stop_result.status_value());

    zx::result stop_result = driver_.SyncCall(&fdf_testing::DriverUnderTest<BtHciBroadcom>::Stop);
    EXPECT_EQ(ZX_OK, stop_result.status_value());
    test_environment_.SyncCall(&TestEnvironmentLocal::DestructDeviceServer);
  }

 protected:
  void SetFirmware(const std::vector<uint8_t> firmware = kFirmware) {
    test_environment_.SyncCall(&TestEnvironmentLocal::AddFirmwareFile, firmware);
  }

  void SetMetadata(uint32_t name = DEVICE_METADATA_MAC_ADDRESS,
                   const std::vector<uint8_t> data = kMacAddress, const size_t size = kMacAddrLen) {
    // Serve metadata.
    ASSERT_EQ(ZX_OK,
              test_environment_.SyncCall(&TestEnvironmentLocal::SetMetadata, name, data, size));
  }

  [[nodiscard]] zx_status_t DriverStart() {
    auto start_result = runtime_.RunToCompletion(
        driver_.SyncCall(&fdf_testing::DriverUnderTest<BtHciBroadcom>::Start,
                         std::move(start_arg_result_.start_args)));
    return start_result.status_value();
  }

  void OpenVendor() {
    // Connect to Vendor protocol through devfs, get the channel handle from node server.
    zx::result<zx::channel> channel_result =
        node_server_.SyncCall([&](fdf_testing::TestNode* test_node) {
          fdf_testing::TestNode* current = &test_node->children().at("bt-hci-broadcom");
          return current->ConnectToDevice();
        });
    EXPECT_FALSE(channel_result.is_error());

    // Bind the channel to a Vendor client end.
    fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor> vendor_client_end(
        std::move(channel_result.value()));
    vendor_client_.Bind(std::move(vendor_client_end));
  }

  void OpenVendorWithHciClient() {
    // Connect to Vendor protocol through devfs, get the channel handle from node server.
    zx::result<zx::channel> channel_result =
        node_server_.SyncCall([&](fdf_testing::TestNode* test_node) {
          fdf_testing::TestNode* current = &test_node->children().at("bt-hci-broadcom");
          return current->ConnectToDevice();
        });
    EXPECT_FALSE(channel_result.is_error());

    // Bind the channel to an Hci client end.
    fidl::ClientEnd<fuchsia_hardware_bluetooth::Hci> hci_client_end(
        std::move(channel_result.value()));
    hci_client_.Bind(std::move(hci_client_end));
  }

  void OpenHci() {
    // Connect to Hci through vendor protocol
    auto open_hci_result = vendor_client_->OpenHci();
    EXPECT_TRUE(open_hci_result.ok());
    EXPECT_FALSE(open_hci_result->is_error());

    hci_client_.Bind(std::move(open_hci_result->value()->channel));
  }

  FakeTransportDevice* transport() { return &fake_transport_device_; }

  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }

  async_dispatcher_t* driver_dispatcher() { return driver_dispatcher_->async_dispatcher(); }

  fidl::WireSyncClient<fuchsia_hardware_bluetooth::Vendor> vendor_client_;
  fidl::WireSyncClient<fuchsia_hardware_bluetooth::Hci> hci_client_;

 private:
  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  fdf_testing::TestNode::CreateStartArgsResult start_arg_result_;

  // Env dispatcher runs in the background because we need to make sync calls into it.
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  // Driver dispatcher set as a background dispatcher. The protocols served by dut will run on
  // this dispatcher.
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};

  async_patterns::TestDispatcherBound<TestEnvironmentLocal> test_environment_{env_dispatcher(),
                                                                              std::in_place};

  FakeTransportDevice fake_transport_device_;

  // The driver under test wrapped by a dispatcher bound.
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<BtHciBroadcom>> driver_{
      driver_dispatcher(), std::in_place};
};

class BtHciBroadcomInitializedTest : public BtHciBroadcomTest {
 public:
  void SetUp() override {
    BtHciBroadcomTest::SetUp();
    SetFirmware();
    SetMetadata();
    ASSERT_EQ(DriverStart(), ZX_OK);
    OpenVendor();
  }
};

TEST_F(BtHciBroadcomInitializedTest, Lifecycle) {}

TEST_F(BtHciBroadcomTest, ReportLoadFirmwareError) {
  // Ensure reading metadata succeeds.
  SetMetadata();

  // No firmware has been set, so load_firmware() should fail during initialization.
  ASSERT_EQ(DriverStart(), ZX_ERR_NOT_FOUND);
}

TEST_F(BtHciBroadcomTest, TooSmallFirmwareBuffer) {
  // Ensure reading metadata succeeds.
  SetMetadata();

  SetFirmware(std::vector<uint8_t>{0x00});
  ASSERT_EQ(DriverStart(), ZX_ERR_INTERNAL);
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
  ASSERT_NE(DriverStart(), ZX_OK);
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
  ASSERT_NE(DriverStart(), ZX_OK);
}

TEST_F(BtHciBroadcomTest, ControllerReturnsBdaddrEventWithoutBdaddrParam) {
  // Set an invalid mac address in the metadata so that a ReadBdaddr command is sent to get
  // fallback address.
  SetMetadata(DEVICE_METADATA_MAC_ADDRESS, kMacAddress, kMacAddress.size() - 1);
  //  Respond to ReadBdaddr command with a command complete (which doesn't include the bdaddr).
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
  ASSERT_EQ(DriverStart(), ZX_OK);
}

TEST_F(BtHciBroadcomTest, VendorProtocolUnknownMethod) {
  SetFirmware();
  SetMetadata();
  ASSERT_EQ(DriverStart(), ZX_OK);

  OpenVendorWithHciClient();

  auto result = hci_client_->ResetSco();

  ASSERT_EQ(result.status(), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(BtHciBroadcomTest, HciProtocolUnknownMethod) {
  SetFirmware();
  SetMetadata();
  ASSERT_EQ(DriverStart(), ZX_OK);
  OpenVendor();

  // Connect to Hci through Vendor protocol
  auto open_hci_result = vendor_client_->OpenHci();
  EXPECT_TRUE(open_hci_result.ok());
  EXPECT_FALSE(open_hci_result->is_error());

  // Bind the channel to Vendor client end.
  fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor> vendor_client_end(
      open_hci_result->value()->channel.TakeChannel());
  fidl::WireSyncClient vendor_client(std::move(vendor_client_end));

  auto result = vendor_client->GetFeatures();

  ASSERT_EQ(result.status(), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(BtHciBroadcomInitializedTest, GetFeatures) {
  auto result = vendor_client_->GetFeatures();
  ASSERT_TRUE(result.ok());

  EXPECT_EQ(result->features, fuchsia_hardware_bluetooth::BtVendorFeatures::kSetAclPriorityCommand);
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
