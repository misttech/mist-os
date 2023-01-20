// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_STREAM_SINK_SERVER_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_STREAM_SINK_SERVER_H_

#include <fidl/fuchsia.audio/cpp/wire.h>
#include <fidl/fuchsia.media/cpp/wire.h>
#include <fidl/fuchsia.media2/cpp/wire.h>
#include <zircon/errors.h>

#include <deque>
#include <memory>
#include <unordered_map>

#include "src/media/audio/lib/format2/format.h"
#include "src/media/audio/lib/format2/stream_converter.h"
#include "src/media/audio/services/common/base_fidl_server.h"
#include "src/media/audio/services/common/memory_mapped_buffer.h"

namespace media_audio {

// This is intended to be used by capturers. Each capturer is represented by a ConsumerNode, which
// sends StreamSink messages to this server, which records the sequence of incoming packets, which
// can be read by the CapturePacket method.
class StreamSinkServer
    : public BaseFidlServer<StreamSinkServer, fidl::WireServer, fuchsia_audio::StreamSink> {
 public:
  struct Args {
    // Format of packets sent to this StreamSink.
    Format format;

    // The StreamSink client writes packets to this payload buffer.
    std::shared_ptr<MemoryMappedBuffer> payload_buffer;
  };

  // The returned server will live until the `server_end` channel is closed.
  static std::shared_ptr<StreamSinkServer> Create(
      std::shared_ptr<const FidlThread> thread,
      fidl::ServerEnd<fuchsia_audio::StreamSink> server_end, Args args);

  // Implementation of fidl::WireServer<fuchsia_audio::StreamSink>.
  void PutPacket(PutPacketRequestView request, PutPacketCompleter::Sync& completer) final;
  void StartSegment(StartSegmentRequestView request, StartSegmentCompleter::Sync& completer) final;
  void End(EndCompleter::Sync& completer) final;
  void WillClose(WillCloseRequestView request, WillCloseCompleter::Sync& completer) final;

  // Waits until `dest_bytes` has arrived via PutPacket, then writes that to `dest_buffer`. The
  // callback is invoked once the packet is written. If multiple CapturePacket calls are pending
  // concurrently, they are handled in sequential order. If sequential calls happen quickly enough,
  // this will capture a continuous sequence of packets, otherwise there may be overflows.
  using CaptureCallback =
      fit::callback<void(int64_t timestamp, int64_t bytes_captured, zx::duration overflow)>;
  void CapturePacket(void* dest_buffer, int64_t dest_bytes, CaptureCallback callback);

  // Drop all pending packets.
  void DiscardPackets();

 private:
  static inline constexpr std::string_view kClassName = "StreamSinkServer";
  template <typename ServerT, template <typename T> typename FidlServerT, typename ProtocolT>
  friend class BaseFidlServer;

  explicit StreamSinkServer(Args args);
  void ServePendingCaptures();
  void CloseWithReason(fuchsia_media2::ConsumerClosedReason reason);

  const Format format_;
  const StreamConverter stream_converter_;
  const std::shared_ptr<MemoryMappedBuffer> payload_buffer_;

  struct PendingPacket {
    zx::time timestamp;
    zx::duration overflow;  // how much overflow happened before this packet
    char* data;
    int64_t bytes_remaining;
    zx::eventpair fence;
  };
  std::deque<PendingPacket> pending_packets_;

  struct PendingCapture {
    char* data;
    int64_t bytes_remaining;
    int64_t bytes_captured;
    std::optional<zx::time> start_timestamp;
    std::optional<zx::time> next_timestamp;
    zx::duration overflow;
    CaptureCallback callback;
  };
  std::deque<PendingCapture> pending_captures_;

  std::optional<zx::time> next_continuous_timestamp_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_STREAM_SINK_SERVER_H_
