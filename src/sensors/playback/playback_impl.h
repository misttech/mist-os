// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_SENSORS_PLAYBACK_PLAYBACK_IMPL_H_
#define SRC_SENSORS_PLAYBACK_PLAYBACK_IMPL_H_

#include <fidl/fuchsia.hardware.sensors/cpp/fidl.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/scope.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>

#include "src/camera/lib/actor/actor_base.h"
#include "src/sensors/playback/playback_controller.h"

namespace sensors::playback {

class PlaybackImpl : public camera::actor::ActorBase,
                     public fidl::Server<fuchsia_hardware_sensors::Playback> {
  using ConfigurePlaybackError = fuchsia_hardware_sensors::ConfigurePlaybackError;
  using FixedValuesPlaybackConfig = fuchsia_hardware_sensors::FixedValuesPlaybackConfig;
  using PlaybackSourceConfig = fuchsia_hardware_sensors::PlaybackSourceConfig;
  using Playback = fuchsia_hardware_sensors::Playback;

 public:
  PlaybackImpl(async_dispatcher_t* dispatcher, PlaybackController& controller);

  void ConfigurePlayback(ConfigurePlaybackRequest& request,
                         ConfigurePlaybackCompleter::Sync& completer) override;

  void handle_unknown_method(fidl::UnknownMethodMetadata<Playback> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

  // Server connection management.
  static void BindSelfManagedServer(async_dispatcher_t* dispatcher, PlaybackController& controller,
                                    fidl::ServerEnd<Playback> server_end) {
    std::unique_ptr impl = std::make_unique<PlaybackImpl>(dispatcher, controller);
    PlaybackImpl* impl_ptr = impl.get();

    fidl::ServerBindingRef binding_ref = fidl::BindServer(
        dispatcher, std::move(server_end), std::move(impl), std::mem_fn(&PlaybackImpl::OnUnbound));
    impl_ptr->binding_ref_.emplace(std::move(binding_ref));
  }

  void OnUnbound(fidl::UnbindInfo info, fidl::ServerEnd<Playback> server_end);

 private:
  std::optional<fidl::ServerBindingRef<Playback>> binding_ref_;

  PlaybackController& controller_;

  // This should always be the last thing in the object. Otherwise scheduled tasks within this scope
  // which reference members of this object may be allowed to run after destruction of this object
  // has started. Keeping this at the end ensures that the scope is destroyed first, cancelling any
  // scheduled tasks before the rest of the members are destroyed.
  fpromise::scope scope_;
};

}  // namespace sensors::playback

#endif  // SRC_SENSORS_PLAYBACK_PLAYBACK_IMPL_H_
