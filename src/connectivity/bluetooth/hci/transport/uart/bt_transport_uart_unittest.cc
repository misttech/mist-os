// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bt_transport_uart.h"

#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/fdio/directory.h>
#include <zircon/device/bt-hci.h>

#include <queue>

#include <gtest/gtest.h>

namespace bt_transport_uart {
namespace fhbt = fuchsia_hardware_bluetooth;
namespace {

// HCI UART packet indicators
enum BtHciPacketIndicator {
  kHciNone = 0,
  kHciCommand = 1,
  kHciAclData = 2,
  kHciSco = 3,
  kHciEvent = 4,
};

class FakeSerialDevice : public fdf::WireServer<fuchsia_hardware_serialimpl::Device> {
 public:
  explicit FakeSerialDevice() = default;

  fuchsia_hardware_serialimpl::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_serialimpl::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                               fidl::kIgnoreBindingClosure),
    });
  }

  void QueueReadValue(std::vector<uint8_t> buffer) {
    read_rsp_queue_.emplace(std::move(buffer));
    MaybeRespondToRead();
  }

  void QueueWithoutSignaling(std::vector<uint8_t> buffer) {
    read_rsp_queue_.emplace(std::move(buffer));
  }

  std::vector<std::vector<uint8_t>>& writes() { return writes_; }

  bool canceled() const { return canceled_; }

  bool enabled() const { return enabled_; }

  void set_writes_paused(bool paused) {
    writes_paused_ = paused;
    MaybeRespondToWrite();
  }

  // fuchsia_hardware_serialimpl::Device FIDL implementation.
  void GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) override {
    fuchsia_hardware_serial::wire::SerialPortInfo info = {
        .serial_class = fuchsia_hardware_serial::Class::kBluetoothHci,
    };

    completer.buffer(arena).ReplySuccess(info);
  }
  void Config(ConfigRequestView request, fdf::Arena& arena,
              ConfigCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess();
  }

  void Enable(EnableRequestView request, fdf::Arena& arena,
              EnableCompleter::Sync& completer) override {
    enabled_ = request->enable;
    completer.buffer(arena).ReplySuccess();
  }

  void Read(fdf::Arena& arena, ReadCompleter::Sync& completer) override {
    // Serial only supports 1 pending read at a time.
    if (!enabled_) {
      completer.buffer(arena).ReplyError(ZX_ERR_IO_REFUSED);
      return;
    }
    read_request_.emplace(ReadRequest{std::move(arena), completer.ToAsync()});
    // The real serial async handler will call the callback immediately if there is data
    // available, do so here to simulate the recursive callstack there.
    MaybeRespondToRead();
  }

  void Write(WriteRequestView request, fdf::Arena& arena,
             WriteCompleter::Sync& completer) override {
    ASSERT_FALSE(write_request_);
    std::vector<uint8_t> buffer(request->data.data(), request->data.data() + request->data.count());
    writes_.emplace_back(std::move(buffer));
    write_request_.emplace(WriteRequest{std::move(arena), completer.ToAsync()});
    MaybeRespondToWrite();
  }

  void CancelAll(fdf::Arena& arena, CancelAllCompleter::Sync& completer) override {
    if (read_request_) {
      read_request_->completer.buffer(read_request_->arena).ReplyError(ZX_ERR_CANCELED);
      read_request_.reset();
    }
    canceled_ = true;
    completer.buffer(arena).Reply();
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_serialimpl::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    ZX_PANIC("Unknown method in Serial requests");
  }

 private:
  struct ReadRequest {
    fdf::Arena arena;
    ReadCompleter::Async completer;
  };
  struct WriteRequest {
    fdf::Arena arena;
    WriteCompleter::Async completer;
  };

  void MaybeRespondToRead() {
    if (read_rsp_queue_.empty() || !read_request_) {
      return;
    }
    std::vector<uint8_t> buffer = std::move(read_rsp_queue_.front());
    read_rsp_queue_.pop();
    // Callback may send new read request synchronously, so clear read_req_.
    ReadRequest request = std::move(read_request_.value());
    read_request_.reset();
    auto data_view = fidl::VectorView<uint8_t>::FromExternal(buffer);
    request.completer.buffer(request.arena).ReplySuccess(data_view);
  }

  void MaybeRespondToWrite() {
    if (writes_paused_) {
      return;
    }
    if (!write_request_) {
      return;
    }
    write_request_->completer.buffer(write_request_->arena).ReplySuccess();
    write_request_.reset();
  }

  bool enabled_ = false;
  bool canceled_ = false;
  bool writes_paused_ = false;
  std::optional<ReadRequest> read_request_;
  std::optional<WriteRequest> write_request_;
  std::queue<std::vector<uint8_t>> read_rsp_queue_;
  std::vector<std::vector<uint8_t>> writes_;

  fdf::ServerBindingGroup<fuchsia_hardware_serialimpl::Device> binding_group_;
};

class FixtureBasedTestEnvironment : fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    auto result = to_driver_vfs.AddService<fuchsia_hardware_serialimpl::Service>(
        serial_device_.GetInstanceHandler());
    return result;
  }

  FakeSerialDevice serial_device_;
};

class BackgroundFixtureConfig final {
 public:
  using DriverType = BtTransportUart;
  using EnvironmentType = FixtureBasedTestEnvironment;
};

class BtTransportUartTest : public ::testing::Test {
 public:
  BtTransportUartTest() = default;

  void SetUp() override {
    zx::result<> result = driver_test().StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
    EXPECT_TRUE(driver_test().RunInEnvironmentTypeContext<bool>(
        [](FixtureBasedTestEnvironment& env) { return env.serial_device_.enabled(); }));
  }

  void TearDown() override {
    // Only PrepareStop() will be called in driver_test().StopDriver(), Stop() won't be called.
    zx::result prepare_stop_result = driver_test().StopDriver();
    EXPECT_EQ(ZX_OK, prepare_stop_result.status_value());

    EXPECT_TRUE(driver_test().RunInEnvironmentTypeContext<bool>(
        [](FixtureBasedTestEnvironment& env) { return env.serial_device_.canceled(); }));
  }

  fdf_testing::BackgroundDriverTest<BackgroundFixtureConfig>& driver_test() { return driver_test_; }

 protected:
  fdf_testing::BackgroundDriverTest<BackgroundFixtureConfig> driver_test_;

  // The FIDL client used in the test to call into dut Hci server.
  fidl::WireSyncClient<fhbt::Hci> hci_client_;
};

class BtTransportUartHciTransportProtocolTest
    : public BtTransportUartTest,
      public fidl::AsyncEventHandler<fhbt::Snoop>,
      public fidl::WireAsyncEventHandler<fuchsia_hardware_bluetooth::HciTransport>,
      public fidl::WireAsyncEventHandler<fuchsia_hardware_bluetooth::ScoConnection> {
 public:
  void SetUp() override {
    BtTransportUartTest::SetUp();

    zx::result hci_transport_result = driver_test().Connect<fhbt::HciService::HciTransport>();
    ASSERT_EQ(ZX_OK, hci_transport_result.status_value());
    hci_transport_client_.Bind(std::move(hci_transport_result.value()),
                               fdf::Dispatcher::GetCurrent()->async_dispatcher(), this);

    // Open SCO connection.
    auto [sco_client_end, sco_server_end] = fidl::CreateEndpoints<fhbt::ScoConnection>().value();

    fidl::Arena arena;
    auto sco_request_builder = fhbt::wire::HciTransportConfigureScoRequest::Builder(arena);
    auto configure_sco_result = hci_transport_client_.sync()->ConfigureSco(
        sco_request_builder.connection(std::move(sco_server_end)).Build());
    ASSERT_EQ(configure_sco_result.status(), ZX_OK);

    sco_client_.Bind(std::move(sco_client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                     this);

    auto wait_sco = driver_test().RunInDriverContext<fit::function<void(void)>>(
        [](BtTransportUart& driver) { return driver.WaitForScoConnectionCallback(); });
    wait_sco();

    zx::result snoop_result = driver_test().Connect<fhbt::HciService::Snoop>();
    ASSERT_EQ(ZX_OK, snoop_result.status_value());
    snoop_client_.Bind(std::move(snoop_result.value()),
                       fdf::Dispatcher::GetCurrent()->async_dispatcher(), this);

    auto wait_snoop = driver_test().RunInDriverContext<fit::function<void(void)>>(
        [](BtTransportUart& driver) { return driver.WaitForSnoopCallback(); });
    wait_snoop();
  }

  void AckSnoop() {
    // Acknowledge the snoop packet with |current_snoop_seq_|.
    fit::result<fidl::OneWayError> result = snoop_client_->AcknowledgePackets(current_snoop_seq_);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to acknowledge snoop packet: %s",
              result.error_value().status_string());
    }
  }

  // fidl::AsyncEventHandler<fhbt::Snoop> overrides:
  void OnObservePacket(
      ::fidl::Event<::fuchsia_hardware_bluetooth::Snoop::OnObservePacket>& event) override {
    ASSERT_TRUE(event.sequence().has_value());
    current_snoop_seq_ = event.sequence().value();
    ASSERT_TRUE(event.direction().has_value());
    if (event.direction().value() == fhbt::PacketDirection::kHostToController) {
      switch (event.packet()->Which()) {
        case fhbt::SnoopPacket::Tag::kAcl:
          snoop_sent_acl_packets_.push_back(event.packet()->acl().value());
          break;
        case fhbt::SnoopPacket::Tag::kCommand:
          snoop_sent_command_packets_.push_back(event.packet()->command().value());
          break;
        case fhbt::SnoopPacket::Tag::kSco:
          snoop_sent_sco_packets_.push_back(event.packet()->sco().value());
          break;
        case fhbt::SnoopPacket::Tag::kIso:
          // No Iso data should be sent.
          // TODO(b/350753924): Handle ISO packets in this driver.
          FAIL();
        default:
          // Unknown packet type sent.
          FAIL();
      };
    } else if (event.direction().value() == fhbt::PacketDirection::kControllerToHost) {
      switch (event.packet()->Which()) {
        case fhbt::SnoopPacket::Tag::kEvent:
          snoop_received_event_packets_.push_back(event.packet()->event().value());
          break;
        case fhbt::SnoopPacket::Tag::kAcl:
          snoop_received_acl_packets_.push_back(event.packet()->acl().value());
          break;
        case fhbt::SnoopPacket::Tag::kSco:
          snoop_received_sco_packets_.push_back(event.packet()->sco().value());
          break;
        case fhbt::SnoopPacket::Tag::kIso:
          // No Iso data should be received.
          // TODO(b/350753924): Handle ISO packets in this driver.
          FAIL();
        default:
          // Unknown packet type received.
          FAIL();
      };
    }
  }
  void OnDroppedPackets(::fidl::Event<fhbt::Snoop::OnDroppedPackets>&) override {
    // Do nothing, the driver shouldn't drop any packet for now.
    FAIL();
  }
  void handle_unknown_event(fidl::UnknownEventMetadata<fhbt::Snoop> metadata) override { FAIL(); }

  void SetManualAckReceive(bool manual) { manual_ack_receive_ = manual; }

  void AckReceive() {
    auto ack_receive_result = hci_transport_client_.sync()->AckReceive();
    ASSERT_EQ(ack_receive_result.status(), ZX_OK);
  }

  // fuchsia_hardware_bluetooth::HciTransport event handler overrides
  void OnReceive(fhbt::wire::ReceivedPacket* packet) override {
    std::vector<uint8_t> received;
    switch (packet->Which()) {
      case fhbt::wire::ReceivedPacket::Tag::kEvent:
        received.assign(packet->event().get().begin(), packet->event().get().end());
        received_event_packets_.push_back(received);
        break;
      case fhbt::wire::ReceivedPacket::Tag::kAcl:
        received.assign(packet->acl().get().begin(), packet->acl().get().end());
        received_acl_packets_.push_back(received);
        break;
      case fhbt::wire::ReceivedPacket::Tag::kIso:
        // No Iso data should be received.
        FAIL();
      default:
        // Unknown packet type received.
        FAIL();
    }

    if (!manual_ack_receive_) {
      AckReceive();
    }
  }

  void handle_unknown_event(fidl::UnknownEventMetadata<fhbt::HciTransport> metadata) override {
    FAIL();
  }

  void on_fidl_error(fidl::UnbindInfo error) override { FAIL(); }

  // fuchsia_hardware_bluetooth::ScoConnection event handler overrides
  void OnReceive(fhbt::wire::ScoPacket* packet) override {
    std::vector<uint8_t> received;
    received.assign(packet->packet.begin(), packet->packet.end());
    received_sco_packets_.push_back(received);
    if (!manual_ack_receive_) {
      AckReceive();
    }
  }

  void handle_unknown_event(fidl::UnknownEventMetadata<fhbt::ScoConnection> metadata) override {
    FAIL();
  }

  const std::vector<std::vector<uint8_t>>& snoop_sent_acl_packets() const {
    return snoop_sent_acl_packets_;
  }

  const std::vector<std::vector<uint8_t>>& snoop_sent_command_packets() const {
    return snoop_sent_command_packets_;
  }

  const std::vector<std::vector<uint8_t>>& snoop_sent_sco_packets() const {
    return snoop_sent_sco_packets_;
  }

  const std::vector<std::vector<uint8_t>>& snoop_received_acl_packets() const {
    return snoop_received_acl_packets_;
  }

  const std::vector<std::vector<uint8_t>>& snoop_received_event_packets() const {
    return snoop_received_event_packets_;
  }

  const std::vector<std::vector<uint8_t>>& snoop_received_sco_packets() const {
    return snoop_received_sco_packets_;
  }

  const std::vector<std::vector<uint8_t>>& received_event_packets() const {
    return received_event_packets_;
  }

  const std::vector<std::vector<uint8_t>>& received_acl_packets() const {
    return received_acl_packets_;
  }

  const std::vector<std::vector<uint8_t>>& received_sco_packets() const {
    return received_sco_packets_;
  }

  const uint64_t& snoop_seq() { return current_snoop_seq_; }

  fidl::WireClient<fhbt::HciTransport> hci_transport_client_;
  fidl::WireClient<fhbt::ScoConnection> sco_client_;

 private:
  fidl::Client<fhbt::Snoop> snoop_client_;

  bool manual_ack_receive_ = false;
  uint64_t current_snoop_seq_ = 0;
  std::vector<std::vector<uint8_t>> received_event_packets_;
  std::vector<std::vector<uint8_t>> received_acl_packets_;
  std::vector<std::vector<uint8_t>> received_sco_packets_;

  std::vector<std::vector<uint8_t>> snoop_sent_acl_packets_;
  std::vector<std::vector<uint8_t>> snoop_sent_command_packets_;
  std::vector<std::vector<uint8_t>> snoop_sent_sco_packets_;
  std::vector<std::vector<uint8_t>> snoop_received_acl_packets_;
  std::vector<std::vector<uint8_t>> snoop_received_event_packets_;
  std::vector<std::vector<uint8_t>> snoop_received_sco_packets_;
};

TEST_F(BtTransportUartHciTransportProtocolTest, SendAclPackets) {
  const uint8_t kNumPackets = 20;
  fidl::Arena arena;
  for (uint8_t i = 0; i < kNumPackets; i++) {
    std::vector<uint8_t> kAclPacket = {i};

    auto packet_view = fidl::VectorView<uint8_t>::FromExternal(kAclPacket);
    auto send_result =
        hci_transport_client_.sync()->Send(fhbt::wire::SentPacket::WithAcl(arena, packet_view));
    ASSERT_EQ(send_result.status(), ZX_OK);
  }

  // Allow ACL packets to be processed and sent to the serial device.
  // This function waits until the condition in the lambda is satisfied, The default poll
  // interval is 10 msec.
  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInEnvironmentTypeContext<size_t>([](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.writes().size();
    }) == kNumPackets;
  });

  const std::vector<std::vector<uint8_t>> packets =
      driver_test().RunInEnvironmentTypeContext<const std::vector<std::vector<uint8_t>>>(
          [](FixtureBasedTestEnvironment& env) { return env.serial_device_.writes(); });
  ASSERT_EQ(packets.size(), kNumPackets);
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // A packet indicator should be prepended.
    std::vector<uint8_t> expected = {BtHciPacketIndicator::kHciAclData, i};
    EXPECT_EQ(packets[i], expected);
  }

  driver_test().runtime().RunUntil(
      [&]() { return snoop_sent_acl_packets().size() == kNumPackets; });
  for (uint8_t i = 0; i < kNumPackets; i++) {
    const std::vector<uint8_t> kExpectedSnoopPacket = {i};
    EXPECT_EQ(snoop_sent_acl_packets()[i], kExpectedSnoopPacket);
  }
  ASSERT_EQ(snoop_seq(), static_cast<uint64_t>(kNumPackets - 1));
}

TEST_F(BtTransportUartHciTransportProtocolTest, SnoopPacketsDropAndResume) {
  // The implicit assumption for this test case is that the driver can only tolerate
  // | kNumPackets | unacked snoop packets.
  const uint8_t kNumPackets = 21;
  fidl::Arena arena;
  for (uint8_t i = 0; i < kNumPackets; i++) {
    std::vector<uint8_t> kAclPacket = {i};

    auto packet_view = fidl::VectorView<uint8_t>::FromExternal(kAclPacket);
    auto send_result =
        hci_transport_client_.sync()->Send(fhbt::wire::SentPacket::WithAcl(arena, packet_view));
    ASSERT_EQ(send_result.status(), ZX_OK);
  }

  // Make sure the snoop of the packet just sent has been received.
  driver_test().runtime().RunUntil(
      [&]() { return snoop_sent_acl_packets().size() == kNumPackets; });

  {
    // Send a packet after driver starts dropping snoop packets.
    std::vector<uint8_t> kAclPacket = {kNumPackets};
    auto packet_view = fidl::VectorView<uint8_t>::FromExternal(kAclPacket);
    auto send_result =
        hci_transport_client_.sync()->Send(fhbt::wire::SentPacket::WithAcl(arena, packet_view));
    ASSERT_EQ(send_result.status(), ZX_OK);
  }

  // Acknowledge the latest snoop packet.
  AckSnoop();

  // Make sure the driver has received the snoop acknowledgement so that it won't drop the snoop
  // of the next packet we send.
  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInDriverContext<uint64_t>(
               [&](BtTransportUart& driver) { return driver.GetAckedSnoopSeq(); }) == snoop_seq();
  });

  {
    // Send another packet after driver resumes sending snoop packets.
    std::vector<uint8_t> kAclPacket = {kNumPackets + 1};
    auto packet_view = fidl::VectorView<uint8_t>::FromExternal(kAclPacket);
    auto send_result =
        hci_transport_client_.sync()->Send(fhbt::wire::SentPacket::WithAcl(arena, packet_view));
    ASSERT_EQ(send_result.status(), ZX_OK);
  }

  // Test start receiving snoop packet again.
  driver_test().runtime().RunUntil(
      [&]() { return snoop_sent_acl_packets().size() == (kNumPackets + 1); });

  for (uint8_t i = 0; i < kNumPackets; i++) {
    const std::vector<uint8_t> kExpectedSnoopPacket = {i};
    EXPECT_EQ(snoop_sent_acl_packets()[i], kExpectedSnoopPacket);
  }

  // Verify that packet with payload "kNumPackets" was dropped, and the payload of the packet
  // which locates at index "kNumPackets" is "kNumPackets + 1".
  const std::vector<uint8_t> kExpectedLastSnoopPacket = {kNumPackets + 1};
  EXPECT_EQ(snoop_sent_acl_packets()[kNumPackets], kExpectedLastSnoopPacket);
}

TEST_F(BtTransportUartHciTransportProtocolTest, AclReadableSignalIgnoredUntilFirstWriteCompletes) {
  // Delay completion of first write.
  driver_test().RunInEnvironmentTypeContext(
      [](FixtureBasedTestEnvironment& env) { return env.serial_device_.set_writes_paused(true); });

  const uint8_t kNumPackets = 2;
  fidl::Arena arena;
  for (uint8_t i = 0; i < kNumPackets; i++) {
    std::vector<uint8_t> kAclPacket = {i};

    auto packet_view = fidl::VectorView<uint8_t>::FromExternal(kAclPacket);

    hci_transport_client_->Send(fhbt::wire::SentPacket::WithAcl(arena, packet_view))
        .Then([](fidl::WireUnownedResult<fhbt::HciTransport::Send>& result) {
          ASSERT_EQ(result.status(), ZX_OK);
        });
  }

  // Wait until the first packet has been received by fake serial device.
  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInEnvironmentTypeContext<size_t>([](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.writes().size();
    }) == 1u;
  });

  // Call the first packet's completion callback. This should resume waiting for signals.
  driver_test().RunInEnvironmentTypeContext(
      [](FixtureBasedTestEnvironment& env) { return env.serial_device_.set_writes_paused(false); });

  // Wait for the readable signal to be processed, and both packets has been received by fake
  // serial device.
  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInEnvironmentTypeContext<size_t>([](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.writes().size();
    }) == kNumPackets;
  });

  const std::vector<std::vector<uint8_t>> packets =
      driver_test().RunInEnvironmentTypeContext<const std::vector<std::vector<uint8_t>>>(
          [](FixtureBasedTestEnvironment& env) { return env.serial_device_.writes(); });
  ASSERT_EQ(packets.size(), kNumPackets);
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // A packet indicator should be prepended.
    std::vector<uint8_t> expected = {BtHciPacketIndicator::kHciAclData, i};
    EXPECT_EQ(packets[i], expected);
  }
}

TEST_F(BtTransportUartHciTransportProtocolTest, ReceiveAclPacketsIn2Parts) {
  const std::vector<uint8_t> kAclBuffer = {
      BtHciPacketIndicator::kHciAclData,
      0x00,
      0x00,  // arbitrary header fields
      0x02,
      0x00,  // 2-byte length in little endian
      0x01,
      0x02,  // arbitrary payload
  };
  const std::vector<uint8_t> kRawData(kAclBuffer.begin() + 1, kAclBuffer.end());
  // Split the packet length field in half to test corner case.
  const std::vector<uint8_t> kPart1(kAclBuffer.begin(), kAclBuffer.begin() + 4);
  const std::vector<uint8_t> kPart2(kAclBuffer.begin() + 4, kAclBuffer.end());

  const size_t kNumPackets = 20;
  for (size_t i = 0; i < kNumPackets; i++) {
    driver_test().RunInEnvironmentTypeContext([&](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.QueueReadValue(kPart1);
    });
    driver_test().RunInEnvironmentTypeContext([&](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.QueueReadValue(kPart2);
    });
  }

  driver_test().runtime().RunUntil(
      [&]() { return received_acl_packets().size() == static_cast<size_t>(kNumPackets); });

  for (const std::vector<uint8_t>& packet : received_acl_packets()) {
    EXPECT_EQ(packet.size(), kRawData.size());
    EXPECT_EQ(packet, kRawData);
  }

  driver_test().runtime().RunUntil(
      [&]() { return snoop_received_acl_packets().size() == kNumPackets; });
  EXPECT_EQ(snoop_received_acl_packets().size(), kNumPackets);

  for (const std::vector<uint8_t>& packet : snoop_received_acl_packets()) {
    EXPECT_EQ(packet, kRawData);
  }

  ASSERT_EQ(snoop_seq(), static_cast<uint64_t>(kNumPackets - 1));
}

TEST_F(BtTransportUartHciTransportProtocolTest, ReceiveAclPacketsLotsInQueue) {
  const std::vector<uint8_t> kSerialAclBuffer = {
      BtHciPacketIndicator::kHciAclData,
      0x00,
      0x00,  // arbitrary header fields
      0x02,
      0x00,  // 2-byte length in little endian
      0x01,
      0x02,  // arbitrary payload
  };
  const std::vector<uint8_t> kAclBuffer(kSerialAclBuffer.begin() + 1, kSerialAclBuffer.end());
  // Split the packet length field in half to test corner case.
  const std::vector<uint8_t> kPart1(kSerialAclBuffer.begin(), kSerialAclBuffer.begin() + 4);
  const std::vector<uint8_t> kPart2(kSerialAclBuffer.begin() + 4, kSerialAclBuffer.end());

  const size_t kNumPackets = 1000;
  for (size_t i = 0; i < kNumPackets; i++) {
    driver_test().RunInEnvironmentTypeContext([&](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.QueueWithoutSignaling(kPart1);
    });
    driver_test().RunInEnvironmentTypeContext([&](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.QueueWithoutSignaling(kPart2);
    });
  }
  driver_test().RunInEnvironmentTypeContext(
      [&](FixtureBasedTestEnvironment& env) { return env.serial_device_.QueueReadValue(kPart1); });
  driver_test().RunInEnvironmentTypeContext(
      [&](FixtureBasedTestEnvironment& env) { return env.serial_device_.QueueReadValue(kPart2); });

  // Wait Until all the packets to be received.
  driver_test().runtime().RunUntil(
      [&]() { return received_acl_packets().size() == static_cast<size_t>(kNumPackets + 1); });

  ASSERT_EQ(received_acl_packets().size(), static_cast<size_t>(kNumPackets + 1));
  for (const std::vector<uint8_t>& packet : received_acl_packets()) {
    EXPECT_EQ(packet.size(), kAclBuffer.size());
    EXPECT_EQ(packet, kAclBuffer);
  }
}

TEST_F(BtTransportUartHciTransportProtocolTest, ReceiveAclPacketsWithFlowControl) {
  // Don't ack received packets from the fake host.
  SetManualAckReceive(true);

  std::vector<uint8_t> kSerialAclBuffer = {
      BtHciPacketIndicator::kHciAclData,
      0x00,
      0x00,  // arbitrary header fields
      0x02,
      0x00,  // 2-byte length in little endian
      0x01,
      0x00,  // payload
  };
  std::vector<uint8_t> kAclBuffer(kSerialAclBuffer.begin() + 1, kSerialAclBuffer.end());

  // The packet number exceeds the limit of unacked packets in the driver. The limit is now 10.
  const size_t kNumPackets = 30;
  const size_t kUnackedLimit = 20;
  for (size_t i = 0; i < kNumPackets; i++) {
    driver_test().RunInEnvironmentTypeContext([&](FixtureBasedTestEnvironment& env) {
      // Store the sequence number in packet payload.
      kSerialAclBuffer[6] = static_cast<uint8_t>(i);
      return env.serial_device_.QueueReadValue(kSerialAclBuffer);
    });
  }

  driver_test().runtime().RunUntil(
      [&]() { return received_acl_packets().size() == kUnackedLimit; });

  // The last packet should be the |kUnackedLimit|th packet, and this verification should never
  // flake because no more ACL packet should be received at this point.
  kAclBuffer[5] = kUnackedLimit - 1;
  EXPECT_EQ(received_acl_packets().back(), kAclBuffer);

  // Ack half of the acked packets and the driver will poll the rest of the packets from bus.
  for (size_t i = 0; i < (kUnackedLimit / 2); i++) {
    AckReceive();
  }

  driver_test().runtime().RunUntil([&]() { return received_acl_packets().size() == kNumPackets; });

  // The last packet received should be the last packet sent by the test this time.
  kAclBuffer[5] = kNumPackets - 1;
  EXPECT_EQ(received_acl_packets().back(), kAclBuffer);
}

TEST_F(BtTransportUartHciTransportProtocolTest, SendHciCommands) {
  const std::vector<uint8_t> kUartCmd0 = {
      BtHciPacketIndicator::kHciCommand,  // UART packet indicator
      0x00,                               // arbitrary payload
  };
  std::vector<uint8_t> kCmd0(kUartCmd0.begin() + 1, kUartCmd0.end());

  fidl::Arena arena;
  {
    auto packet_view = fidl::VectorView<uint8_t>::FromExternal(kCmd0);
    auto send_result =
        hci_transport_client_.sync()->Send(fhbt::wire::SentPacket::WithCommand(arena, packet_view));
    ASSERT_EQ(send_result.status(), ZX_OK);
  }

  // Wait until the first packet is received.
  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInEnvironmentTypeContext<size_t>([](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.writes().size();
    }) == 1u;
  });

  const std::vector<uint8_t> kUartCmd1 = {
      BtHciPacketIndicator::kHciCommand,  // UART packet indicator
      0x01,                               // arbitrary payload
  };
  std::vector<uint8_t> kCmd1(kUartCmd1.begin() + 1, kUartCmd1.end());

  {
    auto packet_view = fidl::VectorView<uint8_t>::FromExternal(kCmd1);
    auto send_result =
        hci_transport_client_.sync()->Send(fhbt::wire::SentPacket::WithCommand(arena, packet_view));
    ASSERT_EQ(send_result.status(), ZX_OK);
  }

  // Wait until the second packet is received.
  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInEnvironmentTypeContext<size_t>([](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.writes().size();
    }) == 2u;
  });

  const std::vector<std::vector<uint8_t>> packets =
      driver_test().RunInEnvironmentTypeContext<const std::vector<std::vector<uint8_t>>>(
          [](FixtureBasedTestEnvironment& env) { return env.serial_device_.writes(); });
  EXPECT_EQ(packets[0], kUartCmd0);
  EXPECT_EQ(packets[1], kUartCmd1);

  driver_test().runtime().RunUntil([&]() { return snoop_sent_command_packets().size() == 2u; });
  EXPECT_EQ(snoop_sent_command_packets()[0], kCmd0);
  EXPECT_EQ(snoop_sent_command_packets()[1], kCmd1);

  ASSERT_EQ(snoop_seq(), 1u);
}

TEST_F(BtTransportUartHciTransportProtocolTest,
       CommandReadableSignalIgnoredUntilFirstWriteCompletes) {
  // Delay completion of first write.
  driver_test().RunInEnvironmentTypeContext(
      [](FixtureBasedTestEnvironment& env) { return env.serial_device_.set_writes_paused(true); });

  const std::vector<uint8_t> kUartCmd0 = {
      BtHciPacketIndicator::kHciCommand,  // UART packet indicator
      0x00,                               // arbitrary payload
  };
  std::vector<uint8_t> kCmd0(kUartCmd0.begin() + 1, kUartCmd0.end());

  fidl::Arena arena;
  {
    auto packet_view = fidl::VectorView<uint8_t>::FromExternal(kCmd0);
    hci_transport_client_->Send(fhbt::wire::SentPacket::WithCommand(arena, packet_view))
        .Then([](fidl::WireUnownedResult<fhbt::HciTransport::Send>& result) {
          ASSERT_EQ(result.status(), ZX_OK);
        });
  }

  const std::vector<uint8_t> kUartCmd1 = {
      BtHciPacketIndicator::kHciCommand,  // UART packet indicator
      0x01,                               // arbitrary payload
  };
  std::vector<uint8_t> kCmd1(kUartCmd1.begin() + 1, kUartCmd1.end());

  {
    auto packet_view = fidl::VectorView<uint8_t>::FromExternal(kCmd1);
    hci_transport_client_->Send(fhbt::wire::SentPacket::WithCommand(arena, packet_view))
        .Then([](fidl::WireUnownedResult<fhbt::HciTransport::Send>& result) {
          ASSERT_EQ(result.status(), ZX_OK);
        });
  }

  // Wait until the first packet is received.
  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInEnvironmentTypeContext<size_t>([](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.writes().size();
    }) == 1u;
  });

  // Make sure the number of packet received never run over 1 before the pause is released.
  EXPECT_FALSE(driver_test().runtime().RunWithTimeoutOrUntil(
      [&]() {
        return driver_test().RunInEnvironmentTypeContext<size_t>(
                   [](FixtureBasedTestEnvironment& env) {
                     return env.serial_device_.writes().size();
                   }) > 1u;
      },
      zx::msec(500)));

  // Call the first command's completion callback. This should resume waiting for signals.
  driver_test().RunInEnvironmentTypeContext(
      [](FixtureBasedTestEnvironment& env) { return env.serial_device_.set_writes_paused(false); });

  // Wait for the readable signal to be processed and the second packet is received.
  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInEnvironmentTypeContext<size_t>([](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.writes().size();
    }) == 2u;
  });

  const std::vector<std::vector<uint8_t>> packets =
      driver_test().RunInEnvironmentTypeContext<const std::vector<std::vector<uint8_t>>>(
          [](FixtureBasedTestEnvironment& env) { return env.serial_device_.writes(); });
  EXPECT_EQ(packets[0], kUartCmd0);
  EXPECT_EQ(packets[1], kUartCmd1);
}

TEST_F(BtTransportUartHciTransportProtocolTest, ReceiveManyHciEventsSplitIntoTwoResponses) {
  const std::vector<uint8_t> kSerialEventBuffer = {
      BtHciPacketIndicator::kHciEvent,
      0x01,  // event code
      0x02,  // parameter_total_size
      0x03,  // arbitrary parameter
      0x04   // arbitrary parameter
  };
  const std::vector<uint8_t> kEventBuffer(kSerialEventBuffer.begin() + 1, kSerialEventBuffer.end());
  const std::vector<uint8_t> kPart1(kSerialEventBuffer.begin(), kSerialEventBuffer.begin() + 3);
  const std::vector<uint8_t> kPart2(kSerialEventBuffer.begin() + 3, kSerialEventBuffer.end());

  const size_t kNumEvents = 20;
  for (size_t i = 0; i < kNumEvents; i++) {
    driver_test().RunInEnvironmentTypeContext([&](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.QueueReadValue(kPart1);
    });
    driver_test().RunInEnvironmentTypeContext([&](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.QueueReadValue(kPart2);
    });
  }

  // Wait for all the packets to be received.
  driver_test().runtime().RunUntil([&]() { return received_event_packets().size() == kNumEvents; });

  ASSERT_EQ(received_event_packets().size(), kNumEvents);
  for (const std::vector<uint8_t>& event : received_event_packets()) {
    EXPECT_EQ(event, kEventBuffer);
  }

  driver_test().runtime().RunUntil(
      [&]() { return snoop_received_event_packets().size() == kNumEvents; });
  for (const std::vector<uint8_t>& packet : snoop_received_event_packets()) {
    EXPECT_EQ(packet, kEventBuffer);
  }

  ASSERT_EQ(snoop_seq(), static_cast<uint64_t>(kNumEvents - 1));
}

TEST_F(BtTransportUartHciTransportProtocolTest, SendScoPackets) {
  const size_t kNumPackets = 20;
  for (size_t i = 0; i < kNumPackets; i++) {
    std::vector<uint8_t> kScoPacket = {static_cast<uint8_t>(i)};

    auto packet_view = fidl::VectorView<uint8_t>::FromExternal(kScoPacket);
    auto send_result = sco_client_.sync()->Send(packet_view);
    ASSERT_EQ(send_result.status(), ZX_OK);
  }
  // Allow SCO packets to be processed and sent to the serial device.
  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInEnvironmentTypeContext<size_t>([](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.writes().size();
    }) == kNumPackets;
  });

  const std::vector<std::vector<uint8_t>> packets =
      driver_test().RunInEnvironmentTypeContext<const std::vector<std::vector<uint8_t>>>(
          [](FixtureBasedTestEnvironment& env) { return env.serial_device_.writes(); });
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // A packet indicator should be prepended.
    std::vector<uint8_t> expected = {BtHciPacketIndicator::kHciSco, i};
    EXPECT_EQ(packets[i], expected);
  }

  driver_test().runtime().RunUntil(
      [&]() { return snoop_sent_sco_packets().size() == kNumPackets; });
  for (uint8_t i = 0; i < kNumPackets; i++) {
    const std::vector<uint8_t> kExpectedSnoopPacket = {i};
    EXPECT_EQ(snoop_sent_sco_packets()[i], kExpectedSnoopPacket);
  }
}

TEST_F(BtTransportUartHciTransportProtocolTest, ScoReadableSignalIgnoredUntilFirstWriteCompletes) {
  // Delay completion of first write.
  driver_test().RunInEnvironmentTypeContext(
      [](FixtureBasedTestEnvironment& env) { return env.serial_device_.set_writes_paused(true); });

  const uint8_t kNumPackets = 2;
  for (uint8_t i = 0; i < kNumPackets; i++) {
    std::vector<uint8_t> kScoPacket = {i};
    auto packet_view = fidl::VectorView<uint8_t>::FromExternal(kScoPacket);

    sco_client_->Send(packet_view)
        .Then([](fidl::WireUnownedResult<fhbt::ScoConnection::Send>& result) {
          ASSERT_EQ(result.status(), ZX_OK);
        });
  }

  // Wait for the first packet to be received.
  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInEnvironmentTypeContext<size_t>([](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.writes().size();
    }) == 1u;
  });

  // Call the first packet's completion callback. This should resume waiting for signals.
  driver_test().RunInEnvironmentTypeContext(
      [](FixtureBasedTestEnvironment& env) { return env.serial_device_.set_writes_paused(false); });

  // Wait for the readable signal to be processed and the second packet to be received.
  driver_test().runtime().RunUntil([&]() {
    return driver_test().RunInEnvironmentTypeContext<size_t>([](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.writes().size();
    }) == kNumPackets;
  });

  const std::vector<std::vector<uint8_t>> packets =
      driver_test().RunInEnvironmentTypeContext<const std::vector<std::vector<uint8_t>>>(
          [](FixtureBasedTestEnvironment& env) { return env.serial_device_.writes(); });
  ASSERT_EQ(packets.size(), kNumPackets);
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // A packet indicator should be prepended.
    std::vector<uint8_t> expected = {BtHciPacketIndicator::kHciSco, i};
    EXPECT_EQ(packets[i], expected);
  }

  driver_test().runtime().RunUntil(
      [&]() { return snoop_sent_sco_packets().size() == kNumPackets; });
  for (uint8_t i = 0; i < kNumPackets; i++) {
    const std::vector<uint8_t> kExpectedSnoopPacket = {i};
    EXPECT_EQ(snoop_sent_sco_packets()[i], kExpectedSnoopPacket);
  }
}

TEST_F(BtTransportUartHciTransportProtocolTest, ReceiveScoPacketsIn2Parts) {
  const std::vector<uint8_t> kSerialScoBuffer = {
      BtHciPacketIndicator::kHciSco,  // Snoop packet flag
      0x07,
      0x08,  // arbitrary header fields
      0x01,  // 1-byte payload length in little endian
      0x02,  // arbitrary payload
  };
  const std::vector<uint8_t> kScoBuffer(kSerialScoBuffer.begin() + 1, kSerialScoBuffer.end());
  // Split the packet before length field to test corner case.
  const std::vector<uint8_t> kPart1(kSerialScoBuffer.begin(), kSerialScoBuffer.begin() + 3);
  const std::vector<uint8_t> kPart2(kSerialScoBuffer.begin() + 3, kSerialScoBuffer.end());

  const size_t kNumPackets = 20;
  for (size_t i = 0; i < kNumPackets; i++) {
    driver_test().RunInEnvironmentTypeContext([&](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.QueueReadValue(kPart1);
    });
    driver_test().RunInEnvironmentTypeContext([&](FixtureBasedTestEnvironment& env) {
      return env.serial_device_.QueueReadValue(kPart2);
    });
  }

  // Wait for all the packets to be received.
  driver_test().runtime().RunUntil([&]() { return received_sco_packets().size() == kNumPackets; });

  for (const std::vector<uint8_t>& packet : received_sco_packets()) {
    EXPECT_EQ(packet.size(), kScoBuffer.size());
    EXPECT_EQ(packet, kScoBuffer);
  }

  driver_test().runtime().RunUntil(
      [&]() { return snoop_received_sco_packets().size() == kNumPackets; });

  for (uint8_t i = 0; i < kNumPackets; i++) {
    EXPECT_EQ(snoop_received_sco_packets()[i], kScoBuffer);
  }
}

}  // namespace
}  // namespace bt_transport_uart
