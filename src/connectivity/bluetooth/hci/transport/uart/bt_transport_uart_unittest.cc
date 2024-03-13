// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bt_transport_uart.h"

#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdio/directory.h>
#include <zircon/device/bt-hci.h>

#include <queue>

#include <gtest/gtest.h>

namespace bt_transport_uart {
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

  const std::vector<std::vector<uint8_t>>& writes() const { return writes_; }

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

class TestEnvironmentLocal : public fdf_testing::TestEnvironment {
 public:
  zx::result<> Initialize(fidl::ServerEnd<fuchsia_io::Directory> incoming_directory_server_end) {
    return fdf_testing::TestEnvironment::Initialize(std::move(incoming_directory_server_end));
  }

  void AddService(fuchsia_hardware_serialimpl::Service::InstanceHandler&& handler) {
    zx::result result =
        incoming_directory().AddService<fuchsia_hardware_serialimpl::Service>(std::move(handler));
    EXPECT_TRUE(result.is_ok());
  }
};

class BtTransportUartTest : public ::testing::Test {
 public:
  BtTransportUartTest() = default;

  void SetUp() override {
    // Create start args
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    ASSERT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment with incoming directory returned from the start args
    zx::result init_result =
        test_environment_.SyncCall(&fdf_testing::TestEnvironment::Initialize,
                                   std::move(start_args->incoming_directory_server));
    EXPECT_EQ(ZX_OK, init_result.status_value());

    // Get service handler from the fake_serial_device_ object.
    auto handler = fake_serial_device_.SyncCall(&FakeSerialDevice::GetInstanceHandler);

    test_environment_.SyncCall(&TestEnvironmentLocal::AddService, std::move(handler));

    zx::result start_result = runtime_.RunToCompletion(driver_.SyncCall(
        &fdf_testing::DriverUnderTest<BtTransportUart>::Start, std::move(start_args->start_args)));
    EXPECT_EQ(ZX_OK, start_result.status_value());

    EXPECT_TRUE(fake_serial_device_.SyncCall(&FakeSerialDevice::enabled));

    // Open the svc directory in the driver's outgoing, and store a client to it.
    auto svc_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    EXPECT_EQ(ZX_OK, svc_endpoints.status_value());
    zx_status_t status = fdio_open_at(start_args->outgoing_directory_client.handle()->get(), "/svc",
                                      static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                      svc_endpoints->server.TakeChannel().release());
    ASSERT_EQ(ZX_OK, status);

    zx::result connect_result =
        component::ConnectAtMember<fuchsia_hardware_bluetooth::HciService::Hci>(
            std::move(svc_endpoints->client));

    ASSERT_EQ(ZX_OK, connect_result.status_value());
    hci_client_.Bind(std::move(connect_result.value()));
  }

  void TearDown() override {
    zx::result prepare_stop_result = runtime_.RunToCompletion(
        driver_.SyncCall(&fdf_testing::DriverUnderTest<BtTransportUart>::PrepareStop));
    EXPECT_EQ(ZX_OK, prepare_stop_result.status_value());

    EXPECT_TRUE(fake_serial_device_.SyncCall(&FakeSerialDevice::canceled));

    zx::result stop_result = driver_.SyncCall(&fdf_testing::DriverUnderTest<BtTransportUart>::Stop);
    EXPECT_EQ(ZX_OK, stop_result.status_value());
  }

  async_patterns::TestDispatcherBound<FakeSerialDevice>* serial() { return &fake_serial_device_; }

  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }

  async_dispatcher_t* driver_dispatcher() { return driver_dispatcher_->async_dispatcher(); }

  fdf_testing::DriverRuntime* runtime() { return &runtime_; }

  // The FIDL client used in the test to call into dut Hci server.
  fidl::WireSyncClient<fuchsia_hardware_bluetooth::Hci> hci_client_;

 private:
  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  // Env dispatcher runs in the background because we need to make sync calls into it.
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  // Driver dispatcher set as a background dispatcher. The protocols served by dut will run on
  // this dispatcher.
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();

  fdf::UnownedSynchronizedDispatcher hci_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};

  async_patterns::TestDispatcherBound<TestEnvironmentLocal> test_environment_{env_dispatcher(),
                                                                              std::in_place};

  async_patterns::TestDispatcherBound<FakeSerialDevice> fake_serial_device_{env_dispatcher(),
                                                                            std::in_place};

  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<BtTransportUart>> driver_{
      driver_dispatcher(), std::in_place};
};

// Test fixture that opens all channels and has helpers for reading/writing data.
class BtTransportUartHciProtocolTest : public BtTransportUartTest {
 public:
  void SetUp() override {
    BtTransportUartTest::SetUp();

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

    BtTransportUartTest::TearDown();
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
  // This method is shared by the waits for all channels. |wait| is used to differentiate which
  // wait called the method.
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

  async::WaitMethod<BtTransportUartHciProtocolTest, &BtTransportUartHciProtocolTest::OnChannelReady>
      cmd_chan_readable_wait_{this, zx_handle_t(), ZX_CHANNEL_READABLE};
  async::WaitMethod<BtTransportUartHciProtocolTest, &BtTransportUartHciProtocolTest::OnChannelReady>
      snoop_chan_readable_wait_{this, zx_handle_t(), ZX_CHANNEL_READABLE};
  async::WaitMethod<BtTransportUartHciProtocolTest, &BtTransportUartHciProtocolTest::OnChannelReady>
      acl_chan_readable_wait_{this, zx_handle_t(), ZX_CHANNEL_READABLE};
  async::WaitMethod<BtTransportUartHciProtocolTest, &BtTransportUartHciProtocolTest::OnChannelReady>
      sco_chan_readable_wait_{this, zx_handle_t(), ZX_CHANNEL_READABLE};

  std::vector<std::vector<uint8_t>> cmd_chan_received_packets_;
  std::vector<std::vector<uint8_t>> snoop_chan_received_packets_;
  std::vector<std::vector<uint8_t>> acl_chan_received_packets_;
  std::vector<std::vector<uint8_t>> sco_chan_received_packets_;
};

TEST_F(BtTransportUartHciProtocolTest, SendAclPackets) {
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
  // This function waits until the condition in the lambda is satisfied, The default poll interval
  // is 10 msec, the default wait timeout is 1 sec. The function returns true if it didn't timeout.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return serial()->SyncCall(&FakeSerialDevice::writes).size() == kNumPackets; }));

  const std::vector<std::vector<uint8_t>>& packets = serial()->SyncCall(&FakeSerialDevice::writes);
  ASSERT_EQ(packets.size(), kNumPackets);
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // A packet indicator should be prepended.
    std::vector<uint8_t> expected = {BtHciPacketIndicator::kHciAclData, i};
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

TEST_F(BtTransportUartHciProtocolTest, AclReadableSignalIgnoredUntilFirstWriteCompletes) {
  // Delay completion of first write.
  serial()->SyncCall(&FakeSerialDevice::set_writes_paused, true);

  const uint8_t kNumPackets = 2;
  for (uint8_t i = 0; i < kNumPackets; i++) {
    const std::vector<uint8_t> kAclPacket = {i};
    zx_status_t write_status =
        acl_chan()->write(/*flags=*/0, kAclPacket.data(), static_cast<uint32_t>(kAclPacket.size()),
                          /*handles=*/nullptr,
                          /*num_handles=*/0);
    ASSERT_EQ(write_status, ZX_OK);
  }

  // Wait until the first packet has been received by fake serial device.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return serial()->SyncCall(&FakeSerialDevice::writes).size() == 1u; }));

  // Call the first packet's completion callback. This should resume waiting for signals.
  serial()->SyncCall(&FakeSerialDevice::set_writes_paused, false);
  // Wait for the readable signal to be processed, and both packets has been received by fake serial
  // device.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return serial()->SyncCall(&FakeSerialDevice::writes).size() == kNumPackets; }));

  const std::vector<std::vector<uint8_t>>& packets = serial()->SyncCall(&FakeSerialDevice::writes);
  ASSERT_EQ(packets.size(), kNumPackets);
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // A packet indicator should be prepended.
    std::vector<uint8_t> expected = {BtHciPacketIndicator::kHciAclData, i};
    EXPECT_EQ(packets[i], expected);
  }
}

TEST_F(BtTransportUartHciProtocolTest, ReceiveAclPacketsIn2Parts) {
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
  kSerialAclBuffer[0] = BtHciPacketIndicator::kHciAclData;
  const std::vector<uint8_t> kAclBuffer(kSnoopAclBuffer.begin() + 1, kSnoopAclBuffer.end());
  // Split the packet length field in half to test corner case.
  const std::vector<uint8_t> kPart1(kSerialAclBuffer.begin(), kSerialAclBuffer.begin() + 4);
  const std::vector<uint8_t> kPart2(kSerialAclBuffer.begin() + 4, kSerialAclBuffer.end());

  const size_t kNumPackets = 20;
  for (size_t i = 0; i < kNumPackets; i++) {
    serial()->SyncCall(&FakeSerialDevice::QueueReadValue, kPart1);
    serial()->SyncCall(&FakeSerialDevice::QueueReadValue, kPart2);
  }

  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return received_acl_packets().size() == static_cast<size_t>(kNumPackets); }));

  for (const std::vector<uint8_t>& packet : received_acl_packets()) {
    EXPECT_EQ(packet.size(), kAclBuffer.size());
    EXPECT_EQ(packet, kAclBuffer);
  }

  EXPECT_TRUE(
      runtime()->RunWithTimeoutOrUntil([&]() { return snoop_packets().size() == kNumPackets; }));
  for (const std::vector<uint8_t>& packet : snoop_packets()) {
    EXPECT_EQ(packet, kSnoopAclBuffer);
  }
}

TEST_F(BtTransportUartHciProtocolTest, ReceiveAclPacketsLotsInQueue) {
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
  kSerialAclBuffer[0] = BtHciPacketIndicator::kHciAclData;
  const std::vector<uint8_t> kAclBuffer(kSnoopAclBuffer.begin() + 1, kSnoopAclBuffer.end());
  // Split the packet length field in half to test corner case.
  const std::vector<uint8_t> kPart1(kSerialAclBuffer.begin(), kSerialAclBuffer.begin() + 4);
  const std::vector<uint8_t> kPart2(kSerialAclBuffer.begin() + 4, kSerialAclBuffer.end());

  const size_t kNumPackets = 1000;
  for (size_t i = 0; i < kNumPackets; i++) {
    serial()->SyncCall(&FakeSerialDevice::QueueWithoutSignaling, kPart1);
    serial()->SyncCall(&FakeSerialDevice::QueueWithoutSignaling, kPart2);
  }
  serial()->SyncCall(&FakeSerialDevice::QueueReadValue, kPart1);
  serial()->SyncCall(&FakeSerialDevice::QueueReadValue, kPart2);

  // Wait Until all the packets to be received.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return received_acl_packets().size() == static_cast<size_t>(kNumPackets + 1); }));

  ASSERT_EQ(received_acl_packets().size(), static_cast<size_t>(kNumPackets + 1));
  for (const std::vector<uint8_t>& packet : received_acl_packets()) {
    EXPECT_EQ(packet.size(), kAclBuffer.size());
    EXPECT_EQ(packet, kAclBuffer);
  }

  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return snoop_packets().size() == kNumPackets + 1; }));
  for (const std::vector<uint8_t>& packet : snoop_packets()) {
    EXPECT_EQ(packet, kSnoopAclBuffer);
  }
}

TEST_F(BtTransportUartHciProtocolTest, SendHciCommands) {
  const std::vector<uint8_t> kSnoopCmd0 = {
      BT_HCI_SNOOP_TYPE_CMD,  // Snoop packet flag
      0x00,                   // arbitrary payload
  };
  const std::vector<uint8_t> kCmd0(kSnoopCmd0.begin() + 1, kSnoopCmd0.end());
  const std::vector<uint8_t> kUartCmd0 = {
      BtHciPacketIndicator::kHciCommand,  // UART packet indicator
      0x00,                               // arbitrary payload
  };
  zx_status_t write_status =
      cmd_chan()->write(/*flags=*/0, kCmd0.data(), static_cast<uint32_t>(kCmd0.size()),
                        /*handles=*/nullptr,
                        /*num_handles=*/0);
  EXPECT_EQ(write_status, ZX_OK);

  // Wait until the first packet is received.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return serial()->SyncCall(&FakeSerialDevice::writes).size() == 1u; }));

  const std::vector<uint8_t> kSnoopCmd1 = {
      BT_HCI_SNOOP_TYPE_CMD,  // Snoop packet flag
      0x01,                   // arbitrary payload
  };
  const std::vector<uint8_t> kCmd1(kSnoopCmd1.begin() + 1, kSnoopCmd1.end());
  const std::vector<uint8_t> kUartCmd1 = {
      BtHciPacketIndicator::kHciCommand,  // UART packet indicator
      0x01,                               // arbitrary payload
  };
  write_status = cmd_chan()->write(/*flags=*/0, kCmd1.data(), static_cast<uint32_t>(kCmd1.size()),
                                   /*handles=*/nullptr,
                                   /*num_handles=*/0);
  EXPECT_EQ(write_status, ZX_OK);

  // Wait until the second packet is received.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return serial()->SyncCall(&FakeSerialDevice::writes).size() == 2u; }));

  const std::vector<std::vector<uint8_t>>& packets = serial()->SyncCall(&FakeSerialDevice::writes);
  EXPECT_EQ(packets[0], kUartCmd0);
  EXPECT_EQ(packets[1], kUartCmd1);

  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil([&]() { return snoop_packets().size() == 2u; }));
  EXPECT_EQ(snoop_packets()[0], kSnoopCmd0);
  EXPECT_EQ(snoop_packets()[1], kSnoopCmd1);
}

TEST_F(BtTransportUartHciProtocolTest, CommandReadableSignalIgnoredUntilFirstWriteCompletes) {
  // Delay completion of first write.
  serial()->SyncCall(&FakeSerialDevice::set_writes_paused, true);

  const std::vector<uint8_t> kUartCmd0 = {
      BtHciPacketIndicator::kHciCommand,  // UART packet indicator
      0x00,                               // arbitrary payload
  };
  const std::vector<uint8_t> kCmd0(kUartCmd0.begin() + 1, kUartCmd0.end());
  zx_status_t write_status =
      cmd_chan()->write(/*flags=*/0, kCmd0.data(), static_cast<uint32_t>(kCmd0.size()),
                        /*handles=*/nullptr,
                        /*num_handles=*/0);
  EXPECT_EQ(write_status, ZX_OK);

  const std::vector<uint8_t> kUartCmd1 = {
      BtHciPacketIndicator::kHciCommand,  // UART packet indicator
      0x01,                               // arbitrary payload
  };
  const std::vector<uint8_t> kCmd1(kUartCmd1.begin() + 1, kUartCmd1.end());
  write_status = cmd_chan()->write(/*flags=*/0, kCmd1.data(), static_cast<uint32_t>(kCmd1.size()),
                                   /*handles=*/nullptr,
                                   /*num_handles=*/0);
  EXPECT_EQ(write_status, ZX_OK);

  // Wait until the first packet is received.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return serial()->SyncCall(&FakeSerialDevice::writes).size() == 1u; }));

  // Make sure the number of packet received never run over 1 before the pause is released.
  EXPECT_FALSE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return serial()->SyncCall(&FakeSerialDevice::writes).size() > 1u; }, zx::msec(500)));

  // Call the first command's completion callback. This should resume waiting for signals.
  serial()->SyncCall(&FakeSerialDevice::set_writes_paused, false);

  // Wait for the readable signal to be processed and the second packet is received.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return serial()->SyncCall(&FakeSerialDevice::writes).size() == 2u; }));

  const std::vector<std::vector<uint8_t>>& packets = serial()->SyncCall(&FakeSerialDevice::writes);
  EXPECT_EQ(packets[0], kUartCmd0);
  EXPECT_EQ(packets[1], kUartCmd1);
}

TEST_F(BtTransportUartHciProtocolTest, ReceiveManyHciEventsSplitIntoTwoResponses) {
  const std::vector<uint8_t> kSnoopEventBuffer = {
      BT_HCI_SNOOP_TYPE_EVT | BT_HCI_SNOOP_FLAG_RECV,  // Snoop packet flag
      0x01,                                            // event code
      0x02,                                            // parameter_total_size
      0x03,                                            // arbitrary parameter
      0x04                                             // arbitrary parameter
  };
  const std::vector<uint8_t> kEventBuffer(kSnoopEventBuffer.begin() + 1, kSnoopEventBuffer.end());
  std::vector<uint8_t> kSerialEventBuffer = kSnoopEventBuffer;
  kSerialEventBuffer[0] = BtHciPacketIndicator::kHciEvent;
  const std::vector<uint8_t> kPart1(kSerialEventBuffer.begin(), kSerialEventBuffer.begin() + 3);
  const std::vector<uint8_t> kPart2(kSerialEventBuffer.begin() + 3, kSerialEventBuffer.end());

  const size_t kNumEvents = 20;
  for (size_t i = 0; i < kNumEvents; i++) {
    serial()->SyncCall(&FakeSerialDevice::QueueReadValue, kPart1);
    serial()->SyncCall(&FakeSerialDevice::QueueReadValue, kPart2);
  }

  // Wait for all the packets to be received.
  EXPECT_TRUE(
      runtime()->RunWithTimeoutOrUntil([&]() { return hci_events().size() == kNumEvents; }));

  ASSERT_EQ(hci_events().size(), kNumEvents);
  for (const std::vector<uint8_t>& event : hci_events()) {
    EXPECT_EQ(event, kEventBuffer);
  }

  EXPECT_TRUE(
      runtime()->RunWithTimeoutOrUntil([&]() { return snoop_packets().size() == kNumEvents; }));
  for (const std::vector<uint8_t>& packet : snoop_packets()) {
    EXPECT_EQ(packet, kSnoopEventBuffer);
  }
}

TEST_F(BtTransportUartHciProtocolTest, SendScoPackets) {
  const size_t kNumPackets = 25;
  for (size_t i = 0; i < kNumPackets; i++) {
    const std::vector<uint8_t> kScoPacket = {static_cast<uint8_t>(i)};
    zx_status_t write_status =
        sco_chan()->write(/*flags=*/0, kScoPacket.data(), static_cast<uint32_t>(kScoPacket.size()),
                          /*handles=*/nullptr,
                          /*num_handles=*/0);
    ASSERT_EQ(write_status, ZX_OK);
  }
  // Allow SCO packets to be processed and sent to the serial device.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return serial()->SyncCall(&FakeSerialDevice::writes).size() == kNumPackets; }));

  const std::vector<std::vector<uint8_t>>& packets = serial()->SyncCall(&FakeSerialDevice::writes);
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // A packet indicator should be prepended.
    std::vector<uint8_t> expected = {BtHciPacketIndicator::kHciSco, i};
    EXPECT_EQ(packets[i], expected);
  }

  EXPECT_TRUE(
      runtime()->RunWithTimeoutOrUntil([&]() { return snoop_packets().size() == kNumPackets; }));
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // Snoop packets should have a snoop packet flag prepended (NOT a UART packet indicator).
    const std::vector<uint8_t> kExpectedSnoopPacket = {BT_HCI_SNOOP_TYPE_SCO, i};
    EXPECT_EQ(snoop_packets()[i], kExpectedSnoopPacket);
  }
}

TEST_F(BtTransportUartHciProtocolTest, ScoReadableSignalIgnoredUntilFirstWriteCompletes) {
  // Delay completion of first write.
  serial()->SyncCall(&FakeSerialDevice::set_writes_paused, true);

  const uint8_t kNumPackets = 2;
  for (uint8_t i = 0; i < kNumPackets; i++) {
    const std::vector<uint8_t> kScoPacket = {i};
    zx_status_t write_status =
        sco_chan()->write(/*flags=*/0, kScoPacket.data(), static_cast<uint32_t>(kScoPacket.size()),
                          /*handles=*/nullptr,
                          /*num_handles=*/0);
    ASSERT_EQ(write_status, ZX_OK);
  }

  // Wait for the first packet to be received.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return serial()->SyncCall(&FakeSerialDevice::writes).size() == 1u; }));

  // Call the first packet's completion callback. This should resume waiting for signals.
  serial()->SyncCall(&FakeSerialDevice::set_writes_paused, false);

  // Wait for the readable signal to be processed and the second packet to be received.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return serial()->SyncCall(&FakeSerialDevice::writes).size() == kNumPackets; }));

  const std::vector<std::vector<uint8_t>>& packets = serial()->SyncCall(&FakeSerialDevice::writes);
  ASSERT_EQ(packets.size(), kNumPackets);
  for (uint8_t i = 0; i < kNumPackets; i++) {
    // A packet indicator should be prepended.
    std::vector<uint8_t> expected = {BtHciPacketIndicator::kHciSco, i};
    EXPECT_EQ(packets[i], expected);
  }
}

TEST_F(BtTransportUartHciProtocolTest, ReceiveScoPacketsIn2Parts) {
  const std::vector<uint8_t> kSnoopScoBuffer = {
      BT_HCI_SNOOP_TYPE_SCO | BT_HCI_SNOOP_FLAG_RECV,  // Snoop packet flag
      0x07,
      0x08,  // arbitrary header fields
      0x01,  // 1-byte payload length in little endian
      0x02,  // arbitrary payload
  };
  std::vector<uint8_t> kSerialScoBuffer = kSnoopScoBuffer;
  kSerialScoBuffer[0] = BtHciPacketIndicator::kHciSco;
  const std::vector<uint8_t> kScoBuffer(kSnoopScoBuffer.begin() + 1, kSnoopScoBuffer.end());
  // Split the packet before length field to test corner case.
  const std::vector<uint8_t> kPart1(kSerialScoBuffer.begin(), kSerialScoBuffer.begin() + 3);
  const std::vector<uint8_t> kPart2(kSerialScoBuffer.begin() + 3, kSerialScoBuffer.end());

  const size_t kNumPackets = 20;
  for (size_t i = 0; i < kNumPackets; i++) {
    serial()->SyncCall(&FakeSerialDevice::QueueReadValue, kPart1);
    serial()->SyncCall(&FakeSerialDevice::QueueReadValue, kPart2);
  }

  // Wait for all the packets to be received.
  EXPECT_TRUE(runtime()->RunWithTimeoutOrUntil(
      [&]() { return received_sco_packets().size() == static_cast<size_t>(kNumPackets); }));

  for (const std::vector<uint8_t>& packet : received_sco_packets()) {
    EXPECT_EQ(packet.size(), kScoBuffer.size());
    EXPECT_EQ(packet, kScoBuffer);
  }

  EXPECT_TRUE(
      runtime()->RunWithTimeoutOrUntil([&]() { return snoop_packets().size() == kNumPackets; }));
  for (const std::vector<uint8_t>& packet : snoop_packets()) {
    EXPECT_EQ(packet, kSnoopScoBuffer);
  }
}

}  // namespace
}  // namespace bt_transport_uart
