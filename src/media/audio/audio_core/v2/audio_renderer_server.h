// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_RENDERER_SERVER_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_RENDERER_SERVER_H_

#include <fidl/fuchsia.audio.mixer/cpp/wire.h>
#include <fidl/fuchsia.audio/cpp/wire.h>
#include <fidl/fuchsia.media/cpp/wire.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl/cpp/wire/unknown_interaction_handler.h>
#include <lib/fit/function.h>
#include <lib/zx/vmo.h>

#include <map>
#include <memory>
#include <optional>
#include <utility>

#include "src/media/audio/audio_core/shared/device_config.h"
#include "src/media/audio/audio_core/shared/stream_usage.h"
#include "src/media/audio/audio_core/v2/graph_types.h"
#include "src/media/audio/audio_core/v2/reference_clock.h"
#include "src/media/audio/lib/format2/format.h"
#include "src/media/audio/services/common/base_fidl_server.h"
#include "src/media/audio/services/common/delay_watcher_client.h"

namespace media_audio {

class AudioRendererServer
    : public BaseFidlServer<AudioRendererServer, fidl::WireServer, fuchsia_media::AudioRenderer>,
      public std::enable_shared_from_this<AudioRendererServer> {
 public:
  struct Args {
    // Connection to the mixer service.
    std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client;

    // Initial configuration. If `usage == ULTRASOUND`, then `format` must be set and cannot be
    // changed by the client. Otherwise, the `format` is optional here and can be set by the client.
    media::audio::RenderUsage usage;
    std::optional<Format> format;

    // Default clock to use if the client does not explicitly choose one.
    // Required.
    zx::clock default_reference_clock;

    // If true, ramp gain up on play and down on pause. Otherwise, no gain ramping on play/pause.
    bool ramp_on_play_pause;

    // Called when `IsFullyCreated`.
    fit::callback<void(std::shared_ptr<AudioRendererServer>)> on_fully_created;

    // Called just before this server shuts down.
    fit::callback<void(std::shared_ptr<AudioRendererServer>)> on_shutdown;
  };

  static std::shared_ptr<AudioRendererServer> Create(
      std::shared_ptr<const FidlThread> fidl_thread,
      fidl::ServerEnd<fuchsia_media::AudioRenderer> server_end, Args args);

  // Reports if the renderer is configured and all graph nodes have been created. When true, we are
  // ready to start rendering packets.
  bool IsFullyCreated() const { return state_ == State::kFullyCreated; }

  // Reports current properties of the renderer.
  // These cannot be called unless the renderer is `IsConfigured()`.
  media::audio::RenderUsage usage() const;
  const Format& format() const;

  // Reports graph objects used by this renderer.
  // These cannot be called unless the renderer is `IsConfigured()`.
  NodeId producer_node() const;
  GainControlId stream_gain_control() const;
  std::optional<GainControlId> play_pause_ramp_gain_control() const;

  //
  // Implementation of fidl::WireServer<fuchsia_media::AudioRenderer>.
  //

  // If these functions are called, the call must happen before SetPcmStreamType.
  // For ULTRASOUND renderers, these cannot be called at all.
  void SetPtsUnits(SetPtsUnitsRequestView request, SetPtsUnitsCompleter::Sync& completer) final;
  void SetPtsContinuityThreshold(SetPtsContinuityThresholdRequestView request,
                                 SetPtsContinuityThresholdCompleter::Sync& completer) final;
  void SetReferenceClock(SetReferenceClockRequestView request,
                         SetReferenceClockCompleter::Sync& completer) final;
  void SetUsage(SetUsageRequestView request, SetUsageCompleter::Sync& completer) final;
  void SetUsage2(SetUsage2RequestView request, SetUsage2Completer::Sync& completer) final;

  // Must be called exactly once, after the above and before the below.
  // For ULTRASOUND renderers, this cannot be called at all.
  void SetPcmStreamType(SetPcmStreamTypeRequestView request,
                        SetPcmStreamTypeCompleter::Sync& completer) final;

  // Must be called after SetPcmStreamType + AddPayloadBuffer.
  void SendPacket(SendPacketRequestView request, SendPacketCompleter::Sync& completer) final;
  void SendPacketNoReply(SendPacketNoReplyRequestView request,
                         SendPacketNoReplyCompleter::Sync& completer) final;

  void EndOfStream(EndOfStreamCompleter::Sync& completer) final;

  void DiscardAllPackets(DiscardAllPacketsCompleter::Sync& completer) final;
  void DiscardAllPacketsNoReply(DiscardAllPacketsNoReplyCompleter::Sync& completer) final;

  void Play(PlayRequestView request, PlayCompleter::Sync& completer) final;
  void PlayNoReply(PlayNoReplyRequestView request, PlayNoReplyCompleter::Sync& completer) final;

  void Pause(PauseCompleter::Sync& completer) final;
  void PauseNoReply(PauseNoReplyCompleter::Sync& completer) final;

  // Can be called at any time.
  void AddPayloadBuffer(AddPayloadBufferRequestView request,
                        AddPayloadBufferCompleter::Sync& completer) final;
  void RemovePayloadBuffer(RemovePayloadBufferRequestView request,
                           RemovePayloadBufferCompleter::Sync& completer) final;

  void BindGainControl(BindGainControlRequestView request,
                       BindGainControlCompleter::Sync& completer) final;

  void GetReferenceClock(GetReferenceClockCompleter::Sync& completer) final;
  void GetMinLeadTime(GetMinLeadTimeCompleter::Sync& completer) final;

  void EnableMinLeadTimeEvents(EnableMinLeadTimeEventsRequestView request,
                               EnableMinLeadTimeEventsCompleter::Sync& completer) final;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_media::AudioRenderer> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) final {
    FX_LOGS(ERROR) << "AudioRendererServer: AudioRenderer::handle_unknown_method(ordinal "
                   << metadata.method_ordinal << ", "
                   << (metadata.unknown_method_type == fidl::UnknownMethodType::kOneWay
                           ? "OneWay)"
                           : "TwoWay)");
  }

 private:
  static constexpr std::string_view kClassName = "AudioRendererServer";
  template <typename ServerT, template <typename T> typename FidlServerT, typename ProtocolT>
  friend class BaseFidlServer;

  AudioRendererServer(std::shared_ptr<const FidlThread> fidl_thread, Args args);

  zx_status_t SetUsageCheck();

  void SendPacketInternal(fuchsia_media::StreamPacket packet,
                          std::optional<SendPacketCompleter::Async> completer);
  void EndOfStreamInternal();
  void DiscardAllPacketsInternal(std::optional<DiscardAllPacketsCompleter::Async> completer);
  void PlayInternal(zx::time reference_time, int64_t media_time,
                    std::optional<PlayCompleter::Async> completer);
  void PauseInternal(std::optional<PauseCompleter::Async> completer);

  void OnShutdown(fidl::UnbindInfo info) final;
  void OnMinLeadTimeUpdated(std::optional<zx::duration> min_lead_time);
  void MaybeConfigure();
  void MaybeSetFullyCreated();
  void RunWhenReady(const char* debug_string, fit::closure fn);
  fuchsia_audio_mixer::wire::ReferenceClock ReferenceClockToFidl(fidl::AnyArena& arena);
  bool IsConfigured() const { return state_ >= State::kConfigured; }

  const int64_t renderer_id_;
  const zx::clock default_reference_clock_;
  const bool ramp_on_play_pause_;

  enum class State {
    // Server has shut down.
    kShutdown,
    // Waiting for the client to select a format and payload buffer.
    kWaitingForConfig,
    // Finalized the format and payload buffer. Started graph node creations.
    kConfigured,
    // Graph nodes created.
    kFullyCreated,
  };
  State state_ = State::kWaitingForConfig;

  // State callbacks.
  fit::callback<void(std::shared_ptr<AudioRendererServer>)> on_fully_created_;
  fit::callback<void(std::shared_ptr<AudioRendererServer>)> on_shutdown_;

  // Objects in the mixer graph.
  std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client_;
  std::optional<NodeId> producer_node_;
  std::optional<GainControlId> stream_gain_control_;
  std::optional<GainControlId> play_pause_ramp_gain_control_;
  std::shared_ptr<DelayWatcherClient> delay_watcher_client_;
  fidl::ServerEnd<fuchsia_audio::DelayWatcher> delay_watcher_server_end_;

  // Configuration which is mutable before the StreamSink channel is created.
  // Optional fields will be set to a default value if not set explicitly by the client.
  std::optional<TimelineRate> media_ticks_per_second_;
  std::optional<float> media_ticks_continuity_threshold_seconds_;
  media::audio::RenderUsage usage_;
  std::optional<Format> format_;
  zx::clock reference_clock_;
  std::map<int64_t, zx::vmo> payload_buffers_;

  // For sending packets to the mixer service.
  std::optional<fidl::WireSharedClient<fuchsia_audio::StreamSink>> stream_sink_client_;
  std::optional<fidl::WireSharedClient<fuchsia_audio::GainControl>> stream_gain_control_client_;
  std::optional<fidl::WireSharedClient<fuchsia_audio::GainControl>>
      play_pause_ramp_gain_control_client_;
  std::optional<int64_t> last_pause_media_time_;
  int64_t segment_id_ = 0;

  bool enable_min_lead_time_events_ = false;
  zx::duration min_lead_time_;

  // Commands that are queued between State::kConfigured and State::kFullyCreated.
  std::vector<fit::closure> queued_tasks_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_RENDERER_SERVER_H_
