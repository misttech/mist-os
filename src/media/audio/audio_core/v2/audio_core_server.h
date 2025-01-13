// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_CORE_SERVER_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_CORE_SERVER_H_

#include <fidl/fuchsia.audio.mixer/cpp/wire.h>
#include <fidl/fuchsia.media/cpp/wire.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl/cpp/wire/unknown_interaction_handler.h>
#include <lib/sys/cpp/component_context.h>

#include <map>
#include <memory>
#include <optional>
#include <utility>

#include "src/media/audio/audio_core/shared/audio_admin.h"
#include "src/media/audio/audio_core/shared/stream_volume_manager.h"
#include "src/media/audio/audio_core/shared/volume_curve.h"
#include "src/media/audio/audio_core/v2/renderer_capturer_creator.h"
#include "src/media/audio/services/common/base_fidl_server.h"

namespace media_audio {

class AudioCoreServer
    : public BaseFidlServer<AudioCoreServer, fidl::WireServer, fuchsia_media::AudioCore>,
      public std::enable_shared_from_this<AudioCoreServer> {
 public:
  struct Args {
    std::shared_ptr<RendererCapturerCreator> creator;
    std::shared_ptr<RouteGraph> route_graph;
    std::shared_ptr<media::audio::StreamVolumeManager> stream_volume_manager;
    std::shared_ptr<media::audio::AudioAdmin> audio_admin;
    media::audio::VolumeCurve default_volume_curve;
  };
  static std::shared_ptr<AudioCoreServer> Create(
      std::shared_ptr<const FidlThread> fidl_thread,
      fidl::ServerEnd<fuchsia_media::AudioCore> server_end, Args args);

  //
  // Implementation of fidl::WireServer<fuchsia_media::AudioCore>.
  //

  void CreateAudioRenderer(CreateAudioRendererRequestView request,
                           CreateAudioRendererCompleter::Sync& completer) final;

  void CreateAudioCapturer(CreateAudioCapturerRequestView request,
                           CreateAudioCapturerCompleter::Sync& completer) final;

  void CreateAudioCapturerWithConfiguration(
      CreateAudioCapturerWithConfigurationRequestView request,
      CreateAudioCapturerWithConfigurationCompleter::Sync& completer) final;

  void EnableDeviceSettings(EnableDeviceSettingsRequestView request,
                            EnableDeviceSettingsCompleter::Sync& completer) final {
    ZX_PANIC("Not implemented");
  }

  void SetRenderUsageGain(SetRenderUsageGainRequestView request,
                          SetRenderUsageGainCompleter::Sync& completer) final;
  void SetRenderUsageGain2(SetRenderUsageGain2RequestView request,
                           SetRenderUsageGain2Completer::Sync& completer) final;

  void SetCaptureUsageGain(SetCaptureUsageGainRequestView request,
                           SetCaptureUsageGainCompleter::Sync& completer) final;

  void BindUsageVolumeControl(BindUsageVolumeControlRequestView request,
                              BindUsageVolumeControlCompleter::Sync& completer) final;
  void BindUsageVolumeControl2(BindUsageVolumeControl2RequestView request,
                               BindUsageVolumeControl2Completer::Sync& completer) final;

  void GetVolumeFromDb(GetVolumeFromDbRequestView request,
                       GetVolumeFromDbCompleter::Sync& completer) final;
  void GetVolumeFromDb2(GetVolumeFromDb2RequestView request,
                        GetVolumeFromDb2Completer::Sync& completer) final;

  void GetDbFromVolume(GetDbFromVolumeRequestView request,
                       GetDbFromVolumeCompleter::Sync& completer) final;
  void GetDbFromVolume2(GetDbFromVolume2RequestView request,
                        GetDbFromVolume2Completer::Sync& completer) final;

  void SetInteraction(SetInteractionRequestView request,
                      SetInteractionCompleter::Sync& completer) final;
  void SetInteraction2(SetInteraction2RequestView request,
                       SetInteraction2Completer::Sync& completer) final;

  void ResetInteractions(ResetInteractionsCompleter::Sync& completer) final;

  void LoadDefaults(LoadDefaultsCompleter::Sync& completer) final;

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_media::AudioCore> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) final {
    FX_LOGS(ERROR) << "AudioCoreServer: AudioCore::handle_unknown_method(ordinal "
                   << metadata.method_ordinal << ", "
                   << (metadata.unknown_method_type == fidl::UnknownMethodType::kOneWay
                           ? "OneWay)"
                           : "TwoWay)");
  }

 private:
  static inline constexpr std::string_view kClassName = "AudioCoreServer";
  template <typename ServerT, template <typename T> typename FidlServerT, typename ProtocolT>
  friend class BaseFidlServer;

  void SetRenderUsageGainBase(const fuchsia::media::AudioRenderUsage2& usage, float gain_db);
  void LoadDefaults();

  explicit AudioCoreServer(Args args);

  const std::shared_ptr<RendererCapturerCreator> creator_;
  const std::shared_ptr<RouteGraph> route_graph_;
  const std::shared_ptr<media::audio::StreamVolumeManager> stream_volume_manager_;
  const std::shared_ptr<media::audio::AudioAdmin> audio_admin_;
  const media::audio::VolumeCurve default_volume_curve_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_CORE_SERVER_H_
