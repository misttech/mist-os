// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_OBSERVER_SERVER_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_OBSERVER_SERVER_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>

#include "src/media/audio/services/common/base_fidl_server.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/observer_notify.h"

namespace media_audio {

// FIDL server for fuchsia_audio_device/Observer. This class makes "immutable" (read-only) calls to
// a Device, and otherwise watches it for state changes.
class ObserverServer
    : public std::enable_shared_from_this<ObserverServer>,
      public ObserverNotify,
      public BaseFidlServer<ObserverServer, fidl::Server, fuchsia_audio_device::Observer> {
 public:
  static std::shared_ptr<ObserverServer> Create(
      std::shared_ptr<const FidlThread> thread,
      fidl::ServerEnd<fuchsia_audio_device::Observer> server_end,
      std::shared_ptr<const Device> device);

  ~ObserverServer() override;

  // ObserverNotify
  void DeviceIsRemoved() final;
  void DeviceHasError() final;
  void GainStateChanged(const fuchsia_audio_device::GainState&) final;
  void PlugStateChanged(const fuchsia_audio_device::PlugState& new_plug_state,
                        zx::time plug_change_time) final;
  void TopologyChanged(TopologyId topology_id) final;
  void ElementStateChanged(
      ElementId element_id,
      fuchsia_hardware_audio_signalprocessing::ElementState element_state) final;

  // fuchsia.audio.device.Observer implementation
  void WatchGainState(WatchGainStateCompleter::Sync& completer) final;
  void WatchPlugState(WatchPlugStateCompleter::Sync& completer) final;
  void GetReferenceClock(GetReferenceClockCompleter::Sync& completer) final;

  // Stub signal_processing implementation
  void GetElements(GetElementsCompleter::Sync& completer) final {}
  void WatchElementState(WatchElementStateRequest& request,
                         WatchElementStateCompleter::Sync& completer) final {}
  void GetTopologies(GetTopologiesCompleter::Sync& completer) final {}
  void WatchTopology(WatchTopologyCompleter::Sync& completer) final {}

  // Static object count, for debugging purposes.
  static inline uint64_t count() { return count_; }

 private:
  template <typename ServerT, template <typename T> typename FidlServerT, typename ProtocolT>
  friend class BaseFidlServer;

  static inline const std::string_view kClassName = "ObserverServer";
  static inline uint64_t count_ = 0;

  explicit ObserverServer(std::shared_ptr<const Device> device);

  std::optional<fuchsia_audio_device::GainState> new_gain_state_to_notify_;
  std::optional<WatchGainStateCompleter::Async> watch_gain_state_completer_;

  std::optional<fuchsia_audio_device::ObserverWatchPlugStateResponse> new_plug_state_to_notify_;
  std::optional<WatchPlugStateCompleter::Async> watch_plug_state_completer_;

  bool has_error_ = false;

  std::shared_ptr<const Device> device_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_OBSERVER_SERVER_H_
