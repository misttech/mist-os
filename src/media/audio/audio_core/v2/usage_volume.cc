// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/usage_volume.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>

#include "src/media/audio/audio_core/v2/reference_clock.h"

namespace media_audio {

namespace {
using ::fuchsia_audio::wire::GainControlSetGainRequest;
using ::fuchsia_audio::wire::GainUpdateMethod;
using ::fuchsia_audio::wire::RampedGain;
using ::fuchsia_audio::wire::RampFunction;
using ::fuchsia_audio::wire::RampFunctionLinearSlope;
using ::media::audio::StreamUsage;
using ::media::audio::VolumeCommand;
using ::media::audio::VolumeCurve;
}  // namespace

void UsageVolume::Create(Args args) {
  // Save this because `args` will be moved into the `constructor` closure.
  auto& graph_client = *args.graph_client;
  auto clock = ReferenceClock::FromMonotonic();
  const std::string usage_name =
      args.device_name + (args.usage.is_render_usage()
                              ? media::audio::RenderUsageToString(args.usage.render_usage())
                              : media::audio::CaptureUsageToString(args.usage.capture_usage()));

  // The GainControl objects are constructed asynchronously.
  auto state = std::make_shared<ConstructorState>(ConstructorState{});
  auto constructor = std::make_shared<fit::closure>([state, args = std::move(args)]() mutable {
    if (!state->ids[0] || !state->ids[1]) {
      return;
    }
    auto callback = std::move(args.callback);
    callback(std::shared_ptr<UsageVolume>(new UsageVolume(std::move(args), *state)));
  });

  fidl::Arena<> arena;

  for (int k = 0; k < 2; k++) {
    // In practice this should never fail. The only possible error is ZX_ERR_NO_MEMORY, of which the
    // kernel docs say "In a future build this error will no longer occur".
    auto endpoints = fidl::CreateEndpoints<fuchsia_audio::GainControl>();
    if (!endpoints.is_ok()) {
      FX_PLOGS(FATAL, endpoints.status_value()) << "fidl::CreateEndpoints failed";
    }

    state->clients[k] = fidl::WireSharedClient(std::move(endpoints->client), args.dispatcher);

    graph_client
        ->CreateGainControl(
            fuchsia_audio_mixer::wire::GraphCreateGainControlRequest::Builder(arena)
                .name((k == kPrimary ? "PrimaryGainControlFor" : "AdjustmentGainControlFor") +
                      usage_name)
                .control(std::move(endpoints->server))
                .reference_clock(clock.ToFidl(arena))
                .Build())
        .Then([state, constructor, k](auto& result) {
          if (!result.ok()) {
            FX_LOGS(WARNING) << "CreateGainControl: failed with transport error: " << result;
            return;
          }
          if (!result->is_ok()) {
            FX_LOGS(ERROR) << "CreateGainControl: failed with code: "
                           << fidl::ToUnderlying(result->error_value());
            return;
          }
          if (!result->value()->has_id()) {
            FX_LOGS(ERROR) << "CreateGainControl bug: response missing `id`";
            return;
          }
          state->ids[k] = result->value()->id();
          (*constructor)();
        });
  }
}

UsageVolume::UsageVolume(Args args, ConstructorState& state)
    : graph_client_(std::move(args.graph_client)),
      volume_curve_(std::move(args.volume_curve)),
      usage_(args.usage) {
  // std::vector's constructor cannot accept an initializer list of move-only objects, so this
  // initialization must be done with push_back.
  for (int k = 0; k < 2; k++) {
    clients_.push_back(std::move(*state.clients[k]));
    gain_controls_.push_back(*state.ids[k]);
  }
}

UsageVolume::~UsageVolume() {
  // Delete graph objects.
  for (int k = 0; k < 2; k++) {
    fidl::Arena<> arena;
    (*graph_client_)
        ->DeleteGainControl(fuchsia_audio_mixer::wire::GraphDeleteGainControlRequest::Builder(arena)
                                .id(gain_controls_[k])
                                .Build())
        .Then([k](auto& result) {
          if (result.ok() && !result->is_ok()) {
            FX_LOGS(ERROR) << "DeleteGainControl(" << k
                           << "): failed with code: " << fidl::ToUnderlying(result->error_value());
          }
        });
  }
}

fuchsia::media::Usage2 UsageVolume::GetStreamUsage() const {
  if (usage_.is_render_usage()) {
    auto out = media::audio::FidlRenderUsageFromRenderUsage(usage_.render_usage());
    FX_CHECK(out);
    return fuchsia::media::Usage2::WithRenderUsage(std::move(*out));
  } else {
    auto out = media::audio::FidlCaptureUsageFromCaptureUsage(usage_.capture_usage());
    FX_CHECK(out);
    return fuchsia::media::Usage2::WithCaptureUsage(std::move(*out));
  }
}

void UsageVolume::RealizeVolume(VolumeCommand command) {
  const float target_dbs[2] = {
      /*primary*/ volume_curve_->VolumeToDb(command.volume),
      /*adjustment*/ command.gain_db_adjustment,
  };

  for (int k = 0; k < 2; k++) {
    fidl::Arena<> arena;
    if (command.ramp) {
      FX_CHECK(command.ramp->ramp_type == fuchsia::media::audio::RampType::SCALE_LINEAR);
      clients_[k]
          ->SetGain(GainControlSetGainRequest::Builder(arena)
                        .how(GainUpdateMethod::WithRamped(
                            arena, RampedGain::Builder(arena)
                                       .target_gain_db(target_dbs[k])
                                       .duration(command.ramp->duration.to_nsecs())
                                       .function(RampFunction::WithLinearSlope(
                                           arena, RampFunctionLinearSlope::Builder(arena).Build()))
                                       .Build()))
                        .when(fuchsia_media2::wire::RealTime::WithAsap({}))
                        .Build())
          .Then([k](auto& result) {
            if (result.ok() && !result->is_ok()) {
              FX_LOGS(ERROR) << "SetGain(" << k << ", with ramp): failed with code: "
                             << fidl::ToUnderlying(result->error_value());
            }
          });
    } else {
      clients_[k]
          ->SetGain(GainControlSetGainRequest::Builder(arena)
                        .how(GainUpdateMethod::WithGainDb(target_dbs[k]))
                        .when(fuchsia_media2::wire::RealTime::WithAsap({}))
                        .Build())
          .Then([k](auto& result) {
            if (result.ok() && !result->is_ok()) {
              FX_LOGS(ERROR) << "SetGain(" << k << "): failed with code: "
                             << fidl::ToUnderlying(result->error_value());
            }
          });
    }
  }
}

}  // namespace media_audio
