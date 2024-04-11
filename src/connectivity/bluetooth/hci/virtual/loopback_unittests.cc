// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/cpp/task.h>
#include <lib/ddk/driver.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/assert.h>
#include <zircon/device/bt-hci.h>
#include <zircon/status.h>

#include <fbl/string_buffer.h>

#include "loopback.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace {

// HCI UART packet indicators
enum PacketIndicator {
  kHciNone = 0,
  kHciCommand = 1,
  kHciAclData = 2,
  kHciSco = 3,
  kHciEvent = 4,
};

class LoopbackTest : public ::testing::Test {
 public:
  explicit LoopbackTest() {}

  void SetUp() override {
    FX_LOGS(TRACE) << "SetUp";

    zx::channel server_end;
    ASSERT_EQ(zx::channel::create(0, &serial_chan_, &server_end), ZX_OK);

    auto dev = std::make_unique<bt_hci_virtual::LoopbackDevice>(root_device_.get());
    auto server_channel = server_end.release();
    ASSERT_EQ(dev->Bind(server_channel, std::string_view("bt-transport-loopback-test")), ZX_OK);
    FX_LOGS(TRACE) << "made device";
    RunUntilIdle();

    // The driver runtime has taken ownership of |dev|.
    [[maybe_unused]] bt_hci_virtual::LoopbackDevice* unused = dev.release();

    FX_LOGS(TRACE) << "release device";
    ASSERT_EQ(1u, root_dev()->child_count());
    ASSERT_TRUE(dut());

    // Connect to the DUT's Vendor protocol server.
    auto endpoints = fidl::Endpoints<fuchsia_hardware_bluetooth::Hci>::Create();
    hci_client_.Bind(std::move(endpoints.client));

    libsync::Completion bind;
    async::PostTask(hci_dispatcher_->async_dispatcher(), [&]() {
      fidl::BindServer(hci_dispatcher_->async_dispatcher(), std::move(endpoints.server),
                       dut()->GetDeviceContext<bt_hci_virtual::LoopbackDevice>());
      bind.Signal();
    });
    bind.Wait();

    // Configure wait for readable signal on serial channel.
    serial_chan_readable_wait_.set_object(serial_chan_.get());
    zx_status_t wait_begin_status =
        serial_chan_readable_wait_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    FX_LOGS(TRACE) << "serial_chan_readable_wait_";
    ASSERT_EQ(wait_begin_status, ZX_OK) << zx_status_get_string(wait_begin_status);
  }

  void TearDown() override {
    FX_LOGS(TRACE) << "TearDown";
    serial_chan_readable_wait_.Cancel();
    FX_LOGS(TRACE) << "Cancel";
    serial_chan_.reset();
    FX_LOGS(TRACE) << "Reset";
    RunUntilIdle();
    FX_LOGS(TRACE) << "RunLoopUntilIdle 1";
    dut()->UnbindOp();
    RunUntilIdle();
    FX_LOGS(TRACE) << "RunLoopUntilIdle 2";
    EXPECT_EQ(dut()->UnbindReplyCallStatus(), ZX_OK);
    dut()->ReleaseOp();
  }

  void RunUntilIdle() { mock_ddk::GetDriverRuntime()->RunUntilIdle(); }

  const std::vector<std::vector<uint8_t>>& received_serial_packets() const {
    return serial_chan_received_packets_;
  }

  zx::channel* serial_chan() { return &serial_chan_; }

  MockDevice* root_dev() const { return root_device_.get(); }

  MockDevice* dut() const { return root_device_->GetLatestChild(); }

  fidl::WireSyncClient<fuchsia_hardware_bluetooth::Hci> hci_client_;

 private:
  void OnChannelReady(async_dispatcher_t*, async::WaitBase* wait, zx_status_t status,
                      const zx_packet_signal_t* signal) {
    ASSERT_EQ(status, ZX_OK);
    ASSERT_TRUE(signal->observed & ZX_CHANNEL_READABLE);

    // Make byte buffer arbitrarily large enough to hold test packets.
    std::vector<uint8_t> bytes(255);
    uint32_t actual_bytes;
    zx_status_t read_status = serial_chan_.read(
        /*flags=*/0, bytes.data(), /*handles=*/nullptr, static_cast<uint32_t>(bytes.size()),
        /*num_handles=*/0, &actual_bytes, /*actual_handles=*/nullptr);
    ASSERT_EQ(read_status, ZX_OK);
    bytes.resize(actual_bytes);
    serial_chan_received_packets_.push_back(std::move(bytes));

    // The wait needs to be restarted.
    zx_status_t wait_begin_status = wait->Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    ASSERT_EQ(wait_begin_status, ZX_OK) << zx_status_get_string(wait_begin_status);
  }

  std::shared_ptr<MockDevice> root_device_ = MockDevice::FakeRootParent();

  fdf::UnownedSynchronizedDispatcher hci_dispatcher_ =
      mock_ddk::GetDriverRuntime()->StartBackgroundDispatcher();

  zx::channel serial_chan_;
  std::vector<std::vector<uint8_t>> serial_chan_received_packets_;
  async::WaitMethod<LoopbackTest, &LoopbackTest::OnChannelReady> serial_chan_readable_wait_{
      this, zx_handle_t(), ZX_CHANNEL_READABLE};
};

// Test fixture that opens all channels and has helpers for reading/writing data.
class LoopbackHciProtocolTest : public LoopbackTest {
 public:
  void SetUp() override {
    LoopbackTest::SetUp();

    zx::channel cmd_chan_driver_end;
    ASSERT_EQ(zx::channel::create(/*flags=*/0, &cmd_chan_, &cmd_chan_driver_end), ZX_OK);
    {
      auto result = hci_client_->OpenCommandChannel(std::move(cmd_chan_driver_end));
      ASSERT_TRUE(result.ok());
      ASSERT_FALSE(result->is_error());
    }

    // Configure wait for readable signal on command channel.
    cmd_chan_readable_wait_.set_object(cmd_chan_.get());
    zx_status_t wait_begin_status =
        cmd_chan_readable_wait_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    ASSERT_EQ(wait_begin_status, ZX_OK) << zx_status_get_string(wait_begin_status);

    zx::channel acl_chan_driver_end;
    ASSERT_EQ(zx::channel::create(/*flags=*/0, &acl_chan_, &acl_chan_driver_end), ZX_OK);
    {
      auto result = hci_client_->OpenAclDataChannel(std::move(acl_chan_driver_end));
      ASSERT_TRUE(result.ok());
      ASSERT_FALSE(result->is_error());
    }

    // Configure wait for readable signal on ACL channel.
    acl_chan_readable_wait_.set_object(acl_chan_.get());
    wait_begin_status =
        acl_chan_readable_wait_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    ASSERT_EQ(wait_begin_status, ZX_OK) << zx_status_get_string(wait_begin_status);

    zx::channel sco_chan_driver_end;
    ASSERT_EQ(zx::channel::create(/*flags=*/0, &sco_chan_, &sco_chan_driver_end), ZX_OK);
    {
      auto result = hci_client_->OpenScoDataChannel(std::move(sco_chan_driver_end));
      ASSERT_TRUE(result.ok());
      ASSERT_FALSE(result->is_error());
    }
    // Configure wait for readable signal on SCO channel.
    sco_chan_readable_wait_.set_object(sco_chan_.get());
    wait_begin_status =
        sco_chan_readable_wait_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    ASSERT_EQ(wait_begin_status, ZX_OK) << zx_status_get_string(wait_begin_status);

    zx::channel snoop_chan_driver_end;
    ZX_ASSERT(zx::channel::create(/*flags=*/0, &snoop_chan_, &snoop_chan_driver_end) == ZX_OK);
    {
      auto result = hci_client_->OpenSnoopChannel(std::move(snoop_chan_driver_end));
      ASSERT_TRUE(result.ok());
      ASSERT_FALSE(result->is_error());
    }
    // Configure wait for readable signal on snoop channel.
    snoop_chan_readable_wait_.set_object(snoop_chan_.get());
    wait_begin_status =
        snoop_chan_readable_wait_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    ASSERT_EQ(wait_begin_status, ZX_OK) << zx_status_get_string(wait_begin_status);
  }

  void TearDown() override {
    cmd_chan_readable_wait_.Cancel();
    cmd_chan_.reset();

    acl_chan_readable_wait_.Cancel();
    acl_chan_.reset();

    sco_chan_readable_wait_.Cancel();
    sco_chan_.reset();

    snoop_chan_readable_wait_.Cancel();
    snoop_chan_.reset();

    LoopbackTest::TearDown();
  }

  const std::vector<std::vector<uint8_t>>& hci_events() const { return cmd_chan_received_packets_; }

  const std::vector<std::vector<uint8_t>>& snoop_packets() const {
    return snoop_chan_received_packets_;
  }

  const std::vector<std::vector<uint8_t>>& received_acl_packets() const {
    return acl_chan_received_packets_;
  }

  const std::vector<std::vector<uint8_t>>& received_sco_packets() const {
    return sco_chan_received_packets_;
  }

  zx::channel* cmd_chan() { return &cmd_chan_; }

  zx::channel* acl_chan() { return &acl_chan_; }

  zx::channel* sco_chan() { return &sco_chan_; }

 private:
  // This method is shared by the waits for all channels. |wait| is used to differentiate which wait
  // called the method.
  void OnChannelReady(async_dispatcher_t*, async::WaitBase* wait, zx_status_t status,
                      const zx_packet_signal_t* signal) {
    ASSERT_EQ(status, ZX_OK);
    ASSERT_TRUE(signal->observed & ZX_CHANNEL_READABLE);

    zx::unowned_channel chan;
    std::vector<std::vector<uint8_t>>* received_packets = nullptr;
    if (wait == &cmd_chan_readable_wait_) {
      chan = zx::unowned_channel(cmd_chan_);
      received_packets = &cmd_chan_received_packets_;
    } else if (wait == &snoop_chan_readable_wait_) {
      chan = zx::unowned_channel(snoop_chan_);
      received_packets = &snoop_chan_received_packets_;
    } else if (wait == &acl_chan_readable_wait_) {
      chan = zx::unowned_channel(acl_chan_);
      received_packets = &acl_chan_received_packets_;
    } else if (wait == &sco_chan_readable_wait_) {
      chan = zx::unowned_channel(sco_chan_);
      received_packets = &sco_chan_received_packets_;
    } else {
      ADD_FAILURE() << "unexpected channel in OnChannelReady";
      return;
    }

    // Make byte buffer arbitrarily large enough to hold test packets.
    std::vector<uint8_t> bytes(255);
    uint32_t actual_bytes;
    zx_status_t read_status = chan->read(
        /*flags=*/0, bytes.data(), /*handles=*/nullptr, static_cast<uint32_t>(bytes.size()),
        /*num_handles=*/0, &actual_bytes, /*actual_handles=*/nullptr);
    ASSERT_EQ(read_status, ZX_OK);
    bytes.resize(actual_bytes);
    received_packets->push_back(std::move(bytes));

    // The wait needs to be restarted.
    zx_status_t wait_begin_status = wait->Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    ASSERT_EQ(wait_begin_status, ZX_OK) << zx_status_get_string(wait_begin_status);
  }

  zx::channel cmd_chan_;
  zx::channel acl_chan_;
  zx::channel sco_chan_;
  zx::channel snoop_chan_;

  async::WaitMethod<LoopbackHciProtocolTest, &LoopbackHciProtocolTest::OnChannelReady>
      cmd_chan_readable_wait_{this, zx_handle_t(), ZX_CHANNEL_READABLE};
  async::WaitMethod<LoopbackHciProtocolTest, &LoopbackHciProtocolTest::OnChannelReady>
      snoop_chan_readable_wait_{this, zx_handle_t(), ZX_CHANNEL_READABLE};
  async::WaitMethod<LoopbackHciProtocolTest, &LoopbackHciProtocolTest::OnChannelReady>
      acl_chan_readable_wait_{this, zx_handle_t(), ZX_CHANNEL_READABLE};
  async::WaitMethod<LoopbackHciProtocolTest, &LoopbackHciProtocolTest::OnChannelReady>
      sco_chan_readable_wait_{this, zx_handle_t(), ZX_CHANNEL_READABLE};

  std::vector<std::vector<uint8_t>> cmd_chan_received_packets_;
  std::vector<std::vector<uint8_t>> snoop_chan_received_packets_;
  std::vector<std::vector<uint8_t>> acl_chan_received_packets_;
  std::vector<std::vector<uint8_t>> sco_chan_received_packets_;
};

TEST_F(LoopbackTest, Lifecycle) {}

TEST_F(LoopbackTest, VendorProtocolSmokeTest) {
  auto endpoints = fidl::Endpoints<fuchsia_hardware_bluetooth::Vendor>::Create();

  // Bind client end
  fidl::WireSyncClient vendor_client(std::move(endpoints.client));

  // Bind server end
  auto server_dispatcher = mock_ddk::GetDriverRuntime()->StartBackgroundDispatcher();
  fidl::BindServer(server_dispatcher->async_dispatcher(), std::move(endpoints.server),
                   dut()->GetDeviceContext<bt_hci_virtual::LoopbackDevice>());

  auto result = vendor_client->GetFeatures();
  ASSERT_TRUE(result.ok());

  EXPECT_EQ(result->features, fuchsia_hardware_bluetooth::BtVendorFeatures{});
}

TEST_F(LoopbackHciProtocolTest, SendAclPackets) {
  const uint8_t kNumPackets = 25;
  for (uint8_t i = 0; i < kNumPackets; i++) {
    const std::vector<uint8_t> kAclPacket = {i};
    zx_status_t write_status =
        acl_chan()->write(/*flags=*/0, kAclPacket.data(), static_cast<uint32_t>(kAclPacket.size()),
                          /*handles=*/nullptr,
                          /*num_handles=*/0);
    ASSERT_EQ(write_status, ZX_OK);
  }
  // Allow ACL packets to be processed and sent to the serial device.
  RunUntilIdle();

  const std::vector<std::vector<uint8_t>>& packets = received_serial_packets();
  ASSERT_EQ(packets.size(), kNumPackets);
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // A packet indicator should be prepended.
    std::vector<uint8_t> expected = {PacketIndicator::kHciAclData, i};
    EXPECT_EQ(packets[i], expected);
  }

  ASSERT_EQ(snoop_packets().size(), kNumPackets);
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // Snoop packets should have a snoop packet flag prepended (NOT a UART packet indicator).
    const std::vector<uint8_t> kExpectedSnoopPacket = {BT_HCI_SNOOP_TYPE_ACL,  // Snoop packet flag
                                                       i};
    EXPECT_EQ(snoop_packets()[i], kExpectedSnoopPacket);
  }
}

TEST_F(LoopbackHciProtocolTest, ReceiveAclPacketsIn2Parts) {
  const std::vector<uint8_t> kSnoopAclBuffer = {
      BT_HCI_SNOOP_TYPE_ACL | BT_HCI_SNOOP_FLAG_RECV,  // Snoop packet flag
      0x00,
      0x00,  // arbitrary header fields
      0x02,
      0x00,  // 2-byte length in little endian
      0x01,
      0x02,  // arbitrary payload
  };
  std::vector<uint8_t> kSerialAclBuffer = kSnoopAclBuffer;
  kSerialAclBuffer[0] = PacketIndicator::kHciAclData;
  const std::vector<uint8_t> kAclBuffer(kSnoopAclBuffer.begin() + 1, kSnoopAclBuffer.end());
  // Split the packet length field in half to test corner case.
  const std::vector<uint8_t> kPart1(kSerialAclBuffer.begin(), kSerialAclBuffer.begin() + 4);
  const std::vector<uint8_t> kPart2(kSerialAclBuffer.begin() + 4, kSerialAclBuffer.end());

  const int kNumPackets = 20;
  for (int i = 0; i < kNumPackets; i++) {
    zx_status_t write_status =
        serial_chan()->write(/*flags=*/0, kPart1.data(), static_cast<uint32_t>(kPart1.size()),
                             /*handles=*/nullptr,
                             /*num_handles=*/0);
    ASSERT_EQ(write_status, ZX_OK);
    write_status =
        serial_chan()->write(/*flags=*/0, kPart2.data(), static_cast<uint32_t>(kPart2.size()),
                             /*handles=*/nullptr,
                             /*num_handles=*/0);
    ASSERT_EQ(write_status, ZX_OK);

    RunUntilIdle();
  }

  ASSERT_EQ(received_acl_packets().size(), static_cast<size_t>(kNumPackets));
  for (const std::vector<uint8_t>& packet : received_acl_packets()) {
    EXPECT_EQ(packet.size(), kAclBuffer.size());
    EXPECT_EQ(packet, kAclBuffer);
  }

  RunUntilIdle();
  ASSERT_EQ(snoop_packets().size(), static_cast<size_t>(kNumPackets));
  for (const std::vector<uint8_t>& packet : snoop_packets()) {
    EXPECT_EQ(packet, kSnoopAclBuffer);
  }
}

TEST_F(LoopbackHciProtocolTest, SendHciCommands) {
  const std::vector<uint8_t> kSnoopCmd0 = {
      BT_HCI_SNOOP_TYPE_CMD,  // Snoop packet flag
      0x00,                   // arbitrary payload
  };
  const std::vector<uint8_t> kCmd0(kSnoopCmd0.begin() + 1, kSnoopCmd0.end());
  const std::vector<uint8_t> kUartCmd0 = {
      PacketIndicator::kHciCommand,  // UART packet indicator
      0x00,                          // arbitrary payload
  };
  zx_status_t write_status =
      cmd_chan()->write(/*flags=*/0, kCmd0.data(), static_cast<uint32_t>(kCmd0.size()),
                        /*handles=*/nullptr,
                        /*num_handles=*/0);
  EXPECT_EQ(write_status, ZX_OK);
  RunUntilIdle();
  EXPECT_EQ(received_serial_packets().size(), 1u);

  const std::vector<uint8_t> kSnoopCmd1 = {
      BT_HCI_SNOOP_TYPE_CMD,  // Snoop packet flag
      0x01,                   // arbitrary payload
  };
  const std::vector<uint8_t> kCmd1(kSnoopCmd1.begin() + 1, kSnoopCmd1.end());
  const std::vector<uint8_t> kUartCmd1 = {
      PacketIndicator::kHciCommand,  // UART packet indicator
      0x01,                          // arbitrary payload
  };
  write_status = cmd_chan()->write(/*flags=*/0, kCmd1.data(), static_cast<uint32_t>(kCmd1.size()),
                                   /*handles=*/nullptr,
                                   /*num_handles=*/0);
  EXPECT_EQ(write_status, ZX_OK);
  RunUntilIdle();

  const std::vector<std::vector<uint8_t>>& packets = received_serial_packets();
  ASSERT_EQ(packets.size(), 2u);
  EXPECT_EQ(packets[0], kUartCmd0);
  EXPECT_EQ(packets[1], kUartCmd1);

  RunUntilIdle();
  ASSERT_EQ(snoop_packets().size(), 2u);
  EXPECT_EQ(snoop_packets()[0], kSnoopCmd0);
  EXPECT_EQ(snoop_packets()[1], kSnoopCmd1);
}

TEST_F(LoopbackHciProtocolTest, ReceiveManyHciEventsSplitIntoTwoResponses) {
  const std::vector<uint8_t> kSnoopEventBuffer = {
      BT_HCI_SNOOP_TYPE_EVT | BT_HCI_SNOOP_FLAG_RECV,  // Snoop packet flag
      0x01,                                            // event code
      0x02,                                            // parameter_total_size
      0x03,                                            // arbitrary parameter
      0x04                                             // arbitrary parameter
  };
  const std::vector<uint8_t> kEventBuffer(kSnoopEventBuffer.begin() + 1, kSnoopEventBuffer.end());
  std::vector<uint8_t> kSerialEventBuffer = kSnoopEventBuffer;
  kSerialEventBuffer[0] = PacketIndicator::kHciEvent;
  const std::vector<uint8_t> kPart1(kSerialEventBuffer.begin(), kSerialEventBuffer.begin() + 3);
  const std::vector<uint8_t> kPart2(kSerialEventBuffer.begin() + 3, kSerialEventBuffer.end());

  const int kNumEvents = 20;
  for (int i = 0; i < kNumEvents; i++) {
    zx_status_t write_status =
        serial_chan()->write(/*flags=*/0, kPart1.data(), static_cast<uint32_t>(kPart1.size()),
                             /*handles=*/nullptr,
                             /*num_handles=*/0);
    ASSERT_EQ(write_status, ZX_OK);
    write_status =
        serial_chan()->write(/*flags=*/0, kPart2.data(), static_cast<uint32_t>(kPart2.size()),
                             /*handles=*/nullptr,
                             /*num_handles=*/0);
    ASSERT_EQ(write_status, ZX_OK);
    RunUntilIdle();
  }

  ASSERT_EQ(hci_events().size(), static_cast<size_t>(kNumEvents));
  for (const std::vector<uint8_t>& event : hci_events()) {
    EXPECT_EQ(event, kEventBuffer);
  }

  ASSERT_EQ(snoop_packets().size(), static_cast<size_t>(kNumEvents));
  for (const std::vector<uint8_t>& packet : snoop_packets()) {
    EXPECT_EQ(packet, kSnoopEventBuffer);
  }
}

TEST_F(LoopbackHciProtocolTest, SendScoPackets) {
  const uint8_t kNumPackets = 25;
  for (uint8_t i = 0; i < kNumPackets; i++) {
    const std::vector<uint8_t> kScoPacket = {i};
    zx_status_t write_status =
        sco_chan()->write(/*flags=*/0, kScoPacket.data(), static_cast<uint32_t>(kScoPacket.size()),
                          /*handles=*/nullptr,
                          /*num_handles=*/0);
    ASSERT_EQ(write_status, ZX_OK);
  }
  // Allow SCO packets to be processed and sent to the serial device.
  RunUntilIdle();

  const std::vector<std::vector<uint8_t>>& packets = received_serial_packets();
  ASSERT_EQ(packets.size(), kNumPackets);
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // A packet indicator should be prepended.
    std::vector<uint8_t> expected = {PacketIndicator::kHciSco, i};
    EXPECT_EQ(packets[i], expected);
  }

  ASSERT_EQ(snoop_packets().size(), kNumPackets);
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // Snoop packets should have a snoop packet flag prepended (NOT a UART packet indicator).
    const std::vector<uint8_t> kExpectedSnoopPacket = {BT_HCI_SNOOP_TYPE_SCO, i};
    EXPECT_EQ(snoop_packets()[i], kExpectedSnoopPacket);
  }
}

TEST_F(LoopbackHciProtocolTest, ReceiveScoPacketsIn2Parts) {
  const std::vector<uint8_t> kSnoopScoBuffer = {
      BT_HCI_SNOOP_TYPE_SCO | BT_HCI_SNOOP_FLAG_RECV,  // Snoop packet flag
      0x07,
      0x08,  // arbitrary header fields
      0x01,  // 1-byte payload length in little endian
      0x02,  // arbitrary payload
  };
  std::vector<uint8_t> kSerialScoBuffer = kSnoopScoBuffer;
  kSerialScoBuffer[0] = PacketIndicator::kHciSco;
  const std::vector<uint8_t> kScoBuffer(kSnoopScoBuffer.begin() + 1, kSnoopScoBuffer.end());
  // Split the packet before length field to test corner case.
  const std::vector<uint8_t> kPart1(kSerialScoBuffer.begin(), kSerialScoBuffer.begin() + 3);
  const std::vector<uint8_t> kPart2(kSerialScoBuffer.begin() + 3, kSerialScoBuffer.end());

  const int kNumPackets = 20;
  for (int i = 0; i < kNumPackets; i++) {
    zx_status_t write_status =
        serial_chan()->write(/*flags=*/0, kPart1.data(), static_cast<uint32_t>(kPart1.size()),
                             /*handles=*/nullptr,
                             /*num_handles=*/0);
    ASSERT_EQ(write_status, ZX_OK);
    write_status =
        serial_chan()->write(/*flags=*/0, kPart2.data(), static_cast<uint32_t>(kPart2.size()),
                             /*handles=*/nullptr,
                             /*num_handles=*/0);
    ASSERT_EQ(write_status, ZX_OK);
    RunUntilIdle();
  }

  ASSERT_EQ(received_sco_packets().size(), static_cast<size_t>(kNumPackets));
  for (const std::vector<uint8_t>& packet : received_sco_packets()) {
    EXPECT_EQ(packet.size(), kScoBuffer.size());
    EXPECT_EQ(packet, kScoBuffer);
  }

  RunUntilIdle();
  ASSERT_EQ(snoop_packets().size(), static_cast<size_t>(kNumPackets));
  for (const std::vector<uint8_t>& packet : snoop_packets()) {
    EXPECT_EQ(packet, kSnoopScoBuffer);
  }
}

}  // namespace
