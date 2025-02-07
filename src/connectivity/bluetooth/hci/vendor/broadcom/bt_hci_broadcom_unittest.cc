// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bt_hci_broadcom.h"

#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/async/cpp/wait.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/sync/cpp/completion.h>

#include <gtest/gtest.h>

#include "src/connectivity/bluetooth/hci/vendor/broadcom/packets.h"
#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"
#include "src/storage/lib/vfs/cpp/vmo_file.h"

namespace bt_hci_broadcom {

namespace {
namespace fhbt = fuchsia_hardware_bluetooth;
// Firmware binaries are a sequence of HCI commands containing the firmware as payloads. For
// testing, we use 1 HCI command with a 1 byte payload.
const std::vector<uint8_t> kFirmware = {
    0x01, 0x02,  // arbitrary "firmware opcode"
    0x01,        // parameter_total_size
    0x03         // payload
};
const std::vector<std::string> kFirmwarePaths = {"BCM4345C5.hcd", "BCM4381A1.hcd"};

const std::vector<uint8_t> kMacAddress = {0x00, 0x01, 0x02, 0x03, 0x04, 0x05};

const std::array<uint8_t, 6> kCommandCompleteEvent = {
    0x0e,        // command complete event code
    0x04,        // parameter_total_size
    0x01,        // num_hci_command_packets
    0x00, 0x00,  // command opcode (hardcoded for simplicity since this isn't checked by the driver)
    0x00,        // return_code (success)
};

class FakeTransportDevice : public fdf::WireServer<fuchsia_hardware_serialimpl::Device>,
                            public fidl::Server<fhbt::HciTransport>,
                            public fidl::Server<fhbt::Snoop> {
 public:
  explicit FakeTransportDevice() = default;

  fuchsia_hardware_serialimpl::Service::InstanceHandler GetSerialInstanceHandler() {
    return fuchsia_hardware_serialimpl::Service::InstanceHandler({
        .device = serial_binding_group_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                                      fidl::kIgnoreBindingClosure),
    });
  }
  fhbt::HciService::InstanceHandler GetHciInstanceHandler() {
    return fhbt::HciService::InstanceHandler({
        .hci_transport = hci_transport_binding_group_.CreateHandler(
            this, fdf::Dispatcher::GetCurrent()->async_dispatcher(), fidl::kIgnoreBindingClosure),
        .snoop = snoop_binding_group_.CreateHandler(
            this, fdf::Dispatcher::GetCurrent()->async_dispatcher(), fidl::kIgnoreBindingClosure),
    });
  }

  void SetCustomizedReply(std::vector<uint8_t> reply) {
    customized_reply_.emplace(std::move(reply));
  }

  // fhbt::HciTransport request handler implementations:
  void Send(SendRequest& request, SendCompleter::Sync& completer) override {
    if (request.Which() == fhbt::SentPacket::Tag::kCommand) {
      // The command opcode is the first two bytes.
      std::vector<uint8_t>& packet = request.command().value();
      uint16_t opcode = static_cast<uint16_t>(packet[1] << 8) | static_cast<uint16_t>(packet[0]);
      received_opcodes_.insert(opcode);
    }

    std::vector<uint8_t> reply;

    if (customized_reply_) {
      reply = *customized_reply_;
    } else {
      reply = std::vector<uint8_t>(kCommandCompleteEvent.data(),
                                   kCommandCompleteEvent.data() + kCommandCompleteEvent.size());
    }

    hci_transport_binding_group_.ForEachBinding(
        [&](const fidl::ServerBinding<fhbt::HciTransport>& binding) {
          auto received_packet = fhbt::ReceivedPacket::WithEvent(reply);
          fit::result<fidl::OneWayError> result =
              fidl::SendEvent(binding)->OnReceive(received_packet);
          ASSERT_FALSE(result.is_error());
        });
    completer.Reply();
  }
  void AckReceive(AckReceiveCompleter::Sync& completer) override {}
  void ConfigureSco(
      fidl::Server<fhbt::HciTransport>::ConfigureScoRequest& request,
      fidl::Server<fhbt::HciTransport>::ConfigureScoCompleter::Sync& completer) override {}
  void handle_unknown_method(::fidl::UnknownMethodMetadata<fhbt::HciTransport> metadata,
                             ::fidl::UnknownMethodCompleter::Sync& completer) override {
    ZX_PANIC("Unknown method in HciTransport requests");
  }

  void SetSerialPid(uint16_t serial_pid) { serial_pid_ = serial_pid; }

  const std::unordered_set<uint16_t>& GetReceivedOpCodes() { return received_opcodes_; }

  // fuchsia_hardware_serialimpl::Device FIDL request handler implementation.
  void GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) override {
    fuchsia_hardware_serial::wire::SerialPortInfo info = {
        .serial_class = fuchsia_hardware_serial::Class::kBluetoothHci,
        .serial_pid = serial_pid_,
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

  // fidl::Server<fhbt::Snoop> overrides:
  void AcknowledgePackets(AcknowledgePacketsRequest& request,
                          AcknowledgePacketsCompleter::Sync& completer) override {}
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Snoop> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  std::optional<std::vector<uint8_t>> customized_reply_;
  uint16_t serial_pid_ = PDEV_PID_BCM43458;
  std::unordered_set<uint16_t> received_opcodes_;

  fdf::ServerBindingGroup<fuchsia_hardware_serialimpl::Device> serial_binding_group_;
  fidl::ServerBindingGroup<fhbt::HciTransport> hci_transport_binding_group_;
  fidl::ServerBindingGroup<fhbt::Snoop> snoop_binding_group_;
};

class TestEnvironment : fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    auto dir_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
    firmware_server_.SetDispatcher(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    // Serve our firmware directory (will start serving FIDL requests on dir_endpoints with
    // dispatcher on previous line)
    ZX_ASSERT(firmware_server_.ServeDirectory(firmware_dir_, std::move(dir_endpoints.server)) ==
              ZX_OK);
    // Attach the firmware directory endpoint to "pkg/lib"
    ZX_ASSERT(to_driver_vfs.component()
                  .AddDirectoryAt(std::move(dir_endpoints.client), "pkg/lib", "firmware")
                  .is_ok());

    // Add the services that the fake parent driver exposes to the incoming directory of the driver
    // under test.
    zx::result result = to_driver_vfs.AddService<fuchsia_hardware_serialimpl::Service>(
        transport_device_.GetSerialInstanceHandler());
    EXPECT_TRUE(result.is_ok());

    result = to_driver_vfs.AddService<fhbt::HciService>(transport_device_.GetHciInstanceHandler());
    EXPECT_TRUE(result.is_ok());

    device_server_.Initialize(component::kDefaultInstance);
    EXPECT_EQ(ZX_OK, device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                          &to_driver_vfs));
    return zx::ok();
  }

  void AddFirmwareFile(const std::vector<uint8_t>& firmware) {
    // Create vmo for firmware file.
    zx::vmo vmo;
    zx::vmo::create(4096, 0, &vmo);
    vmo.write(firmware.data(), 0, firmware.size());
    vmo.set_prop_content_size(firmware.size());

    //  Create firmware file, and add it to the "firmware" directory we added under pkg/lib.
    fbl::RefPtr<fs::VmoFile> firmware_file =
        fbl::MakeRefCounted<fs::VmoFile>(std::move(vmo), firmware.size());
    for (const auto& path : kFirmwarePaths) {
      ZX_ASSERT(firmware_dir_->AddEntry(path, firmware_file) == ZX_OK);
    }
  }

  zx_status_t SetMetadata(uint32_t name, const std::vector<uint8_t>& data, const size_t size) {
    // Serve metadata.
    return device_server_.AddMetadata(name, data.data(), size);
  }

  FakeTransportDevice transport_device_;

 private:
  compat::DeviceServer device_server_;
  fbl::RefPtr<fs::PseudoDir> firmware_dir_ = fbl::MakeRefCounted<fs::PseudoDir>();
  fs::SynchronousVfs firmware_server_;
};

class FixtureConfig final {
 public:
  using DriverType = BtHciBroadcom;
  using EnvironmentType = TestEnvironment;
};

class BtHciBroadcomTest : public ::testing::Test {
 public:
  BtHciBroadcomTest() = default;

  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  fdf_testing::BackgroundDriverTest<FixtureConfig>& driver_test() { return driver_test_; }

 protected:
  void SetFirmware(const std::vector<uint8_t> firmware = kFirmware) {
    driver_test().RunInEnvironmentTypeContext(
        [&](TestEnvironment& env) { env.AddFirmwareFile(firmware); });
  }

  void SetMetadata(uint32_t name = DEVICE_METADATA_MAC_ADDRESS,
                   const std::vector<uint8_t> data = kMacAddress, const size_t size = kMacAddrLen) {
    ASSERT_EQ(ZX_OK, driver_test().RunInEnvironmentTypeContext<zx_status_t>(
                         [&](TestEnvironment& env) { return env.SetMetadata(name, data, size); }));
  }

  void OpenVendor() {
    // Connect to Vendor protocol through devfs, get the channel handle from node server.
    zx::result connect_result = driver_test().ConnectThroughDevfs<fhbt::Vendor>("bt-hci-broadcom");
    ASSERT_EQ(ZX_OK, connect_result.status_value());

    // Bind the channel to a Vendor client end.
    vendor_client_.Bind(std::move(connect_result.value()));

    // Verify features & ensure driver responds to requests.
    fidl::WireResult<fhbt::Vendor::GetFeatures> features = vendor_client_->GetFeatures();
    ASSERT_TRUE(features.ok());
    EXPECT_TRUE(features.value().acl_priority_command());
  }

  void OpenVendorWithHciTransportClient() {
    // Connect to Vendor protocol through devfs, get the channel handle from node server.
    zx::result connect_result = driver_test().ConnectThroughDevfs<fhbt::Vendor>("bt-hci-broadcom");
    ASSERT_EQ(ZX_OK, connect_result.status_value());

    fidl::ClientEnd<fhbt::HciTransport> hci_transport_end(connect_result.value().TakeChannel());
    hci_transport_client_.Bind(std::move(hci_transport_end));
  }

  fdf_testing::BackgroundDriverTest<FixtureConfig> driver_test_;

  fidl::WireSyncClient<fhbt::Vendor> vendor_client_;
  fidl::WireSyncClient<fhbt::HciTransport> hci_transport_client_;
};

class BtHciBroadcomInitializedTest : public BtHciBroadcomTest {
 public:
  void SetUp() override {
    BtHciBroadcomTest::SetUp();
    SetFirmware();
    SetMetadata();
    ASSERT_TRUE(driver_test().StartDriver().is_ok());
    OpenVendor();
  }
};

TEST_F(BtHciBroadcomInitializedTest, Lifecycle) {}

TEST_F(BtHciBroadcomInitializedTest, OpenSnoop) {
  ::fidl::WireResult<::fuchsia_hardware_bluetooth::Vendor::OpenSnoop> result =
      vendor_client_->OpenSnoop();
  ASSERT_TRUE(result.ok());
  ASSERT_FALSE(result->is_error());
}

TEST_F(BtHciBroadcomInitializedTest, HciTransportOpenTwice) {
  // Should be able to open two copies of HciTransport.
  auto result = vendor_client_->OpenHciTransport();
  ASSERT_TRUE(result.ok());
  ASSERT_FALSE(result->is_error());

  auto result_second = vendor_client_->OpenHciTransport();
  ASSERT_TRUE(result_second.ok());
  ASSERT_FALSE(result_second->is_error());
}

TEST_F(BtHciBroadcomTest, ReportLoadFirmwareError) {
  // Ensure reading metadata succeeds.
  SetMetadata();

  // No firmware has been set, so load_firmware() should fail during initialization.
  ASSERT_EQ(driver_test().StartDriver().status_value(), ZX_ERR_NOT_FOUND);
}

TEST_F(BtHciBroadcomTest, TooSmallFirmwareBuffer) {
  // Ensure reading metadata succeeds.
  SetMetadata();

  SetFirmware(std::vector<uint8_t>{0x00});
  ASSERT_EQ(driver_test().StartDriver().status_value(), ZX_ERR_INTERNAL);
}

TEST_F(BtHciBroadcomTest, ControllerReturnsEventSmallerThanEventHeader) {
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.transport_device_.SetCustomizedReply(
        std::vector<uint8_t>(kCommandCompleteEvent.data(), kCommandCompleteEvent.data() + 1));
  });

  SetFirmware();
  SetMetadata();
  ASSERT_NE(driver_test().StartDriver().status_value(), ZX_OK);
}

TEST_F(BtHciBroadcomTest, ControllerReturnsEventSmallerThanCommandComplete) {
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.transport_device_.SetCustomizedReply(std::vector<uint8_t>(
        kCommandCompleteEvent.data(), kCommandCompleteEvent.data() + sizeof(HciEventHeader)));
  });

  SetFirmware();
  SetMetadata();
  ASSERT_FALSE(driver_test().StartDriver().is_ok());
}

TEST_F(BtHciBroadcomTest, ControllerReturnsBdaddrEventWithoutBdaddrParam) {
  // Set an invalid mac address in the metadata so that a ReadBdaddr command is sent to get
  // fallback address.
  SetMetadata(DEVICE_METADATA_MAC_ADDRESS, kMacAddress, kMacAddress.size() - 1);
  //  Respond to ReadBdaddr command with a command complete (which doesn't include the bdaddr).
  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.transport_device_.SetCustomizedReply(std::vector<uint8_t>(
        kCommandCompleteEvent.data(), kCommandCompleteEvent.data() + kCommandCompleteEvent.size()));
  });

  // Ensure loading the firmware succeeds.
  SetFirmware();

  // Initialization should still succeed (an error will be logged, but it's not fatal).
  ASSERT_TRUE(driver_test().StartDriver().is_ok());
}

TEST_F(BtHciBroadcomTest, SendsPowerCapWhenNeeded) {
  SetMetadata();
  //  Respond to SetInfo command with a controller needing PowerCap
  driver_test().RunInEnvironmentTypeContext(
      [](TestEnvironment& env) { env.transport_device_.SetSerialPid(PDEV_PID_BCM4381A1); });
  // Ensure loading the firmware succeeds.
  SetFirmware();

  // Initialization should succeed
  ASSERT_TRUE(driver_test().StartDriver().is_ok());

  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    ASSERT_TRUE(env.transport_device_.GetReceivedOpCodes().contains(kBcmSetPowerCapCmdOpCode));
  });
}

TEST_F(BtHciBroadcomTest, VendorProtocolUnknownMethod) {
  SetFirmware();
  SetMetadata();
  ASSERT_TRUE(driver_test().StartDriver().is_ok());

  OpenVendorWithHciTransportClient();

  fidl::Arena arena;
  std::vector<uint8_t> packet = {1};
  auto packet_view = fidl::VectorView<uint8_t>::FromExternal(packet);
  auto result = hci_transport_client_->Send(fhbt::wire::SentPacket::WithAcl(arena, packet_view));

  ASSERT_EQ(result.status(), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(BtHciBroadcomInitializedTest, EncodeSetAclPrioritySuccessWithParametersHighSink) {
  std::array<uint8_t, kBcmSetAclPriorityCmdSize> result_buffer;
  fidl::Arena arena;
  auto builder = fhbt::wire::VendorSetAclPriorityParams::Builder(arena);
  builder.connection_handle(0xFF00);
  builder.priority(fhbt::wire::VendorAclPriority::kHigh);
  builder.direction(fhbt::wire::VendorAclDirection::kSink);

  auto command = fhbt::wire::VendorCommand::WithSetAclPriority(arena, builder.Build());
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
  fidl::Arena arena;
  auto builder = fhbt::wire::VendorSetAclPriorityParams::Builder(arena);
  builder.connection_handle(0xFF00);
  builder.priority(fhbt::wire::VendorAclPriority::kNormal);
  builder.direction(fhbt::wire::VendorAclDirection::kSource);

  auto command = fhbt::wire::VendorCommand::WithSetAclPriority(arena, builder.Build());
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
