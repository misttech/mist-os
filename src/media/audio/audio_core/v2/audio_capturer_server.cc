// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/audio_capturer_server.h"

#include <fidl/fuchsia.audio/cpp/fidl.h>
#include <fidl/fuchsia.media/cpp/fidl.h>
#include <fidl/fuchsia.media2/cpp/wire_types.h>
#include <lib/async/cpp/wait.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>

#include "src/media/audio/audio_core/shared/mixer/gain.h"

namespace media_audio {

namespace {

using ::fuchsia_audio::wire::GainControlSetGainRequest;
using ::fuchsia_audio::wire::GainUpdateMethod;
using ::media::audio::CaptureUsage;

int64_t num_capturers = 0;

template <typename ResultT>
bool LogResultError(const ResultT& result, const char* debug_context) {
  if (!result.ok()) {
    FX_LOGS(WARNING) << debug_context << ": failed with transport error: " << result;
    return true;
  }
  if (!result->is_ok()) {
    FX_LOGS(ERROR) << debug_context
                   << ": failed with code: " << fidl::ToUnderlying(result->error_value());
    return true;
  }
  return false;
}

template <typename HandleT>
HandleT DupHandle(const HandleT& in) {
  HandleT out;
  if (auto status = in.duplicate(ZX_RIGHT_SAME_RIGHTS, &out); status != ZX_OK) {
    FX_PLOGS(FATAL, status) << "zx::object::duplicate failed";
  }
  return out;
}

}  // namespace

// static
std::shared_ptr<AudioCapturerServer> AudioCapturerServer::Create(
    std::shared_ptr<const FidlThread> fidl_thread,
    fidl::ServerEnd<fuchsia_media::AudioCapturer> server_end, Args args) {
  return BaseFidlServer::Create(fidl_thread, std::move(server_end), fidl_thread, std::move(args));
}

AudioCapturerServer::AudioCapturerServer(std::shared_ptr<const FidlThread> fidl_thread, Args args)
    : capturer_id_(++num_capturers),
      default_reference_clock_(std::move(args.default_reference_clock)),
      on_fully_created_(std::move(args.on_fully_created)),
      on_shutdown_(std::move(args.on_shutdown)),
      graph_client_(std::move(args.graph_client)),
      usage_(args.usage),
      format_(args.format),
      reference_clock_(DupHandle(default_reference_clock_)) {
  FX_CHECK(default_reference_clock_);
  FX_CHECK(graph_client_);

  if (usage_ == CaptureUsage::ULTRASOUND) {
    FX_CHECK(format_);
  }
}

media::audio::CaptureUsage AudioCapturerServer::usage() const {
  FX_CHECK(IsConfigured());
  return usage_;
}

const Format& AudioCapturerServer::format() const {
  FX_CHECK(IsConfigured());
  return *format_;
}

NodeId AudioCapturerServer::consumer_node() const {
  FX_CHECK(IsConfigured());
  return *consumer_node_;
}

GainControlId AudioCapturerServer::stream_gain_control() const {
  FX_CHECK(IsConfigured());
  return *stream_gain_control_;
}

void AudioCapturerServer::SetReferenceClock(SetReferenceClockRequestView request,
                                            SetReferenceClockCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioCapturerServer::SetReferenceClock");

  if (usage_ == CaptureUsage::ULTRASOUND) {
    FX_LOGS(WARNING) << "Unsupported method SetReferenceClock on ultrasound capturer";
    Shutdown(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  if (IsConfigured()) {
    FX_LOGS(WARNING) << "Reference clock cannot be set once configured.";
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }

  if (!request->reference_clock.is_valid()) {
    reference_clock_ = DupHandle(default_reference_clock_);
    return;
  }

  auto clock = std::move(request->reference_clock);
  constexpr auto kRequiredRights = ZX_RIGHT_DUPLICATE | ZX_RIGHT_TRANSFER | ZX_RIGHT_READ;
  if (auto status = clock.replace(kRequiredRights, &clock); status != ZX_OK) {
    FX_LOGS(WARNING) << "Reference clock has insufficient rights";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  reference_clock_ = std::move(clock);
}

void AudioCapturerServer::SetUsage(SetUsageRequestView request,
                                   SetUsageCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioCapturerServer::SetUsage");

  if (usage_ == CaptureUsage::ULTRASOUND || usage_ == CaptureUsage::LOOPBACK) {
    FX_LOGS(WARNING) << "Unsupported method SetUsage on ultrasound or loopback capturer";
    Shutdown(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  if (IsConfigured()) {
    FX_LOGS(WARNING) << "Usage cannot be set once configured.";
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }

  usage_ = media::audio::ToCaptureUsage(request->usage);
}

void AudioCapturerServer::SetPcmStreamType(SetPcmStreamTypeRequestView request,
                                           SetPcmStreamTypeCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioCapturerServer::SetPcmStreamType");

  if (usage_ == CaptureUsage::ULTRASOUND) {
    FX_LOGS(WARNING) << "Unsupported method SetPcmStreamType on ultrasound capturer";
    Shutdown(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  if (IsConfigured()) {
    FX_LOGS(WARNING) << "Format cannot be set once configured.";
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }

  auto format_result = Format::CreateLegacy(request->stream_type);
  if (format_result.is_error()) {
    FX_LOGS(WARNING) << "AudioCapturer: PcmStreamType is invalid: " << format_result.error();
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  format_ = std::move(format_result.value());

  // TODO(https://fxbug.dev/42181009): call reporter
  // reporter_->SetFormat(*format_);

  MaybeConfigure();
}

void AudioCapturerServer::CaptureAt(CaptureAtRequestView request,
                                    CaptureAtCompleter::Sync& completer) {
  RunWhenReady("CaptureAt", [this, payload_buffer_id = request->payload_buffer_id,
                             payload_offset = request->payload_offset, frames = request->frames,
                             completer = completer.ToAsync()]() mutable {
    CaptureAtInternal(payload_buffer_id, payload_offset, frames, std::move(completer));
  });
}

void AudioCapturerServer::StartAsyncCapture(StartAsyncCaptureRequestView request,
                                            StartAsyncCaptureCompleter::Sync& completer) {
  RunWhenReady("StartAsyncCapture",
               [this, frames_per_packet = request->frames_per_packet]() mutable {
                 StartAsyncCaptureInternal(frames_per_packet);
               });
}

void AudioCapturerServer::StopAsyncCapture(StopAsyncCaptureCompleter::Sync& completer) {
  RunWhenReady("StopAsyncCapture", [this, completer = completer.ToAsync()]() mutable {
    StopAsyncCaptureInternal(std::move(completer));
  });
}

void AudioCapturerServer::StopAsyncCaptureNoReply(
    StopAsyncCaptureNoReplyCompleter::Sync& completer) {
  RunWhenReady("StopAsyncCaptureNoReply",
               [this]() mutable { StopAsyncCaptureInternal(std::nullopt); });
}

void AudioCapturerServer::ReleasePacket(ReleasePacketRequestView request,
                                        ReleasePacketCompleter::Sync& completer) {
  // Although `packet` is a wire type, it's safe to copy `packet` into the closure because `packet`
  // is a POD (no strings, tables, or unions) -- it's a value type that can be copied.
  RunWhenReady("ReleasePacket",
               [this, packet = request->packet]() mutable { ReleasePacketInternal(packet); });
}

void AudioCapturerServer::DiscardAllPackets(DiscardAllPacketsCompleter::Sync& completer) {
  RunWhenReady("DiscardAllPackets", [this, completer = completer.ToAsync()]() mutable {
    DiscardAllPacketsInternal(std::move(completer));
  });
}

void AudioCapturerServer::DiscardAllPacketsNoReply(
    DiscardAllPacketsNoReplyCompleter::Sync& completer) {
  RunWhenReady("DiscardAllPacketsNoReply", [this]() { DiscardAllPacketsInternal(std::nullopt); });
}

void AudioCapturerServer::AddPayloadBuffer(AddPayloadBufferRequestView request,
                                           AddPayloadBufferCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioCapturerServer::AddPayloadBuffer");

  // This restriction is also present in audio_core v1.
  if (IsConfigured()) {
    FX_LOGS(WARNING) << "AddPayloadBuffer cannot be called once configured.";
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }

  // The v1 implementation has this same limitation.
  if (request->id != 0) {
    FX_LOGS(WARNING) << "AddPayloadBuffer id must be 0, is " << request->id;
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (payload_buffer_) {
    FX_LOGS(WARNING) << "AddPayloadBuffer id " << request->id << " already exists";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }
  if (!request->payload_buffer.is_valid()) {
    FX_LOGS(WARNING) << "AddPayloadBuffer VMO must be valid";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  // TODO(https://fxbug.dev/42181009): call reporter
  // reporter_->AddPayloadBuffer(request->id, vmo_size);

  payload_buffer_ = std::move(request->payload_buffer);
  MaybeConfigure();
}

void AudioCapturerServer::RemovePayloadBuffer(RemovePayloadBufferRequestView request,
                                              RemovePayloadBufferCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioCapturerServer::RemovePayloadBuffer");

  // The v1 implementation has this same limitation.
  FX_LOGS(WARNING) << "RemovePayloadBuffer is not currently supported";
  Shutdown(ZX_ERR_NOT_SUPPORTED);
}

// TODO(https://fxbug.dev/42181009): implement: need to create a fuchsia.media.audio.GainControl
// server that forwards to stream_gain_control_client_
void AudioCapturerServer::BindGainControl(BindGainControlRequestView request,
                                          BindGainControlCompleter::Sync& completer) {
  FX_LOGS(ERROR) << "BindGainControl not implemented";
  Shutdown(ZX_ERR_NOT_SUPPORTED);
}

void AudioCapturerServer::GetReferenceClock(GetReferenceClockCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioCapturerServer::GetReferenceCLock");
  completer.Reply(DupHandle(reference_clock_));
}

void AudioCapturerServer::GetStreamType(GetStreamTypeCompleter::Sync& completer) {
  if (!format_) {
    FX_LOGS(WARNING) << "GetStreamType cannot be called before a stream type is selected";
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }

  fidl::Arena<> arena;
  completer.Reply({
      .medium_specific = fuchsia_media::wire::MediumSpecificStreamType::WithAudio(
          arena, format_->ToLegacyMediaWireFidl()),
      .encoding = fidl::StringView::FromExternal(fuchsia_media::wire::kAudioEncodingLpcm),
  });
}

void AudioCapturerServer::CaptureAtInternal(uint32_t payload_buffer_id, uint32_t payload_offset,
                                            uint32_t frames_per_packet,
                                            CaptureAtCompleter::Async completer) {
  TRACE_DURATION("audio", "AudioCapturerServer::CaptureAtInternal");

  if (payload_buffer_id != 0) {
    FX_LOGS(WARNING) << "payload_buffer_id must be 0";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (frames_per_packet == 0) {
    FX_LOGS(WARNING) << "frames_per_packet must be > 0";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  size_t payload_size = frames_per_packet * format_->bytes_per_frame();
  size_t payload_end = payload_offset + payload_size;
  if (payload_end > payload_buffer_dest_->size()) {
    FX_LOGS(WARNING) << "payload out-of-range: payload_offset=" << payload_offset
                     << " frames_per_packet=" << frames_per_packet << " payload_end=" << payload_end
                     << " payload_buffer_size=" << payload_buffer_dest_->size();
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  switch (state_) {
    case State::kFullyCreated:
      // Flush packets still pending from a prior async capture, if any.
      stream_sink_server_->DiscardPackets();
      // Start the ConsumerNode. It will keep running until we receive StartAsyncCapture (which
      // switches to async mode) followed by StopAsyncCapture.
      Start();
      state_ = State::kCapturingSync;
      break;
    case State::kCapturingSync:
      // The ConsumerNode is already running.
      break;
    default:
      FX_LOGS(WARNING) << "CaptureAt called in bad state " << static_cast<int>(state_);
      Shutdown(ZX_ERR_BAD_STATE);
      return;
  }

  pending_sync_captures_++;
  stream_sink_server_->CapturePacket(
      payload_buffer_dest_->offset(payload_offset), payload_size,
      [this, self = shared_from_this(), payload_offset, completer = std::move(completer)](
          auto timestamp, auto bytes_captured, auto overflow) mutable {
        auto packet = fuchsia_media::wire::StreamPacket{
            .pts = timestamp,
            .payload_buffer_id = 0,
            .payload_offset = payload_offset,
            .payload_size = static_cast<uint64_t>(bytes_captured),
        };
        SendPacket(packet, overflow, std::move(completer));
        pending_sync_captures_--;
      });
}

void AudioCapturerServer::StartAsyncCaptureInternal(uint32_t frames_per_packet) {
  TRACE_DURATION("audio", "AudioCapturerServer::StartAsyncCaptureInternal");

  const size_t bytes_per_packet =
      static_cast<size_t>(frames_per_packet * format_->bytes_per_frame());
  const size_t packet_count = payload_buffer_dest_->size() / bytes_per_packet;
  if (packet_count == 0) {
    FX_LOGS(WARNING) << "Too many frames_per_packet=" << frames_per_packet
                     << "; bytes_per_packet=" << bytes_per_packet
                     << ", payload_buffer_size=" << payload_buffer_dest_->size();
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  switch (state_) {
    case State::kFullyCreated:
      // Flush packets still pending from a prior async capture, if any.
      stream_sink_server_->DiscardPackets();
      // Start the ConsumerNode. It will keep running until we receive StopAsyncCapture.
      Start();
      state_ = State::kCapturingAsync;
      break;
    case State::kCapturingSync:
      if (pending_sync_captures_ != 0) {
        FX_LOGS(WARNING) << "StartAsyncCapture called while CaptureAt is still pending";
        Shutdown(ZX_ERR_BAD_STATE);
        return;
      }
      // We can switch to async mode without restarting the ConsumerNode.
      state_ = State::kCapturingAsync;
      break;
    default:
      FX_LOGS(WARNING) << "CaptureAt called in bad state " << static_cast<int>(state_);
      Shutdown(ZX_ERR_BAD_STATE);
      return;
  }

  // Prime the `stream_sink_server_` with enough CapturePacket calls to fill our payload buffer.
  for (size_t k = 0; k < packet_count; k++) {
    fuchsia_media::wire::StreamPacket packet{
        .payload_buffer_id = 0,
        .payload_offset = k * bytes_per_packet,
        .payload_size = bytes_per_packet,
    };

    // ReleasePacket will call CapturePacket again with the released packet.
    stream_sink_server_->CapturePacket(
        payload_buffer_dest_->offset(packet.payload_offset), packet.payload_size,
        [this, self = shared_from_this(), packet](auto timestamp, auto bytes_captured,
                                                  auto overflow) mutable {
          FX_CHECK(static_cast<uint64_t>(bytes_captured) == packet.payload_size);
          packet.pts = timestamp;
          SendPacket(packet, overflow, std::nullopt);
        });
  }
}

void AudioCapturerServer::StopAsyncCaptureInternal(
    std::optional<StopAsyncCaptureCompleter::Async> completer) {
  TRACE_DURATION("audio", "AudioCapturerServer::StopAsyncCaptureInternal");

  switch (state_) {
    case State::kCapturingAsync:
      Stop();
      state_ = State::kFullyCreated;
      break;
    default:
      FX_LOGS(WARNING) << "StopAsyncCapture called in bad state " << static_cast<int>(state_);
      Shutdown(ZX_ERR_BAD_STATE);
      return;
  }
}

void AudioCapturerServer::ReleasePacketInternal(fuchsia_media::wire::StreamPacket packet) {
  TRACE_DURATION("audio", "AudioCapturerServer::ReleasePacketInternal");

  if (state_ != State::kCapturingAsync) {
    FX_LOGS(WARNING) << "ReleasePacket called in bad state " << static_cast<int>(state_);
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }

  // Recycle this payload range for another packet.
  stream_sink_server_->CapturePacket(
      payload_buffer_dest_->offset(packet.payload_offset), packet.payload_size,
      [this, self = shared_from_this(), packet](auto timestamp, auto bytes_captured,
                                                auto overflow) mutable {
        FX_CHECK(static_cast<uint64_t>(bytes_captured) == packet.payload_size);
        packet.pts = timestamp;
        SendPacket(packet, overflow, std::nullopt);
      });
}

void AudioCapturerServer::DiscardAllPacketsInternal(
    std::optional<DiscardAllPacketsCompleter::Async> completer) {
  TRACE_DURATION("audio", "AudioCapturerServer::DiscardAllPacketsInternal");

  if (state_ != State::kCapturingSync) {
    FX_LOGS(WARNING) << "DiscardAllPackets called in bad state '" << static_cast<int>(state_);
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }

  stream_sink_server_->DiscardPackets();
}

// TODO(https://fxbug.dev/42181009): at most one start or stop command can be pending; if one is
// already pending, cancel it before sending the new start command
void AudioCapturerServer::Start() {
  // Start the ConsumerNode at "now". To ensure that packets are timestamped with the reference time
  // at which they were captured, pick an explicit start time `T=now` and call Graph.Start with the
  // correspondence pair `(ReferenceTime=T, PacketTimestamp=T)`.
  //
  // TODO(https://fxbug.dev/42181009): once the mixer service populates `Packet.capture_timestamp`,
  // this will be unnecessary. Instead, we can start at `(RealTime=ASAP,
  // PacketTimestamp=<anything>)`. Our `stream_sink_server_` will use `Packet.capture_timestamp` and
  // will ignore `Packet.timestamp`.
  zx_time_t now;
  if (auto status = reference_clock_.read(&now); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Unexpected failure to read reference_clock_";
    Shutdown(ZX_ERR_INTERNAL);
    return;
  }

  fidl::Arena<> arena;
  (*graph_client_)
      ->Start(fuchsia_audio_mixer::wire::GraphStartRequest::Builder(arena)
                  .node_id(*consumer_node_)
                  .when(fuchsia_media2::wire::RealTime::WithReferenceTime(arena, now))
                  .stream_time(fuchsia_media2::wire::StreamTime::WithPacketTimestamp(arena, now))
                  .Build())
      .Then([this, self = shared_from_this()](auto& result) mutable {
        if (LogResultError(result, "Start")) {
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }

        // TODO(https://fxbug.dev/42181009): call reporter
        // reporter_->StartSession(result->value()->reference_time());
      });
}

// TODO(https://fxbug.dev/42181009): at most one start or stop command can be pending; if one is
// already pending, cancel it before sending the new stop command
void AudioCapturerServer::Stop() {
  fidl::Arena<> arena;
  (*graph_client_)
      ->Stop(fuchsia_audio_mixer::wire::GraphStopRequest::Builder(arena)
                 .node_id(*consumer_node_)
                 .when(fuchsia_media2::wire::RealOrStreamTime::WithAsap({}))
                 .Build())
      .Then([this, self = shared_from_this()](auto& result) mutable {
        if (LogResultError(result, "Stop")) {
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }

        // TODO(https://fxbug.dev/42181009): call reporter
        // reporter_->StopSession(result->value()->reference_time());
      });
}

void AudioCapturerServer::SendPacket(
    fuchsia_media::wire::StreamPacket packet, zx::duration overflow,
    std::optional<CaptureAtCompleter::Async> capture_at_completer) {
  if (state_ == State::kShutdown) {
    return;
  }

  // TODO(https://fxbug.dev/42181009): call reporter
  // reporter_->SendPacket(packet);
  // if (overflow) {
  //   reporter_->Overflow(overflow);
  // }

  if (capture_at_completer) {
    capture_at_completer->Reply(packet);
    return;
  }

  auto status = fidl::WireSendEvent(binding())->OnPacketProduced(packet);
  if (!status.ok()) {
    FX_LOGS(ERROR) << "OnPacketProduced failed with status " << status;
    Shutdown(ZX_ERR_INTERNAL);
    return;
  }
}

void AudioCapturerServer::OnShutdown(fidl::UnbindInfo info) {
  state_ = State::kShutdown;
  queued_tasks_.clear();

  // Delete all graph objects. Since we're already shutting down, don't call Shutdown on error.
  // Just check if the calls return an application-level error, which should never happen.
  fidl::Arena<> arena;
  if (consumer_node_) {
    (*graph_client_)
        ->DeleteNode(fuchsia_audio_mixer::wire::GraphDeleteNodeRequest::Builder(arena)
                         .id(*consumer_node_)
                         .Build())
        .Then([](auto& result) { LogResultError(result, "DeleteNode(consumer)"); });
  }
  if (stream_gain_control_) {
    (*graph_client_)
        ->DeleteGainControl(fuchsia_audio_mixer::wire::GraphDeleteGainControlRequest::Builder(arena)
                                .id(*stream_gain_control_)
                                .Build())
        .Then([](auto& result) { LogResultError(result, "DeleteGainControl(stream_gain)"); });
  }

  // Disconnect all clients and servers.
  graph_client_ = nullptr;
  stream_gain_control_client_ = std::nullopt;

  // TODO(https://fxbug.dev/42181009): send OnWillClose event from the StreamSinkServer before
  // shutting down
  if (stream_sink_server_) {
    stream_sink_server_->Shutdown();
    stream_sink_server_ = nullptr;
  }

  // Notify that we are shutting down.
  if (on_shutdown_) {
    on_shutdown_(shared_from_this());
  }

  BaseFidlServer::OnShutdown(info);
}

void AudioCapturerServer::MaybeConfigure() {
  if (state_ == State::kShutdown) {
    return;
  }

  // We can configure once we have a format and a payload buffer.
  if (!format_ || !payload_buffer_) {
    return;
  }

  state_ = State::kConfigured;

  // Map the payload buffer which is read by the client.
  {
    auto result = MemoryMappedBuffer::CreateWithFullSize(*payload_buffer_, true);
    if (!result.is_ok()) {
      FX_LOGS(ERROR) << "Failed to map payload buffer: " << result.error();
      Shutdown(ZX_ERR_INTERNAL);
      return;
    }
    payload_buffer_dest_ = result.take_value();
  }

  // Create and map the payload buffer which is written by the ConsumerNode.
  zx::vmo payload_vmo_source;
  {
    auto status = zx::vmo::create(payload_buffer_dest_->size(), 0, &payload_vmo_source);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "zx::vmo::create failed";
      Shutdown(ZX_ERR_INTERNAL);
      return;
    }
    auto result = MemoryMappedBuffer::CreateWithFullSize(payload_vmo_source, true);
    if (!result.is_ok()) {
      FX_LOGS(ERROR) << "Failed to map payload buffer: " << result.error();
      Shutdown(ZX_ERR_INTERNAL);
      return;
    }
    payload_buffer_source_ = result.take_value();
  }

  auto stream_sink_endpoints = fidl::CreateEndpoints<fuchsia_audio::StreamSink>();
  if (!stream_sink_endpoints.is_ok()) {
    FX_PLOGS(ERROR, stream_sink_endpoints.status_value()) << "fidl::CreateEndpoints failed";
    Shutdown(ZX_ERR_INTERNAL);
    return;
  }

  auto stream_gain_control_endpoints = fidl::CreateEndpoints<fuchsia_audio::GainControl>();
  if (!stream_gain_control_endpoints.is_ok()) {
    FX_PLOGS(ERROR, stream_gain_control_endpoints.status_value()) << "fidl::CreateEndpoints failed";
    Shutdown(ZX_ERR_INTERNAL);
    return;
  }

  // This should be sufficient for all clients: we don't expect that any client will need a capture
  // latency smaller than 10ms.
  const auto frames_per_packet = static_cast<uint32_t>(format_->integer_frames_per(zx::msec(10)));

  stream_sink_server_ =
      StreamSinkServer::Create(thread_ptr(), std::move(stream_sink_endpoints->server),
                               {
                                   .format = *format_,
                                   .payload_buffer = payload_buffer_source_,
                               });
  AddChildServer(stream_sink_server_);

  stream_gain_control_client_ = fidl::WireSharedClient(
      std::move(stream_gain_control_endpoints->client), thread().dispatcher());

  fidl::Arena<> arena;

  // Create a ConsumerNode which writes to `payload_buffer_source`.
  (*graph_client_)
      ->CreateConsumer(fuchsia_audio_mixer::wire::GraphCreateConsumerRequest::Builder(arena)
                           .name("Capturer" + std::to_string(capturer_id_))
                           .direction(fuchsia_audio_mixer::PipelineDirection::kInput)
                           .data_sink(fuchsia_audio_mixer::wire::ConsumerDataSink::WithStreamSink(
                               arena, fuchsia_audio_mixer::wire::StreamSinkConsumer::Builder(arena)
                                          .client_end(std::move(stream_sink_endpoints->client))
                                          .format(format_->ToWireFidl(arena))
                                          .reference_clock(ReferenceClockToFidl(arena))
                                          .payload_buffer(DupHandle(payload_vmo_source))
                                          .media_ticks_per_second(fuchsia_math::wire::RatioU64{
                                              .numerator = 1'000'000'000,  // 1ns per tick
                                              .denominator = 1,
                                          })
                                          .frames_per_packet(frames_per_packet)
                                          .Build()))
                           .Build())
      .Then([this, self = shared_from_this()](auto& result) {
        if (LogResultError(result, "CreateConsumer")) {
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }
        if (!result->value()->has_id()) {
          FX_LOGS(ERROR) << "CreateConsumer bug: response missing `id`";
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }
        consumer_node_ = result->value()->id();
        MaybeSetFullyCreated();
      });

  // Create GainControl for controlling stream gain.
  (*graph_client_)
      ->CreateGainControl(fuchsia_audio_mixer::wire::GraphCreateGainControlRequest::Builder(arena)
                              .name("StreamGainControlForCapturer" + std::to_string(capturer_id_))
                              .control(std::move(stream_gain_control_endpoints->server))
                              .reference_clock(ReferenceClockToFidl(arena))
                              .Build())
      .Then([this, self = shared_from_this()](auto& result) {
        if (LogResultError(result, "CreateGainControl")) {
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }
        if (!result->value()->has_id()) {
          FX_LOGS(ERROR) << "CreateGainControl bug: response missing `id`";
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }
        stream_gain_control_ = result->value()->id();
        MaybeSetFullyCreated();
      });
}

void AudioCapturerServer::MaybeSetFullyCreated() {
  if (state_ == State::kShutdown) {
    return;
  }
  if (!consumer_node_ || !stream_gain_control_) {
    return;
  }

  state_ = State::kFullyCreated;
  if (on_fully_created_) {
    on_fully_created_(shared_from_this());
  }

  // TODO(https://fxbug.dev/42181009): after implementing RouteGraph, this is where we should add
  // this capturer to the RouteGroup.

  // Flush all queued tasks.
  for (auto& fn : queued_tasks_) {
    fn();
  }
  queued_tasks_.clear();
}

void AudioCapturerServer::RunWhenReady(const char* debug_string, fit::closure fn) {
  switch (state_) {
    case State::kShutdown:
      break;
    case State::kWaitingForConfig:
      FX_LOGS(WARNING) << debug_string << ": cannot call until configured";
      Shutdown(ZX_ERR_BAD_STATE);
      break;
    case State::kConfigured:
      queued_tasks_.push_back(std::move(fn));
      break;
    case State::kFullyCreated:
    case State::kCapturingSync:
    case State::kCapturingAsync:
      fn();
      break;
  }
}

fuchsia_audio_mixer::wire::ReferenceClock AudioCapturerServer::ReferenceClockToFidl(
    fidl::AnyArena& arena) {
  return fuchsia_audio_mixer::wire::ReferenceClock::Builder(arena)
      .name("ClockForCapturer" + std::to_string(capturer_id_))
      .handle(DupHandle(reference_clock_))
      .domain(fuchsia_hardware_audio::kClockDomainExternal)
      .Build();
}

}  // namespace media_audio
