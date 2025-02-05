// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_USAGE_VOLUME_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_USAGE_VOLUME_H_

#include <fidl/fuchsia.audio.mixer/cpp/wire.h>
#include <fidl/fuchsia.audio/cpp/wire.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fit/function.h>

#include <memory>
#include <optional>

#include "src/media/audio/audio_core/shared/stream_usage.h"
#include "src/media/audio/audio_core/shared/stream_volume_manager.h"
#include "src/media/audio/audio_core/shared/volume_curve.h"
#include "src/media/audio/audio_core/v2/graph_types.h"

namespace media_audio {

// Controls the gain of a single usage. Each UsageVolume has two GainControls: a primary control,
// which responds to a volume setting on a scale of 0% to 100%; and an adjustment control, which is
// a dBFS adjustment used to implement ducking and muting policies.
class UsageVolume : public media::audio::StreamVolume {
 public:
  struct Args {
    // Connection to the mixer service.
    std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client;

    // Dispatcher for sending GainControl requests;
    async_dispatcher_t* dispatcher;

    // For translating volume commands to gains;
    std::shared_ptr<media::audio::VolumeCurve> volume_curve;

    // Which usage this control is bound to.
    media::audio::StreamUsage usage;

    // Name of the device
    std::string device_name;

    // Callback to invoke once the constructor completes.
    fit::callback<void(std::shared_ptr<UsageVolume>)> callback;
  };

  static void Create(Args args);
  ~UsageVolume();

  // Implements media::audio::StreamVolume.
  fuchsia::media::Usage2 GetStreamUsage() const final;
  void RealizeVolume(media::audio::VolumeCommand volume_command) final;

  // Returns the underlying GainControls managed by this object.
  const std::vector<GainControlId>& gain_controls() const { return gain_controls_; }

 private:
  static constexpr auto kPrimary = 0;
  static constexpr auto kAdjustment = 1;

  struct ConstructorState {
    std::optional<fidl::WireSharedClient<fuchsia_audio::GainControl>> clients[2];
    std::optional<GainControlId> ids[2];
  };
  UsageVolume(Args args, ConstructorState& state);

  const std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client_;
  const std::shared_ptr<media::audio::VolumeCurve> volume_curve_;
  const media::audio::StreamUsage usage_;

  std::vector<fidl::WireSharedClient<fuchsia_audio::GainControl>> clients_;
  std::vector<GainControlId> gain_controls_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_USAGE_VOLUME_H_
