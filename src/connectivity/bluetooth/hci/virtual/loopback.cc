// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "loopback.h"

#include <assert.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/syslog/cpp/macros.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/status.h>

namespace bt_hci_virtual {

LoopbackDevice::LoopbackDevice(zx_device_t* parent, async_dispatcher_t* dispatcher)
    : LoopbackDeviceType(parent), dispatcher_(dispatcher) {}

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

zx_status_t LoopbackDevice::BtHciOpenCommandChannel(zx::channel in) {
  FX_LOGS(TRACE) << "LoopbackDevice::BtHciOpenCommandChannel";
  return HciOpenChannel(&cmd_channel_, in.release());
}

zx_status_t LoopbackDevice::BtHciOpenAclDataChannel(zx::channel in) {
  FX_LOGS(TRACE) << "LoopbackDevice::BtHciOpenAclDataChannel";
  return HciOpenChannel(&acl_channel_, in.release());
}

zx_status_t LoopbackDevice::BtHciOpenSnoopChannel(zx::channel in) {
  FX_LOGS(TRACE) << "LoopbackDevice::BtHciOpenSnoopChannel";
  return HciOpenChannel(&snoop_channel_, in.release());
}

zx_status_t LoopbackDevice::BtHciOpenScoChannel(zx::channel in) {
  FX_LOGS(TRACE) << "LoopbackDevice::BtHciOpenScoChannel";
  return HciOpenChannel(&sco_channel_, in.release());
}

zx_status_t LoopbackDevice::BtHciOpenIsoChannel(zx::channel in) { return ZX_ERR_NOT_SUPPORTED; }

void LoopbackDevice::ChannelCleanupLocked(zx::channel* channel) {
  FX_LOGS(TRACE) << "LoopbackDevice::ChannelCleanupLocked";
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

void LoopbackDevice::SnoopChannelWriteLocked(uint8_t flags, uint8_t* bytes, size_t length) {
  FX_LOGS(TRACE) << "LoopbackDevice::SnoopChannelWriteLocked";
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
      FX_LOGS(ERROR) << "bt-loopback-device: failed to write to snoop channel"
                     << zx_status_get_string(status);
    }

    // It should be safe to clean up the channel right here as the work thread
    // never waits on this channel from outside of the lock.
    ChannelCleanupLocked(&snoop_channel_);
  }
}

void LoopbackDevice::HciBeginShutdown() {
  FX_LOGS(TRACE) << "LoopbackDevice::HciBeginShutdown";
  bool was_shutting_down = shutting_down_.exchange(true, std::memory_order_relaxed);
  if (!was_shutting_down) {
    FX_LOGS(TRACE) << "LoopbackDevice::HciBeginShutdown !was_shutting_down";
    DdkAsyncRemove();
  }
}

void LoopbackDevice::OnChannelSignal(Wait* wait, zx_status_t status,
                                     const zx_packet_signal_t* signal) {
  FX_LOGS(TRACE) << "OnChannelSignal";
  {
    std::lock_guard guard(mutex_);
    wait->pending = false;
  }

  if (wait->channel == &in_channel_) {
    HciHandleIncomingChannel(wait->channel, signal->observed);
  } else {
    HciHandleClientChannel(wait->channel, signal->observed);
  }

  // Reset waiters
  {
    std::lock_guard guard(mutex_);

    // Resume waiting for channel signals. If a packet was queued while the write was processing,
    // it should be immediately signaled.
    if (wait->channel->is_valid() && !wait->pending) {
      ZX_ASSERT(async_begin_wait(dispatcher_, wait) == ZX_OK);
      wait->pending = true;
    }
  }
}

zx_status_t LoopbackDevice::HciOpenChannel(zx::channel* in_channel, zx_handle_t in) {
  FX_LOGS(TRACE) << "LoopbackDevice::HciOpenChannel";
  std::lock_guard guard(mutex_);
  zx_status_t result = ZX_OK;

  if (in_channel->is_valid()) {
    FX_LOGS(ERROR) << "LoopbackDevice: already bound, failing";
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
  FX_LOGS(TRACE) << "HciHandleIncomingChannel readable:" << int(pending & ZX_CHANNEL_READABLE)
                 << ", closed: " << int(pending & ZX_CHANNEL_PEER_CLOSED);
  // If we are in the process of shutting down, we are done.
  if (atomic_load_explicit(&shutting_down_, std::memory_order_relaxed)) {
    return;
  }

  // Channel may have been closed since signal was received.
  if (!chan->is_valid()) {
    FX_LOGS(ERROR) << "channel is invalid";
    return;
  }

  // Handle the read signal first.  If we are also peer closed, we want to make
  // sure that we have processed all of the pending messages before cleaning up.
  if (pending & ZX_CHANNEL_READABLE) {
    uint32_t length;
    uint8_t read_buffer[kAclMaxFrameSize];
    {
      std::lock_guard guard(mutex_);
      zx_status_t status;

      status = zx_channel_read(chan->get(), 0, read_buffer, nullptr, kAclMaxFrameSize, 0, &length,
                               nullptr);
      if (status == ZX_ERR_SHOULD_WAIT) {
        FX_LOGS(WARNING) << "ignoring ZX_ERR_SHOULD_WAIT when reading incoming channel";
        return;
      }
      if (status != ZX_OK) {
        FX_LOGS(ERROR) << "failed to read from incoming channel " << zx_status_get_string(status);
        ChannelCleanupLocked(chan);
        return;
      }
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
          FX_LOGS(ERROR) << "unsupported HCI packet type " << cur_uart_packet_type_
                         << " received. We may be out of sync";
          cur_uart_packet_type_ = kHciNone;
          return;
      }
    }
  }

  if (pending & ZX_CHANNEL_PEER_CLOSED) {
    {
      std::lock_guard guard(mutex_);
      ChannelCleanupLocked(chan);
    }
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
    FX_LOGS(ERROR) << "packet_length is too large (" << packet_length << " > " << buffer_size
                   << ") during packet reassembly. Dropping and "
                      "attempting to re-sync.";

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

  // Attempt to send this packet to the channel. Do so in the lock so we don't shut down while
  // writing.
  if (channel->is_valid()) {
    zx_status_t status = channel->write(/*flags=*/0, &buffer[1], packet_length - 1, nullptr, 0);
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "failed to write packet: " << zx_status_get_string(status);
      ChannelCleanupLocked(&acl_channel_);
    }
  }

  // If the snoop channel is open then try to write the packet even if |channel| was closed.
  SnoopChannelWriteLocked(bt_hci_snoop_flags(snoop_type, true), &buffer[1], packet_length - 1);

  // reset buffer
  cur_uart_packet_type_ = kHciNone;
  *buffer_offset = 1;
}

void LoopbackDevice::HciHandleClientChannel(zx::channel* chan, zx_signals_t pending) {
  FX_LOGS(TRACE) << "LoopbackDevice::HciHandleClientChannel";
  // Channel may have been closed since signal was received.
  if (!chan->is_valid()) {
    FX_LOGS(ERROR) << "chan invalid";
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

  FX_LOGS(TRACE) << "LoopbackDevice::HciHandleClientChannel handling " << chan_name;

  // Handle the read signal first.  If we are also peer closed, we want to make
  // sure that we have processed all of the pending messages before cleaning up.
  if (pending & ZX_CHANNEL_READABLE) {
    zx_status_t status = ZX_OK;
    uint32_t length = max_buf_size - 1;
    {
      std::lock_guard guard(mutex_);

      status =
          zx_channel_read(chan->get(), 0, write_buffer_ + 1, nullptr, length, 0, &length, nullptr);
      if (status == ZX_ERR_SHOULD_WAIT) {
        FX_LOGS(WARNING) << "ignoring ZX_ERR_SHOULD_WAIT when reading " << chan_name << " channel";
        return;
      }
      if (status != ZX_OK) {
        FX_LOGS(ERROR) << "hci_read_thread: failed to read from " << chan_name << " channel "
                       << zx_status_get_string(status);
        ChannelCleanupLocked(chan);
        return;
      }

      write_buffer_[0] = packet_type;
      length++;

      SnoopChannelWriteLocked(bt_hci_snoop_flags(snoop_type, false), write_buffer_ + 1, length - 1);

      if (in_channel_.is_valid()) {
        status = in_channel_.write(/*flags=*/0, write_buffer_, length, nullptr, 0);
        if (status != ZX_OK) {
          FX_LOGS(ERROR) << "failed to write packet: " << zx_status_get_string(status);
        }
      }
    }
    if (status != ZX_OK) {
      HciBeginShutdown();
    }
  }

  if (pending & ZX_CHANNEL_PEER_CLOSED) {
    FX_LOGS(DEBUG) << "received closed signal for " << chan_name << " channel";
    std::lock_guard guard(mutex_);
    ChannelCleanupLocked(chan);
  }
}

void LoopbackDevice::BtHciConfigureSco(sco_coding_format_t coding_format, sco_encoding_t encoding,
                                       sco_sample_rate_t sample_rate,
                                       bt_hci_configure_sco_callback callback, void* cookie) {
  FX_LOGS(TRACE) << "LoopbackDevice::BtHciConfigureSco";
  // UART doesn't require any SCO configuration.
  callback(cookie, ZX_OK);
}

void LoopbackDevice::BtHciResetSco(bt_hci_reset_sco_callback callback, void* cookie) {
  FX_LOGS(TRACE) << "LoopbackDevice::BtHciResetSco";
  // UART doesn't require any SCO configuration, so there's nothing to do.
  callback(cookie, ZX_OK);
}

void LoopbackDevice::DdkUnbind(ddk::UnbindTxn txn) {
  FX_LOGS(TRACE) << "LoopbackDevice::DdkUnbind";
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
    ChannelCleanupLocked(&in_channel_);
  }

  if (loop_) {
    loop_->Quit();
    loop_->JoinThreads();
  }

  // Tell the DDK we are done unbinding.
  txn.Reply();
}

void LoopbackDevice::DdkRelease() {
  FX_LOGS(TRACE) << "LoopbackDevice::DdkRelease";
  // Driver manager is given a raw pointer to this dynamically allocated object in Create(), so
  // when DdkRelease() is called we need to free the allocated memory.
  delete this;
}

void LoopbackDevice::OpenCommandChannel(OpenCommandChannelRequestView request,
                                        OpenCommandChannelCompleter::Sync& completer) {
  BtHciOpenCommandChannel(zx::channel(request->channel.release()));
}

void LoopbackDevice::OpenAclDataChannel(OpenAclDataChannelRequestView request,
                                        OpenAclDataChannelCompleter::Sync& completer) {
  BtHciOpenAclDataChannel(zx::channel(request->channel.release()));
}

void LoopbackDevice::OpenSnoopChannel(OpenSnoopChannelRequestView request,
                                      OpenSnoopChannelCompleter::Sync& completer) {
  BtHciOpenSnoopChannel(zx::channel(request->channel.release()));
}

zx_status_t LoopbackDevice::DdkGetProtocol(uint32_t proto_id, void* out_proto) {
  if (proto_id != ZX_PROTOCOL_BT_HCI) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  bt_hci_protocol_t* hci_proto = static_cast<bt_hci_protocol*>(out_proto);
  hci_proto->ops = &bt_hci_protocol_ops_;
  hci_proto->ctx = this;
  return ZX_OK;
}

zx_status_t LoopbackDevice::Bind(zx_handle_t channel, std::string_view name) {
  FX_LOGS(TRACE) << "LoopbackDevice::Bind";
  zx_status_t result = ZX_OK;
  {
    std::lock_guard guard(mutex_);

    // pre-populate event packet indicators
    event_buffer_[0] = kHciEvent;
    event_buffer_offset_ = 1;
    acl_buffer_[0] = kHciAclData;
    acl_buffer_offset_ = 1;
    sco_buffer_[0] = kHciSco;
    sco_buffer_offset_ = 1;
  }

  // Spawn a new thread in production. In tests, use the test dispatcher provided in the
  // constructor.
  if (!dispatcher_) {
    loop_.emplace(&kAsyncLoopConfigNoAttachToCurrentThread);
    result = loop_->StartThread("bt-loopback-device");
    if (result != ZX_OK) {
      FX_LOGS(ERROR) << "failed to start thread: " << zx_status_get_string(result);
      DdkRelease();
      return result;
    }
    dispatcher_ = loop_->dispatcher();
  }

  {
    FX_LOGS(TRACE) << "LoopbackDevice::Bind setup in channel waiter";
    std::lock_guard guard(mutex_);
    // Setup up incoming channel waiter.
    in_channel_.reset(channel);
    in_channel_wait_.object = in_channel_.get();
    in_channel_wait_.trigger = ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED;
    ZX_ASSERT(async_begin_wait(dispatcher_, &in_channel_wait_) == ZX_OK);
    in_channel_wait_.pending = true;
  }

  ddk::DeviceAddArgs args(name.data());
  args.set_proto_id(ZX_PROTOCOL_BT_HCI);
  return DdkAdd(args);
}

}  // namespace bt_hci_virtual
