// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_DEVICE_WATCHER_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_DEVICE_WATCHER_H_

#include <fidl/fuchsia.audio.device/cpp/wire.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fit/function.h>

#include <memory>
#include <unordered_map>

#include "src/media/audio/audio_core/shared/device_config.h"
#include "src/media/audio/audio_core/shared/device_lister.h"
#include "src/media/audio/audio_core/v2/device.h"
#include "src/media/audio/audio_core/v2/graph_types.h"
#include "src/media/audio/audio_core/v2/route_graph.h"
#include "src/media/audio/lib/effects_loader/effects_loader_v2.h"

namespace media_audio {

// Watches for new devices to be added/removed. One Device object is created for each active device.
// When each device is plugged in/out, it is added/removed from the RouteGraph.
class DeviceWatcher : public media::audio::DeviceLister,
                      public std::enable_shared_from_this<DeviceWatcher> {
 public:
  struct Args {
    // Connection to the mixer service.
    std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client;

    // Connections to ADR.
    fidl::ClientEnd<fuchsia_audio_device::Registry> registry_client;
    fidl::ClientEnd<fuchsia_audio_device::ControlCreator> control_creator_client;

    // Dispatcher for managing FIDL connections.
    async_dispatcher_t* dispatcher;

    // All output devices run on the same thread.
    ThreadId output_thread;
    media::audio::MixProfileConfig output_thread_profile;

    // All input devices run on the same thread.
    ThreadId input_thread;
    media::audio::MixProfileConfig input_thread_profile;

    // Configuration for all devices.
    media::audio::DeviceConfig config;

    // For routing devices.
    std::shared_ptr<RouteGraph> route_graph;

    // For loading effects in OutputDevicePipelines.
    std::shared_ptr<media::audio::EffectsLoaderV2> effects_loader;
  };

  explicit DeviceWatcher(Args args);

  // Implements `media::audio::DeviceLister`.
  std::vector<fuchsia::media::AudioDeviceInfo> GetDeviceInfos();

 private:
  void WatchDevicesAdded();
  void WatchDevicesRemoved();

  void AddDevice(fuchsia_audio_device::wire::Info info);
  void AddOutputDevice(fuchsia_audio_device::wire::Info info,
                       fidl::ClientEnd<fuchsia_audio_device::Observer> observer_client,
                       fidl::ClientEnd<fuchsia_audio_device::Control> control_client);
  void AddInputDevice(fuchsia_audio_device::wire::Info info,
                      fidl::ClientEnd<fuchsia_audio_device::Observer> observer_client,
                      fidl::ClientEnd<fuchsia_audio_device::Control> control_client);
  void RemoveDevice(TokenId token_id);

  const std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client_;
  const fidl::WireSharedClient<fuchsia_audio_device::Registry> registry_client_;
  const fidl::WireSharedClient<fuchsia_audio_device::ControlCreator> control_creator_client_;
  async_dispatcher_t* const dispatcher_;

  const ThreadId output_thread_;
  const ThreadId input_thread_;
  const media::audio::MixProfileConfig output_thread_profile_;
  const media::audio::MixProfileConfig input_thread_profile_;
  const media::audio::DeviceConfig config_;
  const std::shared_ptr<RouteGraph> route_graph_;
  const std::shared_ptr<media::audio::EffectsLoaderV2> effects_loader_;

  std::unordered_map<TokenId, std::shared_ptr<Device>> devices_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_DEVICE_WATCHER_H_
