// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/audio_renderer_server.h"

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
using ::fuchsia_audio::wire::RampedGain;
using ::fuchsia_audio::wire::RampFunction;
using ::fuchsia_audio::wire::RampFunctionLinearSlope;
using ::media::audio::RenderUsage;

int64_t num_renderers = 0;

constexpr zx::duration kPaddingForPlayPause = zx::msec(20);
constexpr zx::duration kRampUpOnPlayDuration = zx::msec(5);
constexpr zx::duration kRampDownOnPauseDuration = zx::msec(5);
constexpr float kMinimumGainForPlayPauseRamps = -120.0f;

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
std::shared_ptr<AudioRendererServer> AudioRendererServer::Create(
    std::shared_ptr<const FidlThread> fidl_thread,
    fidl::ServerEnd<fuchsia_media::AudioRenderer> server_end, Args args) {
  return BaseFidlServer::Create(fidl_thread, std::move(server_end), fidl_thread, std::move(args));
}

AudioRendererServer::AudioRendererServer(std::shared_ptr<const FidlThread> fidl_thread, Args args)
    : renderer_id_(++num_renderers),
      default_reference_clock_(std::move(args.default_reference_clock)),
      ramp_on_play_pause_(args.ramp_on_play_pause),
      on_fully_created_(std::move(args.on_fully_created)),
      on_shutdown_(std::move(args.on_shutdown)),
      graph_client_(std::move(args.graph_client)),
      usage_(args.usage),
      format_(args.format),
      reference_clock_(DupHandle(default_reference_clock_)) {
  FX_CHECK(default_reference_clock_);
  FX_CHECK(graph_client_);

  if (usage_ == RenderUsage::ULTRASOUND) {
    FX_CHECK(format_);
  }

  // Create endpoints for the DelayWatcher. The server end won't be connected until the renderer's
  // ProducerNode is created.
  auto delay_watcher_endpoints = fidl::CreateEndpoints<fuchsia_audio::DelayWatcher>();
  if (!delay_watcher_endpoints.is_ok()) {
    FX_PLOGS(FATAL, delay_watcher_endpoints.status_value()) << "fidl::CreateEndpoints failed";
  }

  delay_watcher_client_ = DelayWatcherClient::Create({
      .name = "DelayWatcherForRenderer" + std::to_string(renderer_id_),
      .client_end = std::move(delay_watcher_endpoints->client),
      .thread = fidl_thread,
  });
  delay_watcher_server_end_ = std::move(delay_watcher_endpoints->server);
}

media::audio::RenderUsage AudioRendererServer::usage() const {
  FX_CHECK(IsConfigured());
  return usage_;
}

const Format& AudioRendererServer::format() const {
  FX_CHECK(IsConfigured());
  return *format_;
}

NodeId AudioRendererServer::producer_node() const {
  FX_CHECK(IsConfigured());
  return *producer_node_;
}

GainControlId AudioRendererServer::stream_gain_control() const {
  FX_CHECK(IsConfigured());
  return *stream_gain_control_;
}

std::optional<GainControlId> AudioRendererServer::play_pause_ramp_gain_control() const {
  FX_CHECK(IsConfigured());
  return play_pause_ramp_gain_control_;
}

void AudioRendererServer::SetPtsUnits(SetPtsUnitsRequestView request,
                                      SetPtsUnitsCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioRendererServer::SetPtsContinuityThreshold");

  const auto tick_per_second_numerator = request->tick_per_second_numerator;
  const auto tick_per_second_denominator = request->tick_per_second_denominator;

  FX_LOGS(DEBUG) << "PTS ticks per sec: " << tick_per_second_numerator << " / "
                 << tick_per_second_denominator;

  if (IsConfigured()) {
    FX_LOGS(WARNING) << "PTS ticks per second cannot be set once configured.";
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }
  if (!tick_per_second_numerator || !tick_per_second_denominator) {
    FX_LOGS(WARNING) << "Bad PTS ticks per second (" << tick_per_second_numerator << "/"
                     << tick_per_second_denominator
                     << "): both numerator and denominator must be non-zero";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  const auto pts_ticks_per_sec =
      TimelineRate(tick_per_second_numerator, tick_per_second_denominator);

  // Sanity checks to ensure that Scale() operations cannot overflow.
  // Must have at most 1 tick per nanosecond. Ticks should not have higher resolution than clocks.
  if (auto t = pts_ticks_per_sec.Scale(1, TimelineRate::RoundingMode::Ceiling);
      t > 1'000'000'000 || t == TimelineRate::kOverflow) {
    FX_LOGS(WARNING) << "PTS ticks per second too high (" << tick_per_second_numerator << "/"
                     << tick_per_second_denominator << ")";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }
  // Must have at least 1 tick per minute. This limit is somewhat arbitrary. We need *some* limit
  // here and we expect this is way more headroom than will be needed in practice.
  if (auto t = pts_ticks_per_sec.Scale(60); t == 0) {
    FX_LOGS(WARNING) << "PTS ticks per second too low (" << tick_per_second_numerator << "/"
                     << tick_per_second_denominator << ")";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  // TODO(https://fxbug.dev/42181009): call reporter
  // reporter_->SetPtsUnits(tick_per_second_numerator, tick_per_second_denominator);

  media_ticks_per_second_ = std::move(pts_ticks_per_sec);
}

void AudioRendererServer::SetPtsContinuityThreshold(
    SetPtsContinuityThresholdRequestView request,
    SetPtsContinuityThresholdCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioRendererServer::SetPtsContinuityThreshold");

  const auto threshold_seconds = request->threshold_seconds;
  FX_LOGS(DEBUG) << "PTS continuity threshold: " << threshold_seconds << " sec";

  if (IsConfigured()) {
    FX_LOGS(WARNING) << "PTS continuity threshold cannot be set once configured.";
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }
  if (!isnormal(threshold_seconds) && threshold_seconds != 0.0) {
    FX_LOGS(WARNING) << "PTS continuity threshold (" << threshold_seconds
                     << ") must be normal or 0";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }
  if (threshold_seconds < 0.0) {
    FX_LOGS(WARNING) << "PTS continuity threshold (" << threshold_seconds << ") cannot be negative";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  // TODO(https://fxbug.dev/42181009): call reporter
  // reporter_->SetPtsContinuityThreshold(threshold_seconds);

  media_ticks_continuity_threshold_seconds_ = threshold_seconds;
}

void AudioRendererServer::SetReferenceClock(SetReferenceClockRequestView request,
                                            SetReferenceClockCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioRendererServer::SetReferenceClock");

  if (usage_ == RenderUsage::ULTRASOUND) {
    FX_LOGS(WARNING) << "Unsupported method SetReferenceClock on ultrasound renderer";
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

void AudioRendererServer::SetUsage(SetUsageRequestView request,
                                   SetUsageCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioRendererServer::SetUsage");

  if (usage_ == RenderUsage::ULTRASOUND) {
    FX_LOGS(WARNING) << "Unsupported method SetUsage on ultrasound renderer";
    Shutdown(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  if (IsConfigured()) {
    FX_LOGS(WARNING) << "Usage cannot be set once configured.";
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }

  usage_ = media::audio::RenderUsageFromFidlRenderUsage(
      static_cast<fuchsia::media::AudioRenderUsage2>(request->usage));
}

void AudioRendererServer::SetPcmStreamType(SetPcmStreamTypeRequestView request,
                                           SetPcmStreamTypeCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioRendererServer::SetPcmStreamType");

  if (usage_ == RenderUsage::ULTRASOUND) {
    FX_LOGS(WARNING) << "Unsupported method SetPcmStreamType on ultrasound renderer";
    Shutdown(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  if (IsConfigured()) {
    FX_LOGS(WARNING) << "Format cannot be set once configured.";
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }

  auto format_result = Format::CreateLegacy(request->type);
  if (format_result.is_error()) {
    FX_LOGS(WARNING) << "AudioRenderer: PcmStreamType is invalid: " << format_result.error();
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  format_ = std::move(format_result.value());

  // TODO(https://fxbug.dev/42181009): call reporter
  // reporter_->SetFormat(*format_);

  MaybeConfigure();
}

void AudioRendererServer::SendPacket(SendPacketRequestView request,
                                     SendPacketCompleter::Sync& completer) {
  RunWhenReady("SendPacket", [this, packet = fidl::ToNatural(request->packet),
                              completer = completer.ToAsync()]() mutable {
    SendPacketInternal(packet, std::move(completer));
  });
}

void AudioRendererServer::SendPacketNoReply(SendPacketNoReplyRequestView request,
                                            SendPacketNoReplyCompleter::Sync& completer) {
  RunWhenReady("SendPacketNoReply", [this, packet = fidl::ToNatural(request->packet)]() {
    SendPacketInternal(packet, std::nullopt);
  });
}

void AudioRendererServer::EndOfStream(EndOfStreamCompleter::Sync& completer) {
  RunWhenReady("EndOfStream", [this]() { EndOfStreamInternal(); });
}

void AudioRendererServer::DiscardAllPackets(DiscardAllPacketsCompleter::Sync& completer) {
  RunWhenReady("DiscardAllPackets", [this, completer = completer.ToAsync()]() mutable {
    DiscardAllPacketsInternal(std::move(completer));
  });
}

void AudioRendererServer::DiscardAllPacketsNoReply(
    DiscardAllPacketsNoReplyCompleter::Sync& completer) {
  RunWhenReady("DiscardAllPacketsNoReply", [this]() { DiscardAllPacketsInternal(std::nullopt); });
}

void AudioRendererServer::Play(PlayRequestView request, PlayCompleter::Sync& completer) {
  RunWhenReady("Play",
               [this, reference_time = request->reference_time, media_time = request->media_time,
                completer = completer.ToAsync()]() mutable {
                 PlayInternal(zx::time(reference_time), media_time, std::move(completer));
               });
}

void AudioRendererServer::PlayNoReply(PlayNoReplyRequestView request,
                                      PlayNoReplyCompleter::Sync& completer) {
  RunWhenReady("PlayNoReply", [this, reference_time = request->reference_time,
                               media_time = request->media_time]() {
    PlayInternal(zx::time(reference_time), media_time, std::nullopt);
  });
}

void AudioRendererServer::Pause(PauseCompleter::Sync& completer) {
  RunWhenReady("Pause", [this, completer = completer.ToAsync()]() mutable {
    PauseInternal(std::move(completer));
  });
}

void AudioRendererServer::PauseNoReply(PauseNoReplyCompleter::Sync& completer) {
  RunWhenReady("PauseNoReply", [this]() { PauseInternal(std::nullopt); });
}

void AudioRendererServer::AddPayloadBuffer(AddPayloadBufferRequestView request,
                                           AddPayloadBufferCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioRendererServer::AddPayloadBuffer");

  // This restriction is also present in audio_core v1.
  if (IsConfigured()) {
    FX_LOGS(WARNING) << "AddPayloadBuffer cannot be called once configured.";
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }

  // TODO(https://fxbug.dev/42181009): support IDs other than 0.
  // This is a current limitation of the mixer service APIs.
  if (request->id != 0) {
    FX_LOGS(WARNING) << "AddPayloadBuffer id must be 0, is " << request->id;
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (payload_buffers_.count(request->id) > 0) {
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
  // reporter_->AddPayloadBuffer(id, vmo_size);

  payload_buffers_[request->id] = std::move(request->payload_buffer);
  MaybeConfigure();
}

void AudioRendererServer::RemovePayloadBuffer(RemovePayloadBufferRequestView request,
                                              RemovePayloadBufferCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioRendererServer::RemovePayloadBuffer");

  // This restriction is also present in audio_core v1.
  if (IsConfigured()) {
    FX_LOGS(WARNING) << "RemovePayloadBuffer cannot be called once configured.";
    Shutdown(ZX_ERR_BAD_STATE);
    return;
  }

  if (payload_buffers_.erase(request->id) != 1) {
    FX_LOGS(WARNING) << "RemovePayloadBuffer id " << request->id << " does not exist";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  // TODO(https://fxbug.dev/42181009): call reporter
  // reporter_->RemovePayloadBuffer(id);
}

// TODO(https://fxbug.dev/42181009): implement: need to create a fuchsia.media.audio.GainControl
// server that forwards to stream_gain_control_client_
void AudioRendererServer::BindGainControl(BindGainControlRequestView request,
                                          BindGainControlCompleter::Sync& completer) {
  FX_LOGS(ERROR) << "BindGainControl not implemented";
  Shutdown(ZX_ERR_NOT_SUPPORTED);
}

void AudioRendererServer::GetReferenceClock(GetReferenceClockCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioRendererServer::GetReferenceCLock");
  completer.Reply(DupHandle(reference_clock_));
}

void AudioRendererServer::GetMinLeadTime(GetMinLeadTimeCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioRendererServer::GetMinLeadTime");

  auto delay = delay_watcher_client_ ? delay_watcher_client_->delay() : std::nullopt;
  completer.Reply(delay ? delay->to_nsecs() : 0);
}

void AudioRendererServer::EnableMinLeadTimeEvents(
    EnableMinLeadTimeEventsRequestView request, EnableMinLeadTimeEventsCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioRendererServer::EnableMinLeadTimeEvents");
  enable_min_lead_time_events_ = request->enabled;
}

void AudioRendererServer::SendPacketInternal(fuchsia_media::StreamPacket packet,
                                             std::optional<SendPacketCompleter::Async> completer) {
  TRACE_DURATION("audio", "AudioRendererServer::SendPacketInternal");
  FX_CHECK(state_ == State::kFullyCreated);
  FX_CHECK(stream_sink_client_);

  // TODO(https://fxbug.dev/42181009): support IDs other than 0.
  if (packet.payload_buffer_id() != 0) {
    FX_LOGS(WARNING) << "SendPacket packet.payload_buffer_id must be 0, is "
                     << packet.payload_buffer_id();
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  // TODO(https://fxbug.dev/42181009): call reporter
  // reporter_->SendPacket(packet);

  fidl::Arena<> arena;

  fuchsia_audio::wire::Timestamp timestamp;
  if (packet.pts() == fuchsia_media::kNoTimestamp) {
    if ((packet.flags() & fuchsia_media::kStreamPacketFlagDiscontinuity) != 0) {
      timestamp = fuchsia_audio::wire::Timestamp::WithUnspecifiedBestEffort({});
    } else {
      timestamp = fuchsia_audio::wire::Timestamp::WithUnspecifiedContinuous({});
    }
  } else {
    timestamp = fuchsia_audio::wire::Timestamp::WithSpecified(arena, packet.pts());
  }

  zx::eventpair local_fence;
  zx::eventpair remote_fence;
  if (auto status = zx::eventpair::create(0, &local_fence, &remote_fence); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "zx::eventpair::create failed";
    Shutdown(ZX_ERR_INTERNAL);
    return;
  }

  auto status = (*stream_sink_client_)
                    ->PutPacket(fuchsia_audio::wire::StreamSinkPutPacketRequest::Builder(arena)
                                    .packet(fuchsia_audio::wire::Packet::Builder(arena)
                                                .payload(fuchsia_media2::wire::PayloadRange{
                                                    .buffer_id = 0,
                                                    .offset = packet.payload_offset(),
                                                    .size = packet.payload_size(),
                                                })
                                                .timestamp(timestamp)
                                                .Build())
                                    .release_fence(std::move(remote_fence))
                                    .Build());
  if (!status.ok()) {
    FX_LOGS(WARNING) << "Error sending StreamSink.PutPacket: " << status;
    Shutdown(ZX_ERR_INTERNAL);
    return;
  }

  // Complete the method call when the fence is closed.
  if (completer) {
    auto waiter = std::make_unique<async::WaitOnce>(local_fence.get(), ZX_EVENTPAIR_PEER_CLOSED);
    auto waiter_ptr = waiter.get();
    waiter_ptr->Begin(
        thread().dispatcher(),
        [local_fence = std::move(local_fence), waiter = std::move(waiter),
         completer = std::move(*completer)](auto* dispatcher, auto* wait, auto status,
                                            auto* signal) mutable { completer.Reply(); });
  }
}

void AudioRendererServer::EndOfStreamInternal() {
  TRACE_DURATION("audio", "AudioRendererServer::EndOfStreamInternal");
  FX_CHECK(stream_sink_client_);

  auto status = (*stream_sink_client_)->End();
  if (!status.ok()) {
    FX_LOGS(WARNING) << "Error sending StreamSink.End: " << status;
  }
}

// TODO(https://fxbug.dev/42181009): implement: need another mixer service API
void AudioRendererServer::DiscardAllPacketsInternal(
    std::optional<DiscardAllPacketsCompleter::Async> completer) {
  TRACE_DURATION("audio", "AudioRendererServer::DiscardAllPacketsInternal");

  FX_LOGS(ERROR) << "DiscardAllPackets not implemented";
  Shutdown(ZX_ERR_NOT_SUPPORTED);
}

void AudioRendererServer::PlayInternal(zx::time reference_time, int64_t media_time,
                                       std::optional<PlayCompleter::Async> completer) {
  TRACE_DURATION("audio", "AudioRendererServer::PlayInternal");
  FX_CHECK(producer_node_);

  // If the client asks to play "ASAP", choose an explicit timestamp so we can synchronize the
  // "Start" command with the gain ramp.
  if (reference_time.get() == fuchsia_media::kNoTimestamp) {
    zx_time_t now;
    if (auto status = reference_clock_.read(&now); status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Unexpected failure to read reference_clock_";
      Shutdown(ZX_ERR_INTERNAL);
      return;
    }
    reference_time = zx::time(now) + min_lead_time_ + kPaddingForPlayPause;
  }

  // If this is unspecified, the user wants to "unpause" after the last pause event. If there was no
  // pause event, default to zero.
  if (media_time == fuchsia_media::kNoTimestamp) {
    media_time = last_pause_media_time_ ? *last_pause_media_time_ : 0;
  }

  fidl::Arena<> arena;
  (*graph_client_)
      ->Start(
          fuchsia_audio_mixer::wire::GraphStartRequest::Builder(arena)
              .node_id(*producer_node_)
              .when(fuchsia_media2::wire::RealTime::WithReferenceTime(arena, reference_time.get()))
              .stream_time(fuchsia_media2::wire::StreamTime::WithPacketTimestamp(arena, media_time))
              .Build())
      .Then([this, self = shared_from_this(),
             completer = std::move(completer)](auto& result) mutable {
        if (LogResultError(result, "Start")) {
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }
        if (!result->value()->has_reference_time() || !result->value()->has_packet_timestamp()) {
          FX_LOGS(ERROR) << "Start bug: response missing fields";
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }
        if (completer) {
          completer->Reply(result->value()->reference_time(), result->value()->packet_timestamp());
        }

        // TODO(https://fxbug.dev/42181009): call reporter
        // reporter_->ReportStartIfStopped();
      });

  // Ramp up after play, if requested.
  if (ramp_on_play_pause_) {
    FX_CHECK(play_pause_ramp_gain_control_client_);

    // First set to 0%.
    (*play_pause_ramp_gain_control_client_)
        ->SetGain(GainControlSetGainRequest::Builder(arena)
                      .how(GainUpdateMethod::WithGainDb(kMinimumGainForPlayPauseRamps))
                      .when(fuchsia_media2::wire::RealTime::WithReferenceTime(arena,
                                                                              reference_time.get()))
                      .Build())
        .Then([this, self = shared_from_this()](auto& result) {
          if (LogResultError(result, "SetGain (play ramp reset)")) {
            Shutdown(ZX_ERR_INTERNAL);
            return;
          }
        });

    // Then immediately start ramping up.
    (*play_pause_ramp_gain_control_client_)
        ->SetGain(GainControlSetGainRequest::Builder(arena)
                      .how(GainUpdateMethod::WithRamped(
                          arena, RampedGain::Builder(arena)
                                     .target_gain_db(kUnityGainDb)
                                     .duration(kRampUpOnPlayDuration.to_nsecs())
                                     .function(RampFunction::WithLinearSlope(
                                         arena, RampFunctionLinearSlope::Builder(arena).Build()))
                                     .Build()))
                      .when(fuchsia_media2::wire::RealTime::WithReferenceTime(arena,
                                                                              reference_time.get()))
                      .Build())
        .Then([this, self = shared_from_this()](auto& result) {
          if (LogResultError(result, "SetGain (play ramp start)")) {
            Shutdown(ZX_ERR_INTERNAL);
            return;
          }
        });
  }
}

void AudioRendererServer::PauseInternal(std::optional<PauseCompleter::Async> completer) {
  TRACE_DURATION("audio", "AudioRendererServer::PauseInternal");
  FX_CHECK(producer_node_);

  fidl::Arena<> arena;

  // Pause() should happen ASAP. Choose an explicit timestamp so we can synchronize the "Stop"
  // command with the gain ramp.
  zx_time_t now;
  if (auto status = reference_clock_.read(&now); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Unexpected failure to read reference_clock_";
    Shutdown(ZX_ERR_INTERNAL);
    return;
  }

  auto reference_time = zx::time(now) + min_lead_time_ + kPaddingForPlayPause;

  // Ramp down first, if requested.
  if (ramp_on_play_pause_) {
    FX_CHECK(play_pause_ramp_gain_control_client_);

    // First set to 100%.
    (*play_pause_ramp_gain_control_client_)
        ->SetGain(GainControlSetGainRequest::Builder(arena)
                      .how(GainUpdateMethod::WithGainDb(kUnityGainDb))
                      .when(fuchsia_media2::wire::RealTime::WithReferenceTime(arena,
                                                                              reference_time.get()))
                      .Build())
        .Then([this, self = shared_from_this()](auto& result) {
          if (LogResultError(result, "SetGain (pause ramp reset)")) {
            Shutdown(ZX_ERR_INTERNAL);
            return;
          }
        });

    // Then immediately start ramping down.
    (*play_pause_ramp_gain_control_client_)
        ->SetGain(GainControlSetGainRequest::Builder(arena)
                      .how(GainUpdateMethod::WithRamped(
                          arena, RampedGain::Builder(arena)
                                     .target_gain_db(kMinimumGainForPlayPauseRamps)
                                     .duration(kRampDownOnPauseDuration.to_nsecs())
                                     .function(RampFunction::WithLinearSlope(
                                         arena, RampFunctionLinearSlope::Builder(arena).Build()))
                                     .Build()))
                      .when(fuchsia_media2::wire::RealTime::WithReferenceTime(arena,
                                                                              reference_time.get()))
                      .Build())
        .Then([this, self = shared_from_this()](auto& result) {
          if (LogResultError(result, "SetGain (pause ramp start)")) {
            Shutdown(ZX_ERR_INTERNAL);
            return;
          }
        });

    reference_time += kRampDownOnPauseDuration;
  }

  // Then pause.
  (*graph_client_)
      ->Stop(fuchsia_audio_mixer::wire::GraphStopRequest::Builder(arena)
                 .node_id(*producer_node_)
                 .when(fuchsia_media2::wire::RealOrStreamTime::WithReferenceTime(
                     arena, reference_time.get()))
                 .Build())
      .Then([this, self = shared_from_this(),
             completer = std::move(completer)](auto& result) mutable {
        if (LogResultError(result, "Stop")) {
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }
        if (!result->value()->has_reference_time() || !result->value()->has_packet_timestamp()) {
          FX_LOGS(ERROR) << "Stop bug: response missing fields";
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }
        if (completer) {
          completer->Reply(result->value()->reference_time(), result->value()->packet_timestamp());
        }

        // TODO(https://fxbug.dev/42181009): call reporter
        // reporter_->ReportStopIfStarted();
      });
}

void AudioRendererServer::OnShutdown(fidl::UnbindInfo info) {
  state_ = State::kShutdown;
  queued_tasks_.clear();

  // Delete all graph objects. Since we're already shutting down, don't callShutdown on error.
  // Just check if the calls return an application-level error, which should never happen.
  fidl::Arena<> arena;
  if (producer_node_) {
    (*graph_client_)
        ->DeleteNode(fuchsia_audio_mixer::wire::GraphDeleteNodeRequest::Builder(arena)
                         .id(*producer_node_)
                         .Build())
        .Then([](auto& result) { LogResultError(result, "DeleteNode(producer)"); });
  }
  if (stream_gain_control_) {
    (*graph_client_)
        ->DeleteGainControl(fuchsia_audio_mixer::wire::GraphDeleteGainControlRequest::Builder(arena)
                                .id(*stream_gain_control_)
                                .Build())
        .Then([](auto& result) { LogResultError(result, "DeleteGainControl(stream_gain)"); });
  }
  if (play_pause_ramp_gain_control_) {
    (*graph_client_)
        ->DeleteGainControl(fuchsia_audio_mixer::wire::GraphDeleteGainControlRequest::Builder(arena)
                                .id(*play_pause_ramp_gain_control_)
                                .Build())
        .Then([](auto& result) {
          LogResultError(result, "DeleteGainControl(play_pause_ramp_gain)");
        });
  }

  // Disconnect all clients.
  graph_client_ = nullptr;
  delay_watcher_client_ = nullptr;
  stream_sink_client_ = std::nullopt;
  stream_gain_control_client_ = std::nullopt;
  play_pause_ramp_gain_control_client_ = std::nullopt;

  // Notify that we are shutting down.
  if (on_shutdown_) {
    on_shutdown_(shared_from_this());
  }

  BaseFidlServer::OnShutdown(info);
}

void AudioRendererServer::OnMinLeadTimeUpdated(std::optional<zx::duration> min_lead_time) {
  min_lead_time_ = min_lead_time ? *min_lead_time : zx::nsec(0);
  if (!enable_min_lead_time_events_) {
    return;
  }

  auto status = fidl::WireSendEvent(binding())->OnMinLeadTimeChanged(min_lead_time_.to_nsecs());
  if (!status.ok()) {
    FX_LOGS(ERROR) << "OnMinLeadTimeChanged failed with status " << status;
    Shutdown(ZX_ERR_INTERNAL);
  }
}

void AudioRendererServer::MaybeConfigure() {
  if (state_ == State::kShutdown) {
    return;
  }

  // We can configure once we have a format and a payload buffer.
  if (!format_ || !payload_buffers_.count(0)) {
    return;
  }

  // Defaults to 1 tick per nanosecond.
  if (!media_ticks_per_second_) {
    media_ticks_per_second_ = TimelineRate(1'000'000'000, 1);
  }

  // TODO(https://fxbug.dev/42181009): add media_ticks_continuity_threshold_seconds_ to mixer
  // service

  state_ = State::kConfigured;

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

  stream_sink_client_ =
      fidl::WireSharedClient(std::move(stream_sink_endpoints->client), thread().dispatcher());
  stream_gain_control_client_ = fidl::WireSharedClient(
      std::move(stream_gain_control_endpoints->client), thread().dispatcher());

  fidl::Arena<> arena;

  // Create ProducerNode.
  (*graph_client_)
      ->CreateProducer(
          fuchsia_audio_mixer::wire::GraphCreateProducerRequest::Builder(arena)
              .name("Renderer" + std::to_string(renderer_id_))
              .direction(fuchsia_audio_mixer::PipelineDirection::kOutput)
              .data_source(fuchsia_audio_mixer::wire::ProducerDataSource::WithStreamSink(
                  arena, fuchsia_audio_mixer::wire::StreamSinkProducer::Builder(arena)
                             .server_end(std::move(stream_sink_endpoints->server))
                             .format(format_->ToWireFidl(arena))
                             .reference_clock(ReferenceClockToFidl(arena))
                             .payload_buffer(DupHandle(payload_buffers_[0]))
                             .media_ticks_per_second(fuchsia_math::wire::RatioU64{
                                 .numerator = media_ticks_per_second_->subject_delta(),
                                 .denominator = media_ticks_per_second_->reference_delta(),
                             })
                             .Build()))
              .Build())
      .Then([this, self = shared_from_this()](auto& result) {
        if (LogResultError(result, "CreateProducer")) {
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }
        if (!result->value()->has_id()) {
          FX_LOGS(ERROR) << "CreateProducer bug: response missing `id`";
          Shutdown(ZX_ERR_INTERNAL);
          return;
        }
        producer_node_ = result->value()->id();
        MaybeSetFullyCreated();

        // Watch for lead time events.
        fidl::Arena<> arena;
        (*graph_client_)
            ->BindProducerLeadTimeWatcher(
                fuchsia_audio_mixer::wire::GraphBindProducerLeadTimeWatcherRequest::Builder(arena)
                    .id(*producer_node_)
                    .server_end(std::move(delay_watcher_server_end_))
                    .Build())
            .Then([this, self = shared_from_this()](auto& result) {
              if (LogResultError(result, "BindProducerLeadTimeWatcher")) {
                Shutdown(ZX_ERR_INTERNAL);
                return;
              }
            });
      });

  // Create GainControl for controlling stream gain.
  (*graph_client_)
      ->CreateGainControl(fuchsia_audio_mixer::wire::GraphCreateGainControlRequest::Builder(arena)
                              .name("StreamGainControlForRenderer" + std::to_string(renderer_id_))
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

  // Create GainControl for ramping on play/pause.
  if (ramp_on_play_pause_) {
    auto play_pause_ramp_gain_control_endpoints =
        fidl::CreateEndpoints<fuchsia_audio::GainControl>();
    if (!play_pause_ramp_gain_control_endpoints.is_ok()) {
      FX_PLOGS(ERROR, play_pause_ramp_gain_control_endpoints.status_value())
          << "fidl::CreateEndpoints failed";
      Shutdown(ZX_ERR_INTERNAL);
      return;
    }

    play_pause_ramp_gain_control_client_ = fidl::WireSharedClient(
        std::move(play_pause_ramp_gain_control_endpoints->client), thread().dispatcher());

    (*graph_client_)
        ->CreateGainControl(fuchsia_audio_mixer::wire::GraphCreateGainControlRequest::Builder(arena)
                                .name("StreamGainControlForRenderer" + std::to_string(renderer_id_))
                                .control(std::move(play_pause_ramp_gain_control_endpoints->server))
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
          play_pause_ramp_gain_control_ = result->value()->id();
          MaybeSetFullyCreated();
        });
  }
}

void AudioRendererServer::MaybeSetFullyCreated() {
  if (state_ == State::kShutdown) {
    return;
  }
  if (!producer_node_ || !stream_gain_control_ ||
      (ramp_on_play_pause_ && !play_pause_ramp_gain_control_)) {
    return;
  }

  state_ = State::kFullyCreated;
  if (on_fully_created_) {
    on_fully_created_(shared_from_this());
  }

  // TODO(https://fxbug.dev/42181009): after implementing RouteGraph, this is where we should add
  // this renderer to the RouteGroup.

  // Flush all queued tasks.
  for (auto& fn : queued_tasks_) {
    fn();
  }
  queued_tasks_.clear();
}

void AudioRendererServer::RunWhenReady(const char* debug_string, fit::closure fn) {
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
      fn();
      break;
  }
}

fuchsia_audio_mixer::wire::ReferenceClock AudioRendererServer::ReferenceClockToFidl(
    fidl::AnyArena& arena) {
  return fuchsia_audio_mixer::wire::ReferenceClock::Builder(arena)
      .name("ClockForRenderer" + std::to_string(renderer_id_))
      .handle(DupHandle(reference_clock_))
      .domain(fuchsia_hardware_audio::kClockDomainExternal)
      .Build();
}

}  // namespace media_audio
