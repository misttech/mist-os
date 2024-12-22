// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bt_transport_uart.h"

#include <assert.h>
#include <lib/async/default.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/sync/cpp/completion.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/status.h>

namespace bt_transport_uart {
namespace fhbt = fuchsia_hardware_bluetooth;

ScoConnectionServer::ScoConnectionServer(SendHandler send_handler, StopHandler stop_handler,
                                         AckReceiveHandler ack_receive_handler)
    : send_handler_(std::move(send_handler)),
      stop_handler_(std::move(stop_handler)),
      ack_receive_handler_(std::move(ack_receive_handler)) {
  // Pre-allocate the vector size to avoid resizing. Reserve the space for packet indicator by +1 on
  // the size.
  write_buffer_.reserve(fhbt::kScoPacketMax + 1);
}

// fhbt::ScoConnection overrides.
void ScoConnectionServer::Send(SendRequest& request, SendCompleter::Sync& completer) {
  write_buffer_.push_back(BtHciPacketIndicator::kHciSco);
  write_buffer_.insert(write_buffer_.end(), request.packet().begin(), request.packet().end());
  send_handler_(write_buffer_, [completer = completer.ToAsync()]() mutable { completer.Reply(); });
  write_buffer_.clear();
}

void ScoConnectionServer::AckReceive(AckReceiveCompleter::Sync& completer) {
  ack_receive_handler_();
}

void ScoConnectionServer::Stop(StopCompleter::Sync& completer) { stop_handler_(); }

void ScoConnectionServer::handle_unknown_method(
    ::fidl::UnknownMethodMetadata<fhbt::ScoConnection> metadata,
    ::fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in ScoConnection protocol, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

BtTransportUart::BtTransportUart(fuchsia_driver_framework::DriverStartArgs start_args,
                                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("bt-transport-uart", std::move(start_args), std::move(driver_dispatcher)),
      dispatcher_(dispatcher()),
      node_(fidl::WireClient(std::move(node()), dispatcher())),
      sco_connection_server_(fit::bind_member(this, &BtTransportUart::OnScoData),
                             fit::bind_member(this, &BtTransportUart::OnScoStop),
                             fit::bind_member(this, &BtTransportUart::OnAckReceive)) {
  // Pre-allocate the vector size to avoid resizing. Reserve the space for packet indicator by +1 on
  // the size.
  for (size_t i = 0; i < kSendQueueLimit; i++) {
    std::vector<uint8_t> buffer;
    buffer.reserve(fhbt::kAclPacketMax + 1);
    available_buffers_.push(std::move(buffer));
  }
}

zx::result<> BtTransportUart::Start() {
  FDF_LOG(DEBUG, "Start");

  zx::result<fdf::ClientEnd<fuchsia_hardware_serialimpl::Device>> client_end =
      incoming()->Connect<fuchsia_hardware_serialimpl::Service::Device>();
  if (client_end.is_error()) {
    FDF_LOG(ERROR, "Connect to fuchsia_hardware_serialimpl::Device protocol failed: %s",
            client_end.status_string());
    return zx::error(client_end.status_value());
  }

  serial_client_ = fdf::WireClient<fuchsia_hardware_serialimpl::Device>(
      std::move(client_end.value()), driver_dispatcher()->get());
  if (!serial_client_.is_valid()) {
    FDF_LOG(ERROR, "fuchsia_hardware_serialimpl::Device Client is not valid");
    return zx::error(ZX_ERR_BAD_HANDLE);
  }

  // pre-populate event packet indicators
  event_buffer_[0] = kHciEvent;
  event_buffer_offset_ = 1;
  acl_buffer_[0] = kHciAclData;
  acl_buffer_offset_ = 1;
  sco_buffer_[0] = kHciSco;
  sco_buffer_offset_ = 1;

  fdf::Arena arena('INIT');
  auto info_result = serial_client_.sync().buffer(arena)->GetInfo();
  if (!info_result.ok()) {
    FDF_LOG(ERROR, "hci_start: GetInfo failed with FIDL error %s", info_result.status_string());
    return zx::error(info_result.status());
  }
  if (info_result->is_error()) {
    FDF_LOG(ERROR, "hci_start: GetInfo failed with error %s",
            zx_status_get_string(info_result->error_value()));
    return zx::error(info_result->error_value());
  }

  if (info_result.value()->info.serial_class != fuchsia_hardware_serial::Class::kBluetoothHci) {
    FDF_LOG(ERROR, "hci_start: device class isn't BLUETOOTH_HCI");
    return zx::error(ZX_ERR_INTERNAL);
  }

  serial_pid_ = info_result.value()->info.serial_pid;

  auto enable_result = serial_client_.sync().buffer(arena)->Enable(true);
  if (!enable_result.ok()) {
    FDF_LOG(ERROR, "hci_start: Enable failed with FIDL error %s", enable_result.status_string());
    return zx::error(enable_result.status());
  }

  if (enable_result->is_error()) {
    FDF_LOG(ERROR, "hci_start: Enable failed with error %s",
            zx_status_get_string(enable_result->error_value()));
    return zx::error(enable_result->error_value());
  }

  zx_status_t status = ServeProtocols();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to serve protocols: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  // Start compat device server to forward metadata.
  zx::result compat_server_result =
      compat_server_.Initialize(incoming(), outgoing(), node_name(), "bt-transport-uart",
                                compat::ForwardMetadata::Some({DEVICE_METADATA_MAC_ADDRESS}));
  if (compat_server_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize device server: %s", compat_server_result.status_string());
    return compat_server_result.take_error();
  }

  // Add child node for the vendor driver to bind.
  fidl::Arena args_arena;

  // Build offers
  auto offers = compat_server_.CreateOffers2(args_arena);
  offers.push_back(fdf::MakeOffer2<fhbt::HciService>(args_arena));
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_serialimpl::Service>(args_arena));

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(args_arena)
                  .name("bt-transport-uart")
                  .offers2(std::move(offers))
                  .Build();

  auto controller_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_LOG(ERROR, "Create node controller end points failed: %s",
            zx_status_get_string(controller_endpoints.error_value()));
    return zx::error(controller_endpoints.error_value());
  }

  // Add bt-transport-uart child node.
  auto result =
      node_.sync()->AddChild(std::move(args), std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add bt-transport-uart node, FIDL error: %s", result.status_string());
    return zx::error(result.status());
  }

  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to add bt-transport-uart node: %u",
            static_cast<uint32_t>(result->error_value()));
    return zx::error(ZX_ERR_INTERNAL);
  }

  node_controller_.Bind(std::move(controller_endpoints->client), dispatcher(), this);

  return zx::ok();
}

void BtTransportUart::PrepareStop(fdf::PrepareStopCompleter completer) {
  FDF_LOG(TRACE, "Unbind");

  // We are now shutting down.  Make sure that any pending callbacks in
  // flight from the serial_impl are nerfed and that our thread is shut down.
  std::atomic_store_explicit(&shutting_down_, true, std::memory_order_relaxed);

  // Finish by making sure that all in flight transactions transactions have
  // been canceled.
  fdf::Arena arena('CANC');
  auto result = serial_client_.sync().buffer(arena)->CancelAll();
  if (!result.ok()) {
    FDF_LOG(ERROR, "hci_bind: CancelAll failed with FIDL error %s", result.status_string());
    completer(zx::error(result.status()));
    return;
  }

  FDF_LOG(TRACE, "PrepareStop complete");

  completer(zx::ok());
}

size_t BtTransportUart::EventPacketLength() {
  // payload length is in byte 2 of the packet
  // add 3 bytes for packet indicator, event code and length byte
  return event_buffer_offset_ > 2 ? event_buffer_[2] + 3 : 0;
}

size_t BtTransportUart::AclPacketLength() {
  // length is in bytes 3 and 4 of the packet
  // add 5 bytes for packet indicator, control info and length fields
  return acl_buffer_offset_ > 4 ? (acl_buffer_[3] | (acl_buffer_[4] << 8)) + 5 : 0;
}

size_t BtTransportUart::ScoPacketLength() {
  // payload length is byte 3 of the packet
  // add 4 bytes for packet indicator, handle, and length byte
  return sco_buffer_offset_ > 3 ? (sco_buffer_[3] + 4) : 0;
}

void BtTransportUart::SendSnoop(fidl::VectorView<uint8_t>& packet,
                                fuchsia_hardware_bluetooth::wire::SnoopPacket::Tag type,
                                fhbt::wire::PacketDirection direction) {
  if (!snoop_server_.has_value()) {
    return;
  }

  static bool log_emitted = false;
  if ((snoop_seq_ - acked_snoop_seq_) > kSeqNumMaxDiff) {
    if (!log_emitted) {
      FDF_LOG(
          WARNING,
          "Too many snoop packets not acked, current seq: %lu, acked seq: %lu, skipping snoop packets",
          snoop_seq_, acked_snoop_seq_);
      log_emitted = true;
    }
    return;
  }
  // Reset log when the acked sequence number catches up.
  log_emitted = false;

  fidl::Arena arena;
  auto builder = fhbt::wire::SnoopOnObservePacketRequest::Builder(arena);
  switch (type) {
    case fhbt::wire::SnoopPacket::Tag::kEvent:
      ZX_DEBUG_ASSERT(direction == fhbt::wire::PacketDirection::kControllerToHost);
      builder.packet(fhbt::wire::SnoopPacket::WithEvent(arena, packet));
      break;
    case fhbt::wire::SnoopPacket::Tag::kCommand:
      ZX_DEBUG_ASSERT(direction == fhbt::wire::PacketDirection::kHostToController);
      builder.packet(fhbt::wire::SnoopPacket::WithCommand(arena, packet));
      break;
    case fhbt::wire::SnoopPacket::Tag::kAcl:
      builder.packet(fhbt::wire::SnoopPacket::WithAcl(arena, packet));
      break;
    case fhbt::wire::SnoopPacket::Tag::kSco:
      builder.packet(fhbt::wire::SnoopPacket::WithSco(arena, packet));
      break;
    default:
      // TODO(b/350753924): Handle ISO packets in this driver.
      FDF_LOG(ERROR, "Unknown snoop packet type: %lu", static_cast<fidl_xunion_tag_t>(type));
  }
  builder.direction(direction);
  builder.sequence(snoop_seq_++);

  fidl::OneWayStatus result = fidl::WireSendEvent(*snoop_server_)->OnObservePacket(builder.Build());
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send snoop for sent packet: %s, unbinding snoop server",
            result.error().status_string());
    snoop_server_->Close(ZX_ERR_INTERNAL);
    snoop_server_.reset();
  }
}

void BtTransportUart::HciBeginShutdown() {
  bool was_shutting_down = shutting_down_.exchange(true, std::memory_order_relaxed);
  if (!was_shutting_down) {
    auto result = node_.UnbindMaybeGetEndpoint();
  }
}

void BtTransportUart::OnScoData(std::vector<uint8_t>& packet, fit::function<void(void)> callback) {
  if (available_buffers_.empty()) {
    FDF_LOG(ERROR, "Send queue is full, closing SCO connection");
    callback();
    sco_connection_binding_.RemoveBindings(&sco_connection_server_);
    return;
  }

  auto& buffer = available_buffers_.front();
  buffer = std::move(packet);
  send_queue_.emplace(std::move(buffer), std::move(callback));
  available_buffers_.pop();
  send_queue_task_.Post(dispatcher_);
}

void BtTransportUart::OnScoStop() {
  sco_connection_binding_.RemoveBindings(&sco_connection_server_);
}

void BtTransportUart::ProcessOnePacketFromSendQueue() {
  if (!can_send_) {
    return;
  }
  if (send_queue_.empty()) {
    return;
  }

  auto& buffer_entry = send_queue_.front();
  SerialWrite(buffer_entry.data_, std::move(buffer_entry.callback_));

  auto snoop_data = fidl::VectorView<uint8_t>::FromExternal(buffer_entry.data_.data() + 1,
                                                            buffer_entry.data_.size() - 1);

  fhbt::wire::SnoopPacket::Tag snoop_type = fhbt::wire::SnoopPacket::Tag::kIso;
  switch (buffer_entry.data_[0]) {
    case BtHciPacketIndicator::kHciAclData:
      snoop_type = fhbt::wire::SnoopPacket::Tag::kAcl;
      break;
    case BtHciPacketIndicator::kHciCommand:
      snoop_type = fhbt::wire::SnoopPacket::Tag::kCommand;
      break;
    case BtHciPacketIndicator::kHciSco:
      snoop_type = fhbt::wire::SnoopPacket::Tag::kSco;
      break;
    default:
      FDF_LOG(DEBUG, "Unsupported snoop sent packet type: %u", buffer_entry.data_[0]);
      send_queue_.pop();
      return;
  }
  SendSnoop(snoop_data, snoop_type, fhbt::wire::PacketDirection::kHostToController);
  buffer_entry.data_.clear();
  available_buffers_.push(std::move(buffer_entry.data_));
  send_queue_.pop();
}

void BtTransportUart::SerialWrite(const std::vector<uint8_t>& data,
                                  fit::function<void(void)> callback) {
  can_send_ = false;

  fdf::Arena arena('WRIT');
  auto data_view = fidl::VectorView<uint8_t>::FromExternal(const_cast<std::vector<uint8_t>&>(data));
  serial_client_.buffer(arena)->Write(data_view).ThenExactlyOnce(
      [this](fdf::WireUnownedResult<fuchsia_hardware_serialimpl::Device::Write>& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "hci_bind: Write failed with FIDL error %s", result.status_string());
          HciTransportWriteComplete(result.status());
          return;
        }

        if (result->is_error()) {
          FDF_LOG(ERROR, "hci_bind: Write failed with error %s",
                  zx_status_get_string(result->error_value()));
          HciTransportWriteComplete(result->error_value());
          return;
        }
        HciTransportWriteComplete(ZX_OK);
      });
  callback();
}

void BtTransportUart::HciHandleUartReadEvents(const uint8_t* buf, size_t length) {
  const uint8_t* const end = buf + length;
  while (buf < end) {
    if (cur_uart_packet_type_ == kHciNone) {
      // start of new packet. read packet type
      cur_uart_packet_type_ = static_cast<BtHciPacketIndicator>(*buf++);
    }
    switch (cur_uart_packet_type_) {
      case kHciEvent:
        ProcessNextUartPacketFromReadBuffer(event_buffer_, sizeof(event_buffer_),
                                            &event_buffer_offset_, &buf, end,
                                            &BtTransportUart::EventPacketLength, kHciEvent);
        break;
      case kHciAclData:
        ProcessNextUartPacketFromReadBuffer(acl_buffer_, sizeof(acl_buffer_), &acl_buffer_offset_,
                                            &buf, end, &BtTransportUart::AclPacketLength,
                                            kHciAclData);
        break;
      case kHciSco:
        ProcessNextUartPacketFromReadBuffer(sco_buffer_, sizeof(sco_buffer_), &sco_buffer_offset_,
                                            &buf, end, &BtTransportUart::ScoPacketLength, kHciSco);
        break;
      default:
        FDF_LOG(ERROR, "unsupported HCI packet type %u received. We may be out of sync",
                cur_uart_packet_type_);
        cur_uart_packet_type_ = kHciNone;
        return;
    }
  }
}

void BtTransportUart::OnAckReceive() {
  if (unacked_receive_packet_number_ == 0) {
    FDF_LOG(ERROR, "Receiving a packet ack when no received packet is pending ack.");
    return;
  }
  unacked_receive_packet_number_--;
  if (unacked_receive_packet_number_ == (kUnackedReceivePacketLimit / 2) && read_stopped_) {
    // Resume reading data from the uart data buffer if half of the unacked packets are acked.
    queue_read_task_.Post(dispatcher_);
    read_stopped_ = false;
  }
}

void BtTransportUart::ProcessNextUartPacketFromReadBuffer(uint8_t* buffer, size_t buffer_size,
                                                          size_t* buffer_offset,
                                                          const uint8_t** uart_src,
                                                          const uint8_t* uart_end,
                                                          PacketLengthFunction get_packet_length,
                                                          BtHciPacketIndicator packet_ind) {
  size_t packet_length = (this->*get_packet_length)();

  while (!packet_length && *uart_src < uart_end) {
    // read until we have enough to compute packet length
    buffer[*buffer_offset] = **uart_src;
    (*buffer_offset)++;
    (*uart_src)++;
    packet_length = (this->*get_packet_length)();
  }

  // Out of bytes, but we still don't know the packet length.  Just wait for
  // the next packet.
  if (!packet_length) {
    return;
  }

  if (packet_length > buffer_size) {
    FDF_LOG(ERROR,
            "packet_length is too large (%zu > %zu) during packet reassembly. Dropping and "
            "attempting to re-sync.",
            packet_length, buffer_size);

    // Reset the reassembly state machine.
    *buffer_offset = 1;
    cur_uart_packet_type_ = kHciNone;
    // Consume the rest of the UART buffer to indicate that it is corrupt.
    *uart_src = uart_end;
    return;
  }

  size_t remaining = uart_end - *uart_src;
  size_t copy_size = packet_length - *buffer_offset;
  if (copy_size > remaining) {
    copy_size = remaining;
  }

  ZX_ASSERT(*buffer_offset + copy_size <= buffer_size);
  memcpy(buffer + *buffer_offset, *uart_src, copy_size);
  *uart_src += copy_size;
  *buffer_offset += copy_size;

  if (*buffer_offset != packet_length) {
    // The packet is incomplete, the next chunk should continue the same packet.
    return;
  }

  fidl::Arena arena;
  auto fidl_vec = fidl::VectorView<uint8_t>::FromExternal(&buffer[1], packet_length - 1);
  if (packet_ind == kHciSco) {
    if (sco_connection_binding_.size() == 0) {
      FDF_LOG(DEBUG, "No SCO connection available for sending SCO packets up.");
      return;
    }

    sco_connection_binding_.ForEachBinding(
        [&](const fidl::ServerBinding<fhbt::ScoConnection>& binding) {
          fidl::OneWayStatus result = fidl::WireSendEvent(binding)->OnReceive(fidl_vec);

          if (!result.ok()) {
            FDF_LOG(ERROR, "Failed to send vendor features to bt-host: %s",
                    result.error().status_string());
          }
        });
  } else if (packet_ind == kHciAclData) {
    auto received_packet = fhbt::wire::ReceivedPacket::WithAcl(arena, fidl_vec);
    if (hci_transport_binding_.has_value()) {
      fidl::OneWayStatus result =
          fidl::WireSendEvent(hci_transport_binding_.value())->OnReceive(received_packet);
      if (!result.ok()) {
        FDF_LOG(ERROR, "Failed to send ACL packet to host: %s", result.error().status_string());
      }
    } else {
      // Note that this likely happens during system shutdown, when the other end of the channel
      // has been shutdown but this driver haven't gotten into the PrepareStop() step. If it
      // doesn't happen during shutdown, this might indicate a bug in either the driver or the
      // other end of this FIDL connection.
      FDF_LOG(INFO, "No HciTransport bindings available for sending up ACL packets");
    }
  } else if (packet_ind == kHciEvent) {
    auto received_packet = fhbt::wire::ReceivedPacket::WithEvent(arena, fidl_vec);
    if (hci_transport_binding_.has_value()) {
      fidl::OneWayStatus result =
          fidl::WireSendEvent(hci_transport_binding_.value())->OnReceive(received_packet);
      if (!result.ok()) {
        FDF_LOG(ERROR, "Failed to send event packet to host: %s", result.error().status_string());
      }
    } else {
      // Note that this likely happens during system shutdown, when the other end of the channel
      // has been shutdown but this driver haven't gotten into the PrepareStop() step. If it
      // doesn't happen during shutdown, this might indicate a bug in either the driver or the
      // other end of this FIDL connection.
      FDF_LOG(INFO, "No HciTransport bindings available for sending up event packets.");
    }
  } else {
    FDF_LOG(ERROR, "Unsupported packet type received");
  }

  unacked_receive_packet_number_++;

  fhbt::wire::SnoopPacket::Tag type = fhbt::wire::SnoopPacket::Tag::kIso;
  if (packet_ind == kHciAclData) {
    type = fhbt::wire::SnoopPacket::Tag::kAcl;
  } else if (packet_ind == kHciEvent) {
    type = fhbt::wire::SnoopPacket::Tag::kEvent;
  } else if (packet_ind == kHciSco) {
    type = fhbt::wire::SnoopPacket::Tag::kSco;
  } else {
    // TODO(b/350753924): Handle ISO packets in this driver.
    FDF_LOG(ERROR, "Unsupported packet type for snoop.");
  }

  SendSnoop(fidl_vec, type, fhbt::wire::PacketDirection::kControllerToHost);

  // reset buffer
  cur_uart_packet_type_ = kHciNone;
  *buffer_offset = 1;
}

void BtTransportUart::HciReadComplete(zx_status_t status, const uint8_t* buffer, size_t length) {
  FDF_LOG(TRACE, "Read complete with status: %s", zx_status_get_string(status));

  // If we are in the process of shutting down, we are done.
  if (atomic_load_explicit(&shutting_down_, std::memory_order_relaxed)) {
    return;
  }

  if (status == ZX_OK) {
    HciHandleUartReadEvents(buffer, length);
    if (unacked_receive_packet_number_ >= kUnackedReceivePacketLimit) {
      FDF_LOG(
          WARNING,
          "Too many unacked packets sent to the host, stop fetching data from the bus temporarily.");
      // Stop reading data from the uart buffer if there are too many unacked packets sent to the
      // host.
      read_stopped_ = true;
      return;
    }
    queue_read_task_.Post(dispatcher_);
  } else {
    // There is not much we can do in the event of a UART read error.  Do not
    // queue a read job and start the process of shutting down.
    FDF_LOG(ERROR, "Fatal UART read error (%s), shutting down", zx_status_get_string(status));
    HciBeginShutdown();
  }
}

void BtTransportUart::HciTransportWriteComplete(zx_status_t status) {
  FDF_LOG(TRACE, "Write complete with status: %s", zx_status_get_string(status));

  // If we are in the process of shutting down, we are done as soon as we
  // have freed our operation.
  if (atomic_load_explicit(&shutting_down_, std::memory_order_relaxed)) {
    return;
  }

  if (status != ZX_OK) {
    HciBeginShutdown();
    return;
  }

  ZX_DEBUG_ASSERT(!can_send_);
  can_send_ = true;

  // Resume processing the data in queue.
  send_queue_task_.Post(dispatcher_);
}

// fhbt::HciTransport protocol overrides.
void BtTransportUart::Send(fidl::Server<fhbt::HciTransport>::SendRequest& request,
                           fidl::Server<fhbt::HciTransport>::SendCompleter::Sync& completer) {
  if (available_buffers_.empty()) {
    FDF_LOG(ERROR, "Send queue is full, closing HciTransport connection");
    completer.Reply();
    hci_transport_binding_->Close(ZX_ERR_INTERNAL);
    hci_transport_binding_.reset();
    return;
  }

  auto& buffer = available_buffers_.front();
  const char* data_type_name = nullptr;
  // Figure out which data type we are dealing with and the constants which go
  // along with it.
  switch (request.Which()) {
    case fhbt::SentPacket::Tag::kIso:
      // TODO(b/350753924): Handle ISO packets in this driver.
      FDF_LOG(ERROR, "Unexpected ISO data packet.");
      return;
    case fhbt::SentPacket::Tag::kAcl:
      buffer.push_back(kHciAclData);
      buffer.insert(buffer.end(), request.acl().value().begin(), request.acl().value().end());
      data_type_name = "ACL";
      break;
    case fhbt::SentPacket::Tag::kCommand:
      buffer.push_back(kHciCommand);
      buffer.insert(buffer.end(), request.command().value().begin(),
                    request.command().value().end());
      data_type_name = "command";
      break;
    default:
      FDF_LOG(ERROR, "Unknown packet type: %zu", request.Which());
  }

  FDF_LOG(TRACE, "received data type: %s", data_type_name);

  send_queue_.emplace(std::move(buffer),
                      [completer = completer.ToAsync()]() mutable { completer.Reply(); });
  available_buffers_.pop();
  send_queue_task_.Post(dispatcher_);
}

void BtTransportUart::AckReceive(
    fidl::Server<fhbt::HciTransport>::AckReceiveCompleter::Sync& completer) {
  OnAckReceive();
}

void BtTransportUart::ConfigureSco(
    fidl::Server<fhbt::HciTransport>::ConfigureScoRequest& request,
    fidl::Server<fhbt::HciTransport>::ConfigureScoCompleter::Sync& completer) {
  if (!request.connection().has_value()) {
    FDF_LOG(ERROR, "No ScoConnection server end received from the host.");
    return;
  }
  if (sco_connection_binding_.size() != 0) {
    FDF_LOG(ERROR, "ScoConnection connection exists.");
    return;
  }

  sco_connection_binding_.AddBinding(dispatcher_, std::move(request.connection().value()),
                                     &sco_connection_server_, fidl::kIgnoreBindingClosure);
  // Only useful in tests, it doesn't hurt to signal it when no one is waiting for it.
  sco_connection_setup_.Signal();
}

fit::function<void(void)> BtTransportUart::WaitForSnoopCallback() {
  return [this]() { snoop_setup_.Wait(); };
}

fit::function<void(void)> BtTransportUart::WaitForScoConnectionCallback() {
  return [this]() { sco_connection_setup_.Wait(); };
}

uint64_t BtTransportUart::GetAckedSnoopSeq() { return acked_snoop_seq_; }

void BtTransportUart::handle_unknown_method(
    ::fidl::UnknownMethodMetadata<fhbt::HciTransport> metadata,
    ::fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in HciTransport protocol, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void BtTransportUart::GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) {
  fuchsia_hardware_serial::wire::SerialPortInfo info{
      .serial_class = fuchsia_hardware_serial::wire::Class::kBluetoothHci,
      .serial_pid = serial_pid_,
  };
  completer.buffer(arena).ReplySuccess(info);
}
void BtTransportUart::Config(ConfigRequestView request, fdf::Arena& arena,
                             ConfigCompleter::Sync& completer) {
  auto result = serial_client_.sync().buffer(arena)->Config(request->baud_rate, request->flags);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Config request failed with FIDL error %s", result.status_string());
    completer.buffer(arena).ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Config request failed with error %s",
            zx_status_get_string(result->error_value()));
    completer.buffer(arena).ReplyError(result->error_value());
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void BtTransportUart::Enable(EnableRequestView request, fdf::Arena& arena,
                             EnableCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void BtTransportUart::Read(fdf::Arena& arena, ReadCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void BtTransportUart::Write(WriteRequestView request, fdf::Arena& arena,
                            WriteCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void BtTransportUart::CancelAll(fdf::Arena& arena, CancelAllCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void BtTransportUart::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_serialimpl::Device> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(
      ERROR,
      "Unknown method in fuchsia_hardware_serialimpl::Device protocol, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void BtTransportUart::AcknowledgePackets(AcknowledgePacketsRequest& request,
                                         AcknowledgePacketsCompleter::Sync& completer) {
  acked_snoop_seq_ = std::max(request.sequence(), acked_snoop_seq_);
}

void BtTransportUart::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Snoop> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in fidl::Server<fuchsia_hardware_bluetooth::Snoop>");
}

void BtTransportUart::QueueUartRead() {
  fdf::Arena arena('READ');
  serial_client_.buffer(arena)->Read().ThenExactlyOnce([this](fdf::WireUnownedResult<
                                                              fuchsia_hardware_serialimpl::Device::
                                                                  Read>& result) {
    if (!result.ok()) {
      FDF_LOG(ERROR, "Read request failed with FIDL error %s", result.status_string());
      HciReadComplete(result.status(), nullptr, 0);
      return;
    }

    if (result->is_error()) {
      if (result->error_value() == ZX_ERR_CANCELED) {
        FDF_LOG(
            WARNING,
            "Read request is cancel by the bus driver, it's likely the serial driver is de-initialized.");
      } else {
        FDF_LOG(ERROR, "Read request failed with error %s",
                zx_status_get_string(result->error_value()));
      }
      HciReadComplete(result->error_value(), nullptr, 0);
      return;
    }
    HciReadComplete(ZX_OK, result->value()->data.data(), result->value()->data.count());
  });
}

zx_status_t BtTransportUart::ServeProtocols() {
  // Add HCI services to the outgoing directory.
  auto hci_transport_protocol = [this](fidl::ServerEnd<fhbt::HciTransport> server_end) mutable {
    if (hci_transport_binding_) {
      FDF_LOG(WARNING, "HciTransport binding exists, replacing it.");
    }
    hci_transport_binding_.emplace(dispatcher_, std::move(server_end), this,
                                   [this](fidl::UnbindInfo) {
                                     hci_transport_binding_.reset();
                                     FDF_LOG(INFO, "HciTransport server binding unbound.");
                                   });
    FDF_LOG(INFO, "HciTransport server binding emplaced.");
    queue_read_task_.Post(dispatcher_);
  };
  auto snoop_protocol = [this](fidl::ServerEnd<fhbt::Snoop> server_end) mutable {
    if (snoop_server_.has_value()) {
      FDF_LOG(ERROR, "Snoop protocol connect with Snoop already active");
      return;
    }
    snoop_server_.emplace(dispatcher_, std::move(server_end), this,
                          [this](fidl::UnbindInfo) { snoop_server_.reset(); });
    // Only useful in tests, it doesn't hurt to signal it when no one is waiting for it.
    snoop_setup_.Signal();
  };

  fhbt::HciService::InstanceHandler hci_handler(
      {.hci_transport = std::move(hci_transport_protocol), .snoop = std::move(snoop_protocol)});

  auto status = outgoing()->AddService<fhbt::HciService>(std::move(hci_handler));
  if (status.is_error()) {
    FDF_LOG(ERROR, "Failed to add HCI service to outgoing directory: %s\n", status.status_string());
    return status.error_value();
  }

  // Add Serial service to the outgoing directory.
  auto serial_protocol =
      [this](fdf::ServerEnd<fuchsia_hardware_serialimpl::Device> server_end) mutable {
        fdf::BindServer(driver_dispatcher()->get(), std::move(server_end), this);
      };
  fuchsia_hardware_serialimpl::Service::InstanceHandler serial_handler(
      {.device = std::move(serial_protocol)});
  status = outgoing()->AddService<fuchsia_hardware_serialimpl::Service>(std::move(serial_handler));
  if (status.is_error()) {
    FDF_LOG(ERROR, "Failed to add Serial service to outgoing directory: %s\n",
            status.status_string());
    return status.error_value();
  }

  return ZX_OK;
}

}  // namespace bt_transport_uart

FUCHSIA_DRIVER_EXPORT(bt_transport_uart::BtTransportUart);
