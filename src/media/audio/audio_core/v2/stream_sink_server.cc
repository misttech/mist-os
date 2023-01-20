// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/stream_sink_server.h"

#include <lib/fit/defer.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>

#include "src/media/audio/services/common/logging.h"

namespace media_audio {

using fuchsia_audio::wire::Timestamp;
using fuchsia_media2::ConsumerClosedReason;

// static
std::shared_ptr<StreamSinkServer> StreamSinkServer::Create(
    std::shared_ptr<const FidlThread> thread, fidl::ServerEnd<fuchsia_audio::StreamSink> server_end,
    Args args) {
  return BaseFidlServer::Create(std::move(thread), std::move(server_end), std::move(args));
}

StreamSinkServer::StreamSinkServer(Args args)
    : format_(args.format),
      stream_converter_(StreamConverter::CreateFromFloatSource(args.format)),
      payload_buffer_(std::move(args.payload_buffer)) {}

void StreamSinkServer::PutPacket(PutPacketRequestView request,
                                 PutPacketCompleter::Sync& completer) {
  TRACE_DURATION("audio", "StreamSink::PutPacket");
  ScopedThreadChecker checker(thread().checker());

  if (!request->has_packet()) {
    FX_LOGS(WARNING) << "PutPacket: missing packet";
    CloseWithReason(ConsumerClosedReason::kInvalidPacket);
    return;
  }
  if (!request->packet().has_payload()) {
    FX_LOGS(WARNING) << "PutPacket: missing payload";
    CloseWithReason(ConsumerClosedReason::kInvalidPacket);
    return;
  }

  const auto& packet = request->packet();
  if (packet.has_flags() || packet.has_front_frames_to_drop() || packet.has_back_frames_to_drop() ||
      packet.has_encryption_properties()) {
    FX_LOGS(WARNING) << "PutPacket: unsupported field";
    CloseWithReason(ConsumerClosedReason::kInvalidPacket);
    return;
  }

  const auto which_timestamp =
      packet.has_timestamp() ? packet.timestamp().Which() : Timestamp::Tag::kUnspecifiedBestEffort;

  zx::time timestamp;
  zx::duration overflow;

  // TODO(fxbug.dev/98652): once the mixer service populates `Packet.capture_timestamp`, we can
  // ignore `packet.timestamp` and instead use `packet.capture_timestamp`.
  switch (which_timestamp) {
    case Timestamp::Tag::kSpecified:
      timestamp = zx::time(packet.timestamp().specified());
      if (next_continuous_timestamp_) {
        if (*next_continuous_timestamp_ > timestamp) {
          FX_LOGS(WARNING) << "packet timestamp went backwards from " << *next_continuous_timestamp_
                           << " to " << timestamp;
        } else {
          overflow = timestamp - *next_continuous_timestamp_;
        }
      }
      break;
    case Timestamp::Tag::kUnspecifiedContinuous:
      // The mixer service uses Specified for the first packet in a sequence, therefore
      // `next_continuous_timestamp_` should always be defined.
      if (!next_continuous_timestamp_) {
        FX_LOGS(WARNING) << "kUnspecifiedContinuous timestamp not preceded by Specified timestamp";
        Shutdown(ZX_ERR_NOT_SUPPORTED);
      }
      timestamp = *next_continuous_timestamp_;
      break;
    case Timestamp::Tag::kUnspecifiedBestEffort:
      // The mixer service ConsumerNodes never use BestEffort, so we don't need to support it.
      FX_LOGS(WARNING) << "UnspecifiedBestEffort timestamps not supported";
      Shutdown(ZX_ERR_NOT_SUPPORTED);
      return;
    default:
      FX_LOGS(WARNING) << "PutPacket: unepxected packet timestamp tag = "
                       << static_cast<int>(packet.timestamp().Which());
      CloseWithReason(ConsumerClosedReason::kInvalidPacket);
      return;
  }

  const auto& payload = packet.payload();
  if (payload.buffer_id != 0) {
    FX_LOGS(WARNING) << "PutPacket: unknown payload buffer id " << payload.buffer_id;
    CloseWithReason(ConsumerClosedReason::kInvalidPacket);
    return;
  }

  // Since the offset is an unsigned integer, the payload is out-of-range if its endpoint is too
  // large or wraps around.
  const uint64_t payload_offset_end = payload.offset + payload.size;
  if (payload_offset_end > payload_buffer_->size() || payload_offset_end < payload.offset) {
    FX_LOGS(WARNING) << "PutPacket: payload buffer out-of-range: offset=" << payload.offset
                     << ", size=" << payload.size << " buffer_size=" << payload_buffer_->size();
    CloseWithReason(ConsumerClosedReason::kInvalidPacket);
    return;
  }
  if (payload.size % format_.bytes_per_frame() != 0) {
    FX_LOGS(WARNING) << "PutPacket: payload buffer has a non-integral number of frames";
    CloseWithReason(ConsumerClosedReason::kInvalidPacket);
    return;
  }

  pending_packets_.push_back({
      .timestamp = timestamp,
      .overflow = overflow,
      .data = static_cast<char*>(payload_buffer_->offset(payload.offset)),
      .bytes_remaining = static_cast<int64_t>(payload.size),
      .fence = request->has_release_fence() ? std::move(request->release_fence()) : zx::eventpair(),
  });

  next_continuous_timestamp_ =
      timestamp + format_.duration_per(Fixed(payload.size / format_.bytes_per_frame()));

  ServePendingCaptures();
}

void StreamSinkServer::StartSegment(StartSegmentRequestView request,
                                    StartSegmentCompleter::Sync& completer) {
  TRACE_DURATION("audio", "StreamSink::StartSegment");

  // The mixer service ConsumerNodes never use StartSegment, so we don't need to implement it.
  FX_LOGS(WARNING) << "StartSegment not supported";
  Shutdown(ZX_ERR_NOT_SUPPORTED);
}

void StreamSinkServer::End(EndCompleter::Sync& completer) {
  // End-of-stream is not used in audio_core.
}

void StreamSinkServer::WillClose(WillCloseRequestView request,
                                 WillCloseCompleter::Sync& completer) {
  TRACE_DURATION("audio", "StreamSink::WillClose");
  ScopedThreadChecker checker(thread().checker());

  if (request->has_reason()) {
    FX_LOGS(INFO) << "StreamSink closing with reason " << static_cast<uint32_t>(request->reason());
  }
}

void StreamSinkServer::CapturePacket(void* dest_buffer, int64_t dest_bytes,
                                     CaptureCallback callback) {
  pending_captures_.push_back({
      .data = static_cast<char*>(dest_buffer),
      .bytes_remaining = dest_bytes,
      .bytes_captured = 0,
      .callback = std::move(callback),
  });
  ServePendingCaptures();
}

void StreamSinkServer::DiscardPackets() {
  pending_packets_.clear();
  while (!pending_captures_.empty()) {
    auto& capture = pending_captures_.front();
    capture.callback(capture.start_timestamp.value_or(zx::time(0)).get(), capture.bytes_captured,
                     capture.overflow);
    pending_captures_.pop_front();
  }
}

void StreamSinkServer::ServePendingCaptures() {
  while (!pending_captures_.empty() && !pending_packets_.empty()) {
    auto& packet = pending_packets_.front();
    auto& capture = pending_captures_.front();

    capture.overflow += packet.overflow;
    packet.overflow = zx::nsec(0);

    if (!capture.start_timestamp) {
      // We haven't copied anything into `capture` yet, so start from `packet`'s current position.
      capture.start_timestamp = packet.timestamp;
      capture.next_timestamp = packet.timestamp;
    } else {
      FX_CHECK(capture.next_timestamp);
      // If `capture`'s current position doesn't align with `packet`, fill with silence. `capture`
      // cannot be ahead of `packet`, since consecutive packet timestamps cannot go backwards.
      const auto duration_ahead = packet.timestamp - *capture.next_timestamp;
      const auto frames_ahead = format_.integer_frames_per(duration_ahead);
      FX_CHECK(frames_ahead >= 0);
      if (frames_ahead > 0) {
        const auto capture_frames_needed = capture.bytes_remaining / format_.bytes_per_frame();
        const auto silent_frames = std::max(frames_ahead, capture_frames_needed);
        const auto silent_duration = format_.duration_per(Fixed(silent_frames));
        const auto silent_bytes = silent_frames * format_.bytes_per_frame();

        stream_converter_.WriteSilence(capture.data, silent_frames);
        capture.bytes_remaining -= silent_bytes;
        capture.bytes_captured += silent_bytes;
        *capture.next_timestamp += silent_duration;

        // If this `capture` is done, advance to the next `capture`.
        if (capture.bytes_remaining == 0) {
          capture.callback(capture.start_timestamp->get(), capture.bytes_captured,
                           capture.overflow);
          pending_captures_.pop_front();
          continue;
        }
      }
    }

    // Copy `packet` into `capture`.
    const auto bytes_to_copy = std::min(capture.bytes_remaining, packet.bytes_remaining);
    memmove(capture.data, packet.data, bytes_to_copy);

    capture.data += bytes_to_copy;
    capture.bytes_remaining -= bytes_to_copy;
    capture.bytes_captured += bytes_to_copy;

    packet.data += bytes_to_copy;
    packet.bytes_remaining -= bytes_to_copy;

    const auto frames_copied = bytes_to_copy / format_.bytes_per_frame();
    const auto duration_copied = format_.duration_per(Fixed(frames_copied));
    *capture.next_timestamp += duration_copied;
    packet.timestamp += duration_copied;

    if (packet.bytes_remaining == 0) {
      pending_packets_.pop_front();
    }

    if (capture.bytes_remaining == 0) {
      capture.callback(capture.start_timestamp->get(), capture.bytes_captured, capture.overflow);
      pending_captures_.pop_front();
    }
  }
}

void StreamSinkServer::CloseWithReason(ConsumerClosedReason reason) {
  fidl::Arena<> arena;
  std::ignore = fidl::WireSendEvent(binding())->OnWillClose(
      fuchsia_audio::wire::StreamSinkOnWillCloseRequest::Builder(arena).reason(reason).Build());
  Shutdown();
}

}  // namespace media_audio
