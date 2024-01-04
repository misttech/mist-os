// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_CORE_COMPONENT_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_CORE_COMPONENT_H_

#include <lib/async/dispatcher.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/trace-provider/provider.h>

#include "src/media/audio/audio_core/shared/activity_dispatcher.h"
#include "src/media/audio/audio_core/shared/audio_policy.h"
#include "src/media/audio/audio_core/shared/device_lister.h"
#include "src/media/audio/audio_core/shared/process_config.h"
#include "src/media/audio/audio_core/shared/profile_provider.h"
#include "src/media/audio/audio_core/shared/stream_volume_manager.h"
#include "src/media/audio/audio_core/shared/usage_gain_reporter_impl.h"
#include "src/media/audio/audio_core/shared/usage_reporter_impl.h"
#include "src/media/audio/audio_core/v2/renderer_capturer_creator.h"

namespace media_audio {

// Everything needed to run the "audio_core" component.
class AudioCoreComponent {
 public:
  // Start running the service. Discoverable protocols are published to `outgoing` and served from
  // `fidl_thread`. Cobalt reporting is enabled iff `enable_cobalt`.
  AudioCoreComponent(component::OutgoingDirectory& outgoing,
                     std::shared_ptr<const FidlThread> fidl_thread, bool enable_cobalt);

 private:
  // TODO(https://fxbug.dev/98652): delete when we have a real implementation
  class EmptyDeviceLister : public media::audio::DeviceLister {
    std::vector<fuchsia::media::AudioDeviceInfo> GetDeviceInfos() { return {}; }
  };

  std::shared_ptr<const FidlThread> fidl_thread_;

  // Configs.
  media::audio::ProcessConfig process_config_;
  media::audio::AudioPolicy policy_config_;

  // Objects that serve discoverable FIDL protocols.
  std::shared_ptr<media::audio::ActivityDispatcherImpl> activity_dispatcher_;
  std::shared_ptr<media::audio::ProfileProvider> profile_provider_;
  std::shared_ptr<media::audio::UsageGainReporterImpl> usage_gain_reporter_;
  std::shared_ptr<media::audio::UsageReporterImpl> usage_reporter_;

  // TODO(https://fxbug.dev/98652):
  // fuchsia.media.AudioDeviceEnumerator
  // fuchsia.media.tuning.AudioTuner

  // Misc objects.
  EmptyDeviceLister empty_device_lister_;

  std::unique_ptr<async::Loop> io_loop_;
  std::unique_ptr<trace::TraceProviderWithFdio> trace_provider_;
  std::shared_ptr<media::audio::StreamVolumeManager> stream_volume_manager_;
  std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client_;
  std::shared_ptr<RouteGraph> route_graph_;
  std::shared_ptr<RendererCapturerCreator> renderer_capturer_creator_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_CORE_COMPONENT_H_
