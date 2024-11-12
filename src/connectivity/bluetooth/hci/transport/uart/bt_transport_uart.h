// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_TRANSPORT_UART_BT_TRANSPORT_UART_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_TRANSPORT_UART_BT_TRANSPORT_UART_H_

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/wire.h>
#include <fidl/fuchsia.hardware.serialimpl/cpp/driver/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async/cpp/wait.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fit/thread_checker.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/event.h>
#include <threads.h>
#include <zircon/device/bt-hci.h>

#include <mutex>

#include <sdk/lib/driver/logging/cpp/logger.h>

namespace bt_transport_uart {

// HCI UART packet indicators
enum BtHciPacketIndicator : uint8_t {
  kHciNone = 0,
  kHciCommand = 1,
  kHciAclData = 2,
  kHciSco = 3,
  kHciEvent = 4,
};

class BtTransportUart;
class ScoConnectionServer : public fidl::Server<fuchsia_hardware_bluetooth::ScoConnection> {
  using SendHandler = fit::function<void(std::vector<uint8_t>&, fit::function<void(void)>)>;
  using StopHandler = fit::function<void(void)>;
  using AckReceiveHandler = fit::function<void(void)>;

 public:
  explicit ScoConnectionServer(SendHandler send_handler, StopHandler stop_handler,
                               AckReceiveHandler ack_receive_handler);

  // fuchsia_hardware_bluetooth::ScoConnection overrides.
  void Send(SendRequest& request, SendCompleter::Sync& completer) override;
  void AckReceive(AckReceiveCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;
  void handle_unknown_method(
      ::fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::ScoConnection> metadata,
      ::fidl::UnknownMethodCompleter::Sync& completer) override;

 private:
  SendHandler send_handler_;
  StopHandler stop_handler_;
  AckReceiveHandler ack_receive_handler_;
  std::vector<uint8_t> write_buffer_;
};

class BtTransportUart
    : public fdf::DriverBase,
      public fidl::WireAsyncEventHandler<fuchsia_driver_framework::NodeController>,
      public fidl::Server<fuchsia_hardware_bluetooth::HciTransport>,
      public fdf::WireServer<fuchsia_hardware_serialimpl::Device>,
      public fidl::Server<fuchsia_hardware_bluetooth::Snoop> {
 public:
  // If |dispatcher| is non-null, it will be used instead of a new work thread.
  // tests.
  explicit BtTransportUart(fdf::DriverStartArgs start_args,
                           fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;
  void Stop() override { fdf::DriverBase::Stop(); }

  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override {}

  // fuchsia_hardware_bluetooth::HciTransport protocol overrides.
  void Send(SendRequest& request, SendCompleter::Sync& completer) override;
  void AckReceive(AckReceiveCompleter::Sync& completer) override;
  void ConfigureSco(
      fidl::Server<fuchsia_hardware_bluetooth::HciTransport>::ConfigureScoRequest& request,
      fidl::Server<fuchsia_hardware_bluetooth::HciTransport>::ConfigureScoCompleter::Sync&
          completer) override;
  void handle_unknown_method(
      ::fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::HciTransport> metadata,
      ::fidl::UnknownMethodCompleter::Sync& completer) override;

  // fuchsia_hardware_serialimpl::Device FIDL request handler implementation.
  void GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) override;
  void Config(ConfigRequestView request, fdf::Arena& arena,
              ConfigCompleter::Sync& completer) override;
  void Enable(EnableRequestView request, fdf::Arena& arena,
              EnableCompleter::Sync& completer) override;
  void Read(fdf::Arena& arena, ReadCompleter::Sync& completer) override;
  void Write(WriteRequestView request, fdf::Arena& arena, WriteCompleter::Sync& completer) override;
  void CancelAll(fdf::Arena& arena, CancelAllCompleter::Sync& completer) override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_serialimpl::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // fidl::Server<fuchsia_hardware_bluetooth::Snoop> overrides:
  void AcknowledgePackets(AcknowledgePacketsRequest& request,
                          AcknowledgePacketsCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Snoop> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // Used by tests only.
  fit::function<void(void)> WaitForSnoopCallback();
  fit::function<void(void)> WaitForScoConnectionCallback();
  uint64_t GetAckedSnoopSeq();

 private:
  // Returns length of current event packet being received
  // Must only be called in the read callback (HciHandleUartReadEvents).
  size_t EventPacketLength();

  // Returns length of current ACL data packet being received
  // Must only be called in the read callback (HciHandleUartReadEvents).
  size_t AclPacketLength();

  // Returns length of current SCO data packet being received
  // Must only be called in the read callback (HciHandleUartReadEvents).
  size_t ScoPacketLength();

  void SendSnoop(fidl::VectorView<uint8_t>& packet,
                 fuchsia_hardware_bluetooth::wire::SnoopPacket::Tag type,
                 fuchsia_hardware_bluetooth::wire::PacketDirection direction);

  void HciBeginShutdown();

  void OnScoData(std::vector<uint8_t>& packet, fit::function<void(void)> callback);
  void OnScoStop();

  void ProcessOnePacketFromSendQueue();
  void SerialWrite(const std::vector<uint8_t>& data, fit::function<void(void)> callback);

  // Queues a read callback for async serial on the dispatcher.
  void QueueUartRead();
  void HciHandleUartReadEvents(const uint8_t* buf, size_t length);

  // Handle the acknowledgements of received packets from the host. Called by |AckReceive| handler
  // of both ScoConnection and HciTransport protocol.
  void OnAckReceive();

  // Reads the next packet chunk from |uart_src| into |buffer| and increments |buffer_offset| and
  // |uart_src| by the number of bytes read. If a complete packet is read, it will be written to
  // |channel|.
  using PacketLengthFunction = size_t (BtTransportUart::*)();
  void ProcessNextUartPacketFromReadBuffer(uint8_t* buffer, size_t buffer_size,
                                           size_t* buffer_offset, const uint8_t** uart_src,
                                           const uint8_t* uart_end,
                                           PacketLengthFunction get_packet_length,
                                           BtHciPacketIndicator packet_ind);

  void HciReadComplete(zx_status_t status, const uint8_t* buffer, size_t length);

  void HciTransportWriteComplete(zx_status_t status);

  zx_status_t ServeProtocols();

  fdf::WireClient<fuchsia_hardware_serialimpl::Device> serial_client_;

  std::atomic_bool shutting_down_ = false;

  // Used in HciTransport send data path.
  bool can_send_ = true;

  // type of current packet being read from the UART
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  BtHciPacketIndicator cur_uart_packet_type_ = kHciNone;

  // for accumulating HCI events
  // Must only be used in the UART read callback (HciHandleUartReadEvents). Set the limit to
  // kEventMax + 1 to reserve space for the packet indicator, the actual maximum length of the
  // buffer is 258.
  uint8_t event_buffer_[fuchsia_hardware_bluetooth::kEventMax + 1];
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  size_t event_buffer_offset_ = 0;

  // for accumulating ACL data packets
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  uint8_t acl_buffer_[fuchsia_hardware_bluetooth::kAclPacketMax];
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  size_t acl_buffer_offset_ = 0;

  // For accumulating SCO packets
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  uint8_t sco_buffer_[fuchsia_hardware_bluetooth::kScoPacketMax];
  // Must only be used in the UART read callback (HciHandleUartReadEvents).
  size_t sco_buffer_offset_ = 0;

  static constexpr size_t kUnackedReceivePacketLimit = 20;
  // Mark the read state of the driver, when the value is set to true, it means that the driver has
  // stopped reading data from the bus.
  bool read_stopped_ = false;

  // This number is to keep track of the number of packets that were sent through
  // |OnReceive| event but haven't received their ack from |AckReceive|s yet.
  size_t unacked_receive_packet_number_ = 0;

  struct BufferEntry {
    std::vector<uint8_t> data_;
    fit::function<void(void)> callback_;

    BufferEntry(std::vector<uint8_t>&& packet, fit::function<void(void)> callback) : data_(packet) {
      callback_ = std::move(callback);
    }
  };

  // The send queue shouldn't be infinitely long, set a reasonable limit for the queue, so that when
  // the limit is hit, it could indicate something goes wrong.(e.g. serial bus stucks)
  static constexpr uint32_t kSendQueueLimit = 20;

  // The buffer pool to store incoming SCO, ACL and command packets.
  std::queue<std::vector<uint8_t>> available_buffers_;

  // Queuing the data packets before the bus is available to send. The data buffer comes from
  // |available_buffers_|. The driver will reply the FIDL completer to notify the caller of
  // "ScoConnection::Send" or "HciTransport::Send" after it takes out a packet from the queue and
  // throws it into the bus, for each protocol, the client side shouldn't send more packets if there
  // are 10 sent packets' completers are not replied.
  std::queue<BufferEntry> send_queue_;

  // The task to fetch and send one packet in |send_queue_|, each task posted aims to deal with one
  // packet, if the bus is not available, the task will post itself again until one packet gets
  // sent.
  async::TaskClosureMethod<BtTransportUart, &BtTransportUart::ProcessOnePacketFromSendQueue>
      send_queue_task_{this};

  // Save the serial device pid for vendor drivers to fetch.
  uint32_t serial_pid_ = 0;

  std::optional<async::Loop> loop_;
  // In production, this is loop_.dispatcher(). In tests, this is the test dispatcher.
  async_dispatcher_t* dispatcher_ = nullptr;

  fidl::WireClient<fuchsia_driver_framework::Node> node_;
  fidl::WireClient<fuchsia_driver_framework::NodeController> node_controller_;

  std::optional<fidl::ServerBinding<fuchsia_hardware_bluetooth::Snoop>> snoop_server_;

  // When |snoop_seq_| - |acked_snoop_seq_| > |kSeqNumMaxDiff|, it means that the receiver of
  // snoop packets can't catch up the speed that the driver sends packets. Drop snoop packets if
  // this happens.
  static constexpr uint64_t kSeqNumMaxDiff = 20;
  uint64_t snoop_seq_ = 0;
  uint64_t acked_snoop_seq_ = 0;
  // Used for test only, to synchronize the snoop protocol setup.
  libsync::Completion snoop_setup_;
  libsync::Completion sco_connection_setup_;

  ScoConnectionServer sco_connection_server_;
  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::ScoConnection> sco_connection_binding_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_bluetooth::HciTransport>>
      hci_transport_binding_;

  // The task which runs to queue a uart read.
  async::TaskClosureMethod<BtTransportUart, &BtTransportUart::QueueUartRead> queue_read_task_{this};

  compat::SyncInitializedDeviceServer compat_server_;
};

}  // namespace bt_transport_uart

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_TRANSPORT_UART_BT_TRANSPORT_UART_H_
