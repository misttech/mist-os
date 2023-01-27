// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_CONTROL_SERVER_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_CONTROL_SERVER_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>

#include <cstdint>
#include <optional>
#include <unordered_set>

#include "src/media/audio/services/common/base_fidl_server.h"
#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/control_notify.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

class RingBufferServer;

// FIDL server for fuchsia_audio_device/Control. Claims a Device and makes "mutable" calls on it.
class ControlServer
    : public std::enable_shared_from_this<ControlServer>,
      public ControlNotify,
      public BaseFidlServer<ControlServer, fidl::Server, fuchsia_audio_device::Control> {
 public:
  static std::shared_ptr<ControlServer> Create(
      std::shared_ptr<const FidlThread> thread,
      fidl::ServerEnd<fuchsia_audio_device::Control> server_end,
      std::shared_ptr<AudioDeviceRegistry> parent, std::shared_ptr<Device> device);

  void OnShutdown(fidl::UnbindInfo info) override;
  ~ControlServer() override;

  // ObserverNotify
  //
  void DeviceIsRemoved() final;
  void DeviceHasError() final;
  void GainStateChanged(const fuchsia_audio_device::GainState&) final;
  void PlugStateChanged(const fuchsia_audio_device::PlugState& new_plug_state,
                        zx::time plug_change_time) final;
  // ControlNotify
  //
  void DeviceDroppedRingBuffer() final;
  void DelayInfoChanged(const fuchsia_audio_device::DelayInfo&) final;

  // fuchsia.audio.device.Control
  //
  void SetGain(SetGainRequest& request, SetGainCompleter::Sync& completer) final;
  void GetCurrentlyPermittedFormats(GetCurrentlyPermittedFormatsCompleter::Sync& completer) final;
  void CreateRingBuffer(CreateRingBufferRequest& request,
                        CreateRingBufferCompleter::Sync& completer) final;

  // fuchsia.hardware.audio.signalprocessing support
  //
  void GetElements(GetElementsCompleter::Sync& completer) final {}
  void WatchElementState(WatchElementStateRequest& request,
                         WatchElementStateCompleter::Sync& completer) final {}
  void GetTopologies(GetTopologiesCompleter::Sync& completer) final {}
  void SetElementState(SetElementStateRequest& request,
                       SetElementStateCompleter::Sync& completer) final {}
  void SetTopology(SetTopologyRequest& request, SetTopologyCompleter::Sync& completer) final {}

  // Static object count, for debugging purposes.
  static inline uint64_t count() { return count_; }

  bool ControlledDeviceReceivedError() const { return device_has_error_; }

 private:
  template <typename ServerT, template <typename T> typename FidlServerT, typename ProtocolT>
  friend class BaseFidlServer;

  static inline const std::string_view kClassName = "ControlServer";
  static inline uint64_t count_ = 0;

  ControlServer(std::shared_ptr<AudioDeviceRegistry> parent, std::shared_ptr<Device> device);

  std::shared_ptr<AudioDeviceRegistry> parent_;
  std::shared_ptr<Device> device_;

  std::optional<GetCurrentlyPermittedFormatsCompleter::Async>
      currently_permitted_formats_completer_;
  std::optional<CreateRingBufferCompleter::Async> create_ring_buffer_completer_;

  // Locks weak_ptr ring_buffer_server_ to shared_ptr and returns it.
  // If it cannot, returns nullptr and resets the optional.
  std::shared_ptr<RingBufferServer> GetRingBufferServer();
  std::optional<std::weak_ptr<RingBufferServer>> ring_buffer_server_;

  uint64_t min_ring_buffer_frames_;

  fuchsia_audio_device::DelayInfo delay_info_;

  bool device_has_error_ = false;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_CONTROL_SERVER_H_
