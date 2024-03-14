// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bt_transport_uart.h"

#include <assert.h>
#include <lib/async/default.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/status.h>

namespace bt_transport_uart {

BtTransportUart::BtTransportUart(fuchsia_driver_framework::DriverStartArgs start_args,
                                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("bt-transport-uart", std::move(start_args), std::move(driver_dispatcher)),
      dispatcher_(dispatcher()),
      node_(fidl::WireClient(std::move(node()), dispatcher())) {}

zx::result<> BtTransportUart::Start() {
  FDF_LOG(DEBUG, "Start");

  zx::result<fdf::ClientEnd<fuchsia_hardware_serialimpl::Device>> client_end =
      incoming()->Connect<fuchsia_hardware_serialimpl::Service::Device>();
  if (client_end.is_error()) {
    FDF_LOG(ERROR, "Connect to fuchsia_hardware_serialimpl::Device protocol failed: %s",
            client_end.status_string());
    return zx::error(client_end.status_value());
  }

  {
    std::lock_guard guard(mutex_);

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
  }

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

  queue_read_task_.Post(dispatcher_);

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
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_bluetooth::HciService>(args_arena));
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

  {
    std::lock_guard guard(mutex_);

    // Close the transport channels so that the host stack is notified of device
    // removal and tasks aren't posted to work thread.
    ChannelCleanupLocked(&cmd_channel_);
    ChannelCleanupLocked(&acl_channel_);
    ChannelCleanupLocked(&sco_channel_);
    ChannelCleanupLocked(&snoop_channel_);
  }

  // Finish by making sure that all in flight transactions transactions have
  // been canceled.
  fdf::Arena arena('CANC');
  auto result = serial_client_.sync().buffer(arena)->CancelAll();
  if (!result.ok()) {
    FDF_LOG(ERROR, "hci_bind: CancelAll failed with FIDL error %s", result.status_string());
    completer(zx::error(result.status()));
  }

  FDF_LOG(TRACE, "PrepareStop complete");

  completer(zx::ok());
}

// NOLINTNEXTLINE(bugprone-easily-swappable-parameters)
BtTransportUart::Wait::Wait(BtTransportUart* uart, zx::channel* channel) {
  this->state = ASYNC_STATE_INIT;
  this->handler = Handler;
  this->object = ZX_HANDLE_INVALID;
  this->trigger = ZX_SIGNAL_NONE;
  this->options = 0;
  this->uart = uart;
  this->channel = channel;
}

void BtTransportUart::Wait::Handler(async_dispatcher_t* dispatcher, async_wait_t* async_wait,
                                    zx_status_t status, const zx_packet_signal_t* signal) {
  auto wait = static_cast<Wait*>(async_wait);
  wait->uart->OnChannelSignal(wait, status, signal);
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

void BtTransportUart::ChannelCleanupLocked(zx::channel* channel) {
  if (!channel->is_valid()) {
    return;
  }

  if (channel == &cmd_channel_ && cmd_channel_wait_.pending) {
    async_cancel_wait(dispatcher_, &cmd_channel_wait_);
    cmd_channel_wait_.pending = false;
  } else if (channel == &acl_channel_ && acl_channel_wait_.pending) {
    async_cancel_wait(dispatcher_, &acl_channel_wait_);
    acl_channel_wait_.pending = false;
  } else if (channel == &sco_channel_ && sco_channel_wait_.pending) {
    async_cancel_wait(dispatcher_, &sco_channel_wait_);
    sco_channel_wait_.pending = false;
  }
  channel->reset();
}

void BtTransportUart::SnoopChannelWriteLocked(uint8_t flags, uint8_t* bytes, size_t length) {
  if (!snoop_channel_.is_valid()) {
    return;
  }

  // We tack on a flags byte to the beginning of the payload.
  // Use an iovec to avoid a large allocation + copy.
  zx_channel_iovec_t iovs[2];
  iovs[0] = {.buffer = &flags, .capacity = sizeof(flags), .reserved = 0};
  iovs[1] = {.buffer = bytes, .capacity = static_cast<uint32_t>(length), .reserved = 0};

  zx_status_t status =
      snoop_channel_.write(/*flags=*/ZX_CHANNEL_WRITE_USE_IOVEC, /*bytes=*/iovs,
                           /*num_bytes=*/std::size(iovs), /*handles=*/nullptr, /*num_handles=*/0);

  if (status != ZX_OK) {
    if (status != ZX_ERR_PEER_CLOSED) {
      FDF_LOG(ERROR, "bt-transport-uart: failed to write to snoop channel: %s",
              zx_status_get_string(status));
    }

    // It should be safe to clean up the channel right here as the work thread
    // never waits on this channel from outside of the lock.
    ChannelCleanupLocked(&snoop_channel_);
  }
}

void BtTransportUart::HciBeginShutdown() {
  bool was_shutting_down = shutting_down_.exchange(true, std::memory_order_relaxed);
  if (!was_shutting_down) {
    auto result = node_.UnbindMaybeGetEndpoint();
  }
}

void BtTransportUart::SerialWrite(uint8_t* buffer, size_t length) {
  {
    std::lock_guard guard(mutex_);
    ZX_DEBUG_ASSERT(can_write_);
    // Clear the can_write flag.  The UART can currently only handle one in flight
    // transaction at a time.
    can_write_ = false;
  }

  fdf::Arena arena('WRIT');
  auto data = fidl::VectorView<uint8_t>::FromExternal(buffer, length);
  serial_client_.buffer(arena)->Write(data).ThenExactlyOnce(
      [this](fdf::WireUnownedResult<fuchsia_hardware_serialimpl::Device::Write>& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "hci_bind: Write failed with FIDL error %s", result.status_string());
          HciWriteComplete(result.status());
          return;
        }

        if (result->is_error()) {
          FDF_LOG(ERROR, "hci_bind: Write failed with error %s",
                  zx_status_get_string(result->error_value()));
          HciWriteComplete(result->error_value());
          return;
        }
        HciWriteComplete(ZX_OK);
      });
}

// Returns false if there's an error while sending the packet to the hardware or
// if the channel peer closed its endpoint.
void BtTransportUart::HciHandleClientChannel(zx::channel* chan, zx_signals_t pending) {
  // Channel may have been closed since signal was received.
  if (!chan->is_valid()) {
    return;
  }

  // Figure out which channel we are dealing with and the constants which go
  // along with it.
  uint32_t max_buf_size;
  BtHciPacketIndicator packet_type;
  bt_hci_snoop_type_t snoop_type;
  const char* chan_name = nullptr;

  if (chan == &cmd_channel_) {
    max_buf_size = kCmdBufSize;
    packet_type = kHciCommand;
    snoop_type = BT_HCI_SNOOP_TYPE_CMD;
    chan_name = "command";
  } else if (chan == &acl_channel_) {
    max_buf_size = kAclMaxFrameSize;
    packet_type = kHciAclData;
    snoop_type = BT_HCI_SNOOP_TYPE_ACL;
    chan_name = "ACL";
  } else if (chan == &sco_channel_) {
    max_buf_size = kScoMaxFrameSize;
    packet_type = kHciSco;
    snoop_type = BT_HCI_SNOOP_TYPE_SCO;
    chan_name = "SCO";
  } else {
    // This should never happen, we only know about three packet types currently.
    ZX_ASSERT(false);
    return;
  }

  // Handle the read signal first.  If we are also peer closed, we want to make
  // sure that we have processed all of the pending messages before cleaning up.
  if (pending & ZX_CHANNEL_READABLE) {
    FDF_LOG(TRACE, "received readable signal for %s channel", chan_name);
    uint32_t length = max_buf_size - 1;
    {
      std::lock_guard guard(mutex_);

      // Do not proceed if we are not allowed to write.  Let the work thread call
      // us back again when it is safe to write.
      if (!can_write_) {
        return;
      }

      zx_status_t status;

      status =
          zx_channel_read(chan->get(), 0, write_buffer_ + 1, nullptr, length, 0, &length, nullptr);
      if (status == ZX_ERR_SHOULD_WAIT) {
        FDF_LOG(WARNING, "ignoring ZX_ERR_SHOULD_WAIT when reading %s channel", chan_name);
        return;
      }
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "hci_read_thread: failed to read from %s channel %s", chan_name,
                zx_status_get_string(status));
        ChannelCleanupLocked(chan);
        return;
      }

      write_buffer_[0] = packet_type;
      length++;

      SnoopChannelWriteLocked(bt_hci_snoop_flags(snoop_type, false), write_buffer_ + 1, length - 1);
    }

    SerialWrite(write_buffer_, length);
  }

  if (pending & ZX_CHANNEL_PEER_CLOSED) {
    FDF_LOG(DEBUG, "received closed signal for %s channel", chan_name);
    std::lock_guard guard(mutex_);
    ChannelCleanupLocked(chan);
  }
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
        ProcessNextUartPacketFromReadBuffer(
            event_buffer_, sizeof(event_buffer_), &event_buffer_offset_, &buf, end,
            &BtTransportUart::EventPacketLength, &cmd_channel_, BT_HCI_SNOOP_TYPE_EVT);
        break;
      case kHciAclData:
        ProcessNextUartPacketFromReadBuffer(acl_buffer_, sizeof(acl_buffer_), &acl_buffer_offset_,
                                            &buf, end, &BtTransportUart::AclPacketLength,
                                            &acl_channel_, BT_HCI_SNOOP_TYPE_ACL);
        break;
      case kHciSco:
        ProcessNextUartPacketFromReadBuffer(sco_buffer_, sizeof(sco_buffer_), &sco_buffer_offset_,
                                            &buf, end, &BtTransportUart::ScoPacketLength,
                                            &sco_channel_, BT_HCI_SNOOP_TYPE_SCO);
        break;
      default:
        FDF_LOG(ERROR, "unsupported HCI packet type %u received. We may be out of sync",
                cur_uart_packet_type_);
        cur_uart_packet_type_ = kHciNone;
        return;
    }
  }
}

void BtTransportUart::ProcessNextUartPacketFromReadBuffer(
    uint8_t* buffer, size_t buffer_size, size_t* buffer_offset, const uint8_t** uart_src,
    const uint8_t* uart_end, PacketLengthFunction get_packet_length, zx::channel* channel,
    bt_hci_snoop_type_t snoop_type) {
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

  std::lock_guard guard(mutex_);

  // Attempt to send this packet to the channel. We are working on the callback thread from the
  // UART, so we need to do this inside of the lock to make sure that nothing closes the channel
  // out from under us while we try to write. If something goes wrong here, close the channel.
  if (channel->is_valid()) {
    zx_status_t status = channel->write(/*flags=*/0, &buffer[1], packet_length - 1, nullptr, 0);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "failed to write packet: %s", zx_status_get_string(status));
      ChannelCleanupLocked(&acl_channel_);
    }
  }

  // If the snoop channel is open then try to write the packet even if |channel| was closed.
  SnoopChannelWriteLocked(bt_hci_snoop_flags(snoop_type, true), &buffer[1], packet_length - 1);

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
    queue_read_task_.Post(dispatcher_);
  } else {
    // There is not much we can do in the event of a UART read error.  Do not
    // queue a read job and start the process of shutting down.
    FDF_LOG(ERROR, "Fatal UART read error (%s), shutting down", zx_status_get_string(status));
    HciBeginShutdown();
  }
}

void BtTransportUart::HciWriteComplete(zx_status_t status) {
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

  // We can write now.
  {
    std::lock_guard guard(mutex_);
    can_write_ = true;

    // Resume waiting for channel signals. If a packet was queued while the write was processing,
    // it should be immediately signaled.
    if (cmd_channel_wait_.channel->is_valid() && !cmd_channel_wait_.pending) {
      ZX_ASSERT(async_begin_wait(dispatcher_, &cmd_channel_wait_) == ZX_OK);
      cmd_channel_wait_.pending = true;
    }
    if (acl_channel_wait_.channel->is_valid() && !acl_channel_wait_.pending) {
      ZX_ASSERT(async_begin_wait(dispatcher_, &acl_channel_wait_) == ZX_OK);
      acl_channel_wait_.pending = true;
    }
    if (sco_channel_wait_.channel->is_valid() && !sco_channel_wait_.pending) {
      ZX_ASSERT(async_begin_wait(dispatcher_, &sco_channel_wait_) == ZX_OK);
      sco_channel_wait_.pending = true;
    }
  }
}

void BtTransportUart::OnChannelSignal(Wait* wait, zx_status_t status,
                                      const zx_packet_signal_t* signal) {
  {
    std::lock_guard guard(mutex_);
    wait->pending = false;
  }

  HciHandleClientChannel(wait->channel, signal->observed);

  // The readable signal wait will be re-enabled in the write completion callback.
}

zx_status_t BtTransportUart::HciOpenChannel(zx::channel* in_channel, zx_handle_t in) {
  std::lock_guard guard(mutex_);
  zx_status_t result = ZX_OK;

  if (in_channel->is_valid()) {
    FDF_LOG(ERROR, "bt-transport-uart: already bound, failing");
    result = ZX_ERR_ALREADY_BOUND;
    return result;
  }

  in_channel->reset(in);

  Wait* wait = nullptr;
  if (in_channel == &cmd_channel_) {
    FDF_LOG(DEBUG, "opening command channel");
    wait = &cmd_channel_wait_;
  } else if (in_channel == &acl_channel_) {
    FDF_LOG(DEBUG, "opening ACL channel");
    wait = &acl_channel_wait_;
  } else if (in_channel == &sco_channel_) {
    FDF_LOG(DEBUG, "opening SCO channel");
    wait = &sco_channel_wait_;
  } else if (in_channel == &snoop_channel_) {
    FDF_LOG(DEBUG, "opening snoop channel");
    // TODO(https://fxbug.dev/42172901): Handle snoop channel closed signal.
    return ZX_OK;
  }
  ZX_ASSERT(wait);
  wait->object = in_channel->get();
  wait->trigger = ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED;
  ZX_ASSERT(async_begin_wait(dispatcher_, wait) == ZX_OK);
  wait->pending = true;
  return result;
}

void BtTransportUart::OpenCommandChannel(OpenCommandChannelRequestView request,
                                         OpenCommandChannelCompleter::Sync& completer) {
  if (zx_status_t status = HciOpenChannel(&cmd_channel_, std::move(request->channel.release()));
      status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to open command channel: %s", zx_status_get_string(status));
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}
void BtTransportUart::OpenAclDataChannel(OpenAclDataChannelRequestView request,
                                         OpenAclDataChannelCompleter::Sync& completer) {
  if (zx_status_t status = HciOpenChannel(&acl_channel_, std::move(request->channel.release()));
      status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to open acl channel: %s", zx_status_get_string(status));
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}
void BtTransportUart::OpenSnoopChannel(OpenSnoopChannelRequestView request,
                                       OpenSnoopChannelCompleter::Sync& completer) {
  if (zx_status_t status = HciOpenChannel(&snoop_channel_, std::move(request->channel.release()));
      status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to open snoop channel: %s", zx_status_get_string(status));
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}
void BtTransportUart::OpenScoDataChannel(OpenScoDataChannelRequestView request,
                                         OpenScoDataChannelCompleter::Sync& completer) {
  if (zx_status_t status = HciOpenChannel(&sco_channel_, std::move(request->channel.release()));
      status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to open sco channel: %s", zx_status_get_string(status));
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}
void BtTransportUart::OpenIsoDataChannel(OpenIsoDataChannelRequestView request,
                                         OpenIsoDataChannelCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void BtTransportUart::ConfigureSco(ConfigureScoRequestView request,
                                   ConfigureScoCompleter::Sync& completer) {
  // UART doesn't require any SCO configuration.
  completer.ReplySuccess();
}
void BtTransportUart::ResetSco(ResetScoCompleter::Sync& completer) {
  // UART doesn't require any SCO configuration, so there's nothing to do.
  completer.ReplySuccess();
}
void BtTransportUart::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Hci> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in Hci protocol, closing with ZX_ERR_NOT_SUPPORTED");
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
  auto result = serial_client_.sync().buffer(arena)->Config(
      request->baud_rate, fuchsia_hardware_serialimpl::wire::kSerialSetBaudRateOnly);
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
  // Add HCI service to the outgoing directory.
  auto hci_protocol = [this](fidl::ServerEnd<fuchsia_hardware_bluetooth::Hci> server_end) mutable {
    fidl::BindServer(dispatcher_, std::move(server_end), this);
  };
  fuchsia_hardware_bluetooth::HciService::InstanceHandler hci_handler(
      {.hci = std::move(hci_protocol)});
  auto status =
      outgoing()->AddService<fuchsia_hardware_bluetooth::HciService>(std::move(hci_handler));
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
