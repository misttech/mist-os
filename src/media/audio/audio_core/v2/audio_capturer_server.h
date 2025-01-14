// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_CAPTURER_SERVER_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_CAPTURER_SERVER_H_

#include <fidl/fuchsia.audio.mixer/cpp/wire.h>
#include <fidl/fuchsia.audio/cpp/wire.h>
#include <fidl/fuchsia.media/cpp/wire.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fit/function.h>
#include <lib/zx/vmo.h>

#include <memory>
#include <optional>
#include <utility>

#include "src/media/audio/audio_core/shared/device_config.h"
#include "src/media/audio/audio_core/shared/stream_usage.h"
#include "src/media/audio/audio_core/v2/graph_types.h"
#include "src/media/audio/audio_core/v2/reference_clock.h"
#include "src/media/audio/audio_core/v2/stream_sink_server.h"
#include "src/media/audio/lib/format2/format.h"
#include "src/media/audio/services/common/base_fidl_server.h"
#include "src/media/audio/services/common/memory_mapped_buffer.h"

namespace media_audio {

class AudioCapturerServer
    : public BaseFidlServer<AudioCapturerServer, fidl::WireServer, fuchsia_media::AudioCapturer>,
      public std::enable_shared_from_this<AudioCapturerServer> {
 public:
  struct Args {
    // Connection to the mixer service.
    std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client;

    // Initial configuration. If `usage == ULTRASOUND`, then `format` must be set and cannot be
    // changed by the client. Otherwise, the `format` is optional here and can be set by the client.
    media::audio::CaptureUsage usage;
    std::optional<Format> format;

    // Default clock to use if the client does not explicitly choose one.
    // Required.
    zx::clock default_reference_clock;

    // Called when `IsFullyCreated`.
    fit::callback<void(std::shared_ptr<AudioCapturerServer>)> on_fully_created;

    // Called just before this server shuts down.
    fit::callback<void(std::shared_ptr<AudioCapturerServer>)> on_shutdown;
  };

  static std::shared_ptr<AudioCapturerServer> Create(
      std::shared_ptr<const FidlThread> fidl_thread,
      fidl::ServerEnd<fuchsia_media::AudioCapturer> server_end, Args args);

  // Reports if the capturer is configured and all graph objects have been created. When true, we
  // are ready to start capturing packets.
  bool IsFullyCreated() const { return state_ == State::kFullyCreated; }

  // Reports current properties of the capturer.
  // These cannot be called unless the capturer is `IsConfigured()`.
  media::audio::CaptureUsage usage() const;
  const Format& format() const;

  // Reports graph objects used by this capturer.
  // These cannot be called unless the capturer is `IsConfigured()`.
  NodeId consumer_node() const;
  GainControlId stream_gain_control() const;

  //
  // Implementation of fidl::WireServer<fuchsia_media::AudioCapturer>.
  //

  // If these functions are called, the call must happen before SetPcmStreamType.
  // For ULTRASOUND capturers, these cannot be called at all.
  void SetReferenceClock(SetReferenceClockRequestView request,
                         SetReferenceClockCompleter::Sync& completer) final;
  void SetUsage(SetUsageRequestView request, SetUsageCompleter::Sync& completer) final;
  void SetUsage2(SetUsage2RequestView request, SetUsage2Completer::Sync& completer) final;

  // Must be called exactly once, after the above and before the below.
  // For ULTRASOUND capturers, this cannot be called at all.
  void SetPcmStreamType(SetPcmStreamTypeRequestView request,
                        SetPcmStreamTypeCompleter::Sync& completer) final;

  // Must be called after SetPcmStreamType + AddPayloadBuffer.
  void CaptureAt(CaptureAtRequestView request, CaptureAtCompleter::Sync& completer) final;

  void StartAsyncCapture(StartAsyncCaptureRequestView request,
                         StartAsyncCaptureCompleter::Sync& completer) final;

  void StopAsyncCapture(StopAsyncCaptureCompleter::Sync& completer) final;
  void StopAsyncCaptureNoReply(StopAsyncCaptureNoReplyCompleter::Sync& completer) final;

  void ReleasePacket(ReleasePacketRequestView request,
                     ReleasePacketCompleter::Sync& completer) final;

  void DiscardAllPackets(DiscardAllPacketsCompleter::Sync& completer) final;
  void DiscardAllPacketsNoReply(DiscardAllPacketsNoReplyCompleter::Sync& completer) final;

  // Can be called at any time.
  void AddPayloadBuffer(AddPayloadBufferRequestView request,
                        AddPayloadBufferCompleter::Sync& completer) final;
  void RemovePayloadBuffer(RemovePayloadBufferRequestView request,
                           RemovePayloadBufferCompleter::Sync& completer) final;

  void BindGainControl(BindGainControlRequestView request,
                       BindGainControlCompleter::Sync& completer) final;

  void GetReferenceClock(GetReferenceClockCompleter::Sync& completer) final;
  void GetStreamType(GetStreamTypeCompleter::Sync& completer) final;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_media::AudioCapturer> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) final {
    FX_LOGS(ERROR) << "AudioCapturerServer: AudioCapturer::handle_unknown_method(ordinal "
                   << metadata.method_ordinal << ", "
                   << (metadata.unknown_method_type == fidl::UnknownMethodType::kOneWay
                           ? "OneWay)"
                           : "TwoWay)");
  }

 private:
  static constexpr std::string_view kClassName = "AudioCapturerServer";
  template <typename ServerT, template <typename T> typename FidlServerT, typename ProtocolT>
  friend class BaseFidlServer;

  AudioCapturerServer(std::shared_ptr<const FidlThread> fidl_thread, Args args);

  void CaptureAtInternal(uint32_t payload_buffer_id, uint32_t payload_offset,
                         uint32_t frames_per_packet, CaptureAtCompleter::Async completer);
  void StartAsyncCaptureInternal(uint32_t frames_per_packet);
  void StopAsyncCaptureInternal(std::optional<StopAsyncCaptureCompleter::Async> completer);
  void ReleasePacketInternal(fuchsia_media::wire::StreamPacket packet);
  void DiscardAllPacketsInternal(std::optional<DiscardAllPacketsCompleter::Async> completer);

  void SetUsageBase(fuchsia::media::AudioCaptureUsage2 usage);
  void Start();
  void Stop();
  void SendPacket(fuchsia_media::wire::StreamPacket packet, zx::duration overflow,
                  std::optional<CaptureAtCompleter::Async> capture_at_completer);

  void OnShutdown(fidl::UnbindInfo info) final;
  void MaybeConfigure();
  void MaybeSetFullyCreated();
  void RunWhenReady(const char* debug_string, fit::closure fn);
  fuchsia_audio_mixer::wire::ReferenceClock ReferenceClockToFidl(fidl::AnyArena& arena);
  bool IsConfigured() const { return state_ >= State::kConfigured; }

  const int64_t capturer_id_;
  const zx::clock default_reference_clock_;

  enum class State {
    // Server has shut down.
    kShutdown,
    // Waiting for the client to select a format and payload buffer.
    kWaitingForConfig,
    // Finalized the format and payload buffer. Started graph node creations.
    kConfigured,
    // Graph nodes created.
    kFullyCreated,
    // Capturing started on behalf of CaptureAt.
    kCapturingSync,
    // Capturing started on behalf of StartAsyncCapture.
    kCapturingAsync,
  };
  State state_ = State::kWaitingForConfig;

  // State callbacks.
  fit::callback<void(std::shared_ptr<AudioCapturerServer>)> on_fully_created_;
  fit::callback<void(std::shared_ptr<AudioCapturerServer>)> on_shutdown_;

  // Objects in the mixer graph.
  std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client_;
  std::optional<NodeId> consumer_node_;
  std::optional<GainControlId> stream_gain_control_;

  // Configuration which is mutable before the StreamSink channel is created.
  // Optional fields will be set to a default value if not set explicitly by the client.
  media::audio::CaptureUsage usage_;
  std::optional<Format> format_;
  zx::clock reference_clock_;
  std::optional<zx::vmo> payload_buffer_;

  // As in audio_core/v1, only one payload buffer (id=0) is supported. We use two copies of that
  // buffer: the Graph's ConsumerNode writes to `payload_buffer_source_`, then audio is copied from
  // that buffer into `payload_buffer_dest`, which is the actual buffer read by the client.
  //
  // We need two copies because the ConsumerNode's `frames_per_packet` must be set when the
  // ConsumerNode is created, however the CaptureAt and StartAsyncCapture APIs allow the client to
  // decide on packet sizes dynamically. The source payload buffer (passed to the ConsumerNode) uses
  // a fixed packet size of 10ms. The dest payload buffer (read by the client) uses
  // dynamically-sized packets.
  std::shared_ptr<MemoryMappedBuffer> payload_buffer_source_;
  std::shared_ptr<MemoryMappedBuffer> payload_buffer_dest_;

  // For capturing packets from the mixer service.
  std::shared_ptr<StreamSinkServer> stream_sink_server_;
  std::optional<fidl::WireSharedClient<fuchsia_audio::GainControl>> stream_gain_control_client_;
  int64_t pending_sync_captures_ = 0;

  // Commands that are queued between State::kConfigured and State::kReadyToPlay.
  std::vector<fit::closure> queued_tasks_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_CAPTURER_SERVER_H_
