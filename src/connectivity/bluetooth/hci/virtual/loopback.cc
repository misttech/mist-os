// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "loopback.h"

#include <lib/driver/logging/cpp/logger.h>

namespace bt_hci_virtual {

LoopbackDevice::LoopbackDevice()
    : devfs_connector_(fit::bind_member<&LoopbackDevice::Connect>(this)) {}

zx_status_t LoopbackDevice::Initialize(zx_handle_t channel, std::string_view name,
                                       AddChildCallback callback) {
  // Pre-populate event packet indicators
  event_buffer_[0] = kHciEvent;
  event_buffer_offset_ = 1;
  acl_buffer_[0] = kHciAclData;
  acl_buffer_offset_ = 1;
  sco_buffer_[0] = kHciSco;
  sco_buffer_offset_ = 1;

  // Setup up incoming channel waiter
  in_channel_.reset(channel);
  in_channel_wait_.object = in_channel_.get();
  in_channel_wait_.trigger = ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED;
  ZX_ASSERT(async_begin_wait(dispatcher_, &in_channel_wait_) == ZX_OK);
  in_channel_wait_.pending = true;

  // Create args to add loopback as a child node on behalf of VirtualController
  zx::result connector = devfs_connector_.Bind(dispatcher_);
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Failed to bind devfs connecter to dispatcher: %u", connector.status_value());
    return connector.error_value();
  }

  fidl::Arena args_arena;
  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(args_arena)
                   .connector(std::move(connector.value()))
                   .class_name("bt-hci")
                   .Build();
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(args_arena)
                  .name(name.data())
                  .devfs_args(devfs)
                  .Build();

  callback(args);

  return ZX_OK;
}

void LoopbackDevice::Shutdown() {
  // We are now shutting down. Make sure that any pending callbacks in
  // flight from the serial_impl are nerfed and that our thread is shut down.
  std::atomic_store_explicit(&shutting_down_, true, std::memory_order_relaxed);

  // Close the transport channels so that the host stack is notified of device
  // removal and tasks aren't posted to work thread.
  ChannelCleanup(&cmd_channel_);
  ChannelCleanup(&acl_channel_);
  ChannelCleanup(&sco_channel_);
  ChannelCleanup(&snoop_channel_);
  ChannelCleanup(&in_channel_);
}

void LoopbackDevice::Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request) {
  vendor_binding_group_.AddBinding(dispatcher_, std::move(request), this,
                                   fidl::kIgnoreBindingClosure);

  vendor_binding_group_.ForEachBinding(
      [](const fidl::ServerBinding<fuchsia_hardware_bluetooth::Vendor>& binding) {
        fidl::Arena arena;
        auto builder = fuchsia_hardware_bluetooth::wire::VendorFeatures::Builder(arena);
        fidl::Status status = fidl::WireSendEvent(binding)->OnFeatures(builder.Build());

        if (status.status() != ZX_OK) {
          FDF_LOG(ERROR, "Failed to send vendor features to bt-host: %s", status.status_string());
        }
      });
}

// NOLINTNEXTLINE(bugprone-easily-swappable-parameters)
LoopbackDevice::Wait::Wait(LoopbackDevice* uart, zx::channel* channel) {
  this->state = ASYNC_STATE_INIT;
  this->handler = Handler;
  this->object = ZX_HANDLE_INVALID;
  this->trigger = ZX_SIGNAL_NONE;
  this->options = 0;
  this->uart = uart;
  this->channel = channel;
}

void LoopbackDevice::Wait::Handler(async_dispatcher_t* dispatcher, async_wait_t* async_wait,
                                   zx_status_t status, const zx_packet_signal_t* signal) {
  auto wait = static_cast<Wait*>(async_wait);
  wait->uart->OnChannelSignal(wait, status, signal);
}

size_t LoopbackDevice::EventPacketLength() {
  // payload length is in byte 2 of the packet
  // add 3 bytes for packet indicator, event code and length byte
  return event_buffer_offset_ > 2 ? event_buffer_[2] + 3 : 0;
}

size_t LoopbackDevice::AclPacketLength() {
  // length is in bytes 3 and 4 of the packet
  // add 5 bytes for packet indicator, control info and length fields
  return acl_buffer_offset_ > 4 ? (acl_buffer_[3] | (acl_buffer_[4] << 8)) + 5 : 0;
}

size_t LoopbackDevice::ScoPacketLength() {
  // payload length is byte 3 of the packet
  // add 4 bytes for packet indicator, handle, and length byte
  return sco_buffer_offset_ > 3 ? (sco_buffer_[3] + 4) : 0;
}

void LoopbackDevice::ChannelCleanup(zx::channel* channel) {
  FDF_LOG(TRACE, "LoopbackDevice::ChannelCleanup");
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
  } else if (channel == &in_channel_ && in_channel_wait_.pending) {
    async_cancel_wait(dispatcher_, &in_channel_wait_);
    in_channel_wait_.pending = false;
  }
  channel->reset();
}

void LoopbackDevice::SnoopChannelWrite(uint8_t flags, uint8_t* bytes, size_t length) {
  FDF_LOG(TRACE, "LoopbackDevice::SnoopChannelWrite");
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
                           /*num_bytes=*/2, /*handles=*/nullptr, /*num_handles=*/0);

  if (status != ZX_OK) {
    if (status != ZX_ERR_PEER_CLOSED) {
      FDF_LOG(ERROR, "LoopbackDevice: failed to write to snoop channel %s",
              zx_status_get_string(status));
    }

    // It should be safe to clean up the channel right here as the work thread
    // never waits on this channel from outside of the lock.
    ChannelCleanup(&snoop_channel_);
  }
}

void LoopbackDevice::HciBeginShutdown() {
  FDF_LOG(TRACE, "LoopbackDevice::HciBeginShutdown");
  bool was_shutting_down = shutting_down_.exchange(true, std::memory_order_relaxed);
  if (!was_shutting_down) {
    FDF_LOG(TRACE, "LoopbackDevice::HciBeginShutdown !was_shutting_down");
    // DdkAsyncRemove(); // TODO(luluwang): Replace this
  }
}

void LoopbackDevice::OnChannelSignal(Wait* wait, zx_status_t status,
                                     const zx_packet_signal_t* signal) {
  FDF_LOG(TRACE, "OnChannelSignal");
  wait->pending = false;

  if (wait->channel == &in_channel_) {
    HciHandleIncomingChannel(wait->channel, signal->observed);
  } else {
    HciHandleClientChannel(wait->channel, signal->observed);
  }

  // Reset waiters and resume waiting for channel signals. If a packet was queued while the write
  // was processing, it should be immediately signaled.
  if (wait->channel->is_valid() && !wait->pending) {
    ZX_ASSERT(async_begin_wait(dispatcher_, wait) == ZX_OK);
    wait->pending = true;
  }
}

zx_status_t LoopbackDevice::HciOpenChannel(zx::channel* in_channel, zx_handle_t in) {
  FDF_LOG(TRACE, "LoopbackDevice::HciOpenChannel");
  zx_status_t result = ZX_OK;

  if (in_channel->is_valid()) {
    FDF_LOG(ERROR, "LoopbackDevice: already bound, failing");
    result = ZX_ERR_ALREADY_BOUND;
    return result;
  }

  in_channel->reset(in);

  Wait* wait = nullptr;
  if (in_channel == &cmd_channel_) {
    wait = &cmd_channel_wait_;
  } else if (in_channel == &acl_channel_) {
    wait = &acl_channel_wait_;
  } else if (in_channel == &sco_channel_) {
    wait = &sco_channel_wait_;
  } else if (in_channel == &snoop_channel_) {
    return ZX_OK;
  }
  ZX_ASSERT(wait);
  wait->object = in_channel->get();
  wait->trigger = ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED;
  ZX_ASSERT(async_begin_wait(dispatcher_, wait) == ZX_OK);
  wait->pending = true;
  return result;
}

void LoopbackDevice::HciHandleIncomingChannel(zx::channel* chan, zx_signals_t pending) {
  FDF_LOG(TRACE, "HciHandleIncomingChannel readable:%d, closed: %d",
          int(pending & ZX_CHANNEL_READABLE), int(pending & ZX_CHANNEL_PEER_CLOSED));
  // If we are in the process of shutting down, we are done.
  if (atomic_load_explicit(&shutting_down_, std::memory_order_relaxed)) {
    return;
  }

  // Channel may have been closed since signal was received.
  if (!chan->is_valid()) {
    FDF_LOG(ERROR, "channel is invalid");
    return;
  }

  // Handle the read signal first.  If we are also peer closed, we want to make
  // sure that we have processed all of the pending messages before cleaning up.
  if (pending & ZX_CHANNEL_READABLE) {
    uint32_t length;
    uint8_t read_buffer[kAclMaxFrameSize];
    zx_status_t status;

    status = zx_channel_read(chan->get(), 0, read_buffer, nullptr, kAclMaxFrameSize, 0, &length,
                             nullptr);
    if (status == ZX_ERR_SHOULD_WAIT) {
      FDF_LOG(WARNING, "ignoring ZX_ERR_SHOULD_WAIT when reading incoming channel");
      return;
    }
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "failed to read from incoming channel %s", zx_status_get_string(status));
      ChannelCleanup(chan);
      return;
    }

    const uint8_t* buf = read_buffer;
    const uint8_t* const end = read_buffer + length;
    while (buf < end) {
      if (cur_uart_packet_type_ == kHciNone) {
        // start of new packet. read packet type
        cur_uart_packet_type_ = static_cast<BtHciPacketIndicator>(*buf++);
      }

      switch (cur_uart_packet_type_) {
        case kHciEvent:
          ProcessNextUartPacketFromReadBuffer(
              event_buffer_, sizeof(event_buffer_), &event_buffer_offset_, &buf, end,
              &LoopbackDevice::EventPacketLength, &cmd_channel_, BT_HCI_SNOOP_TYPE_EVT);
          break;
        case kHciAclData:
          ProcessNextUartPacketFromReadBuffer(acl_buffer_, sizeof(acl_buffer_), &acl_buffer_offset_,
                                              &buf, end, &LoopbackDevice::AclPacketLength,
                                              &acl_channel_, BT_HCI_SNOOP_TYPE_ACL);
          break;
        case kHciSco:
          ProcessNextUartPacketFromReadBuffer(sco_buffer_, sizeof(sco_buffer_), &sco_buffer_offset_,
                                              &buf, end, &LoopbackDevice::ScoPacketLength,
                                              &sco_channel_, BT_HCI_SNOOP_TYPE_SCO);
          break;
        default:
          FDF_LOG(ERROR, "unsupported HCI packet type %i received. We may be out of sync",
                  int(cur_uart_packet_type_));
          cur_uart_packet_type_ = kHciNone;
          return;
      }
    }
  }

  if (pending & ZX_CHANNEL_PEER_CLOSED) {
    ChannelCleanup(chan);
    HciBeginShutdown();
  }
}

void LoopbackDevice::ProcessNextUartPacketFromReadBuffer(
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
  copy_size = std::min(copy_size, remaining);

  ZX_ASSERT(*buffer_offset + copy_size <= buffer_size);
  memcpy(buffer + *buffer_offset, *uart_src, copy_size);
  *uart_src += copy_size;
  *buffer_offset += copy_size;

  if (*buffer_offset != packet_length) {
    // The packet is incomplete, the next chunk should continue the same packet.
    return;
  }

  // Attempt to send this packet to the channel. Do so in the lock so we don't shut down while
  // writing.
  if (channel->is_valid()) {
    zx_status_t status = channel->write(/*flags=*/0, &buffer[1], packet_length - 1, nullptr, 0);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "failed to write packet: %s", zx_status_get_string(status));
      ChannelCleanup(&acl_channel_);
    }
  }

  // If the snoop channel is open then try to write the packet even if |channel| was closed.
  SnoopChannelWrite(bt_hci_snoop_flags(snoop_type, true), &buffer[1], packet_length - 1);

  // reset buffer
  cur_uart_packet_type_ = kHciNone;
  *buffer_offset = 1;
}

void LoopbackDevice::HciHandleClientChannel(zx::channel* chan, zx_signals_t pending) {
  FDF_LOG(TRACE, "LoopbackDevice::HciHandleClientChannel");
  // Channel may have been closed since signal was received.
  if (!chan->is_valid()) {
    FDF_LOG(ERROR, "chan invalid");
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

  FDF_LOG(TRACE, "LoopbackDevice::HciHandleClientChannel handling %s", chan_name);

  // Handle the read signal first.  If we are also peer closed, we want to make
  // sure that we have processed all of the pending messages before cleaning up.
  if (pending & ZX_CHANNEL_READABLE) {
    zx_status_t status = ZX_OK;
    uint32_t length = max_buf_size - 1;

    status =
        zx_channel_read(chan->get(), 0, write_buffer_ + 1, nullptr, length, 0, &length, nullptr);
    if (status == ZX_ERR_SHOULD_WAIT) {
      FDF_LOG(WARNING, "ignoring ZX_ERR_SHOULD_WAIT when reading %s channel", chan_name);
      return;
    }
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "hci_read_thread: failed to read from %s channel %s", chan_name,
              zx_status_get_string(status));
      ChannelCleanup(chan);
      return;
    }

    write_buffer_[0] = packet_type;
    length++;

    SnoopChannelWrite(bt_hci_snoop_flags(snoop_type, false), write_buffer_ + 1, length - 1);

    if (in_channel_.is_valid()) {
      status = in_channel_.write(/*flags=*/0, write_buffer_, length, nullptr, 0);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "failed to write packet: %s", zx_status_get_string(status));
      }
    }

    if (status != ZX_OK) {
      HciBeginShutdown();
    }
  }

  if (pending & ZX_CHANNEL_PEER_CLOSED) {
    FDF_LOG(DEBUG, "received closed signal for %s channel", chan_name);
    ChannelCleanup(chan);
  }
}

void LoopbackDevice::EncodeCommand(EncodeCommandRequestView request,
                                   EncodeCommandCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void LoopbackDevice::OpenHci(OpenHciCompleter::Sync& completer) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_bluetooth::Hci>();
  if (endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create endpoints: %s", zx_status_get_string(endpoints.error_value()));
    completer.ReplyError(endpoints.error_value());
    return;
  }
  fidl::BindServer(dispatcher_, std::move(endpoints->server), this);
  completer.ReplySuccess(std::move(endpoints->client));
}

void LoopbackDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in Vendor request, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void LoopbackDevice::OpenCommandChannel(OpenCommandChannelRequestView request,
                                        OpenCommandChannelCompleter::Sync& completer) {
  if (zx_status_t status = HciOpenChannel(&cmd_channel_, request->channel.release());
      status != ZX_OK) {
    completer.ReplyError(status);
  }
  completer.ReplySuccess();
}

void LoopbackDevice::OpenAclDataChannel(OpenAclDataChannelRequestView request,
                                        OpenAclDataChannelCompleter::Sync& completer) {
  if (zx_status_t status = HciOpenChannel(&acl_channel_, request->channel.release());
      status != ZX_OK) {
    completer.ReplyError(status);
  }
  completer.ReplySuccess();
}

void LoopbackDevice::OpenScoDataChannel(OpenScoDataChannelRequestView request,
                                        OpenScoDataChannelCompleter::Sync& completer) {
  if (zx_status_t status = HciOpenChannel(&sco_channel_, request->channel.release());
      status != ZX_OK) {
    completer.ReplyError(status);
  }
  completer.ReplySuccess();
}

void LoopbackDevice::ConfigureSco(ConfigureScoRequestView request,
                                  ConfigureScoCompleter::Sync& completer) {
  // UART doesn't require any SCO configuration.
  completer.ReplySuccess();
}

void LoopbackDevice::ResetSco(ResetScoCompleter::Sync& completer) {
  // UART doesn't require any SCO configuration, so there's nothing to do.
  completer.ReplySuccess();
}

void LoopbackDevice::OpenIsoDataChannel(OpenIsoDataChannelRequestView request,
                                        OpenIsoDataChannelCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void LoopbackDevice::OpenSnoopChannel(OpenSnoopChannelRequestView request,
                                      OpenSnoopChannelCompleter::Sync& completer) {
  if (zx_status_t status = HciOpenChannel(&snoop_channel_, request->channel.release());
      status != ZX_OK) {
    completer.ReplyError(status);
  };
  completer.ReplySuccess();
}

void LoopbackDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Hci> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in Hci request, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

}  // namespace bt_hci_virtual
