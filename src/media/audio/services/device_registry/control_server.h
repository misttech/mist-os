// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_CONTROL_SERVER_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_CONTROL_SERVER_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>

#include <cstdint>
#include <memory>
#include <optional>

#include "src/media/audio/services/common/base_fidl_server.h"
#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/control_notify.h"
#include "src/media/audio/services/device_registry/device.h"

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
  void TopologyChanged(TopologyId topology_id) final;
  void ElementStateChanged(
      ElementId element_id,
      fuchsia_hardware_audio_signalprocessing::ElementState element_state) final;

  // ControlNotify
  //
  void DeviceDroppedRingBuffer(ElementId element_id) final;
  void DelayInfoChanged(ElementId element_id, const fuchsia_audio_device::DelayInfo&) final;
  // If `dai_format` contains no value, no DaiFormat is set. The Device might be newly-initialized,
  // or `Reset` may have been called. `SetDaiFormat` must be called.
  void DaiFormatChanged(
      ElementId element_id, const std::optional<fuchsia_hardware_audio::DaiFormat>& dai_format,
      const std::optional<fuchsia_hardware_audio::CodecFormatInfo>& codec_format_info) final;
  // `SetDaiFormat` did not change the format. The previously-set DaiFormat is still be in effect.
  void DaiFormatNotSet(ElementId element_id, const fuchsia_hardware_audio::DaiFormat& dai_format,
                       fuchsia_audio_device::ControlSetDaiFormatError error) final;
  void CodecStarted(const zx::time& start_time) final;
  // A call to `CodecStart` did not succeed.
  void CodecNotStarted() final;
  void CodecStopped(const zx::time& stop_time) final;
  // A call to `CodecStop` did not succeed.
  void CodecNotStopped() final;

  // fuchsia.audio.device.Control
  //
  void SetGain(SetGainRequest& request, SetGainCompleter::Sync& completer) final;
  void CreateRingBuffer(CreateRingBufferRequest& request,
                        CreateRingBufferCompleter::Sync& completer) final;
  void SetDaiFormat(SetDaiFormatRequest& request, SetDaiFormatCompleter::Sync& completer) final;
  void Reset(ResetCompleter::Sync& completer) final;
  void CodecStart(CodecStartCompleter::Sync& completer) final;
  void CodecStop(CodecStopCompleter::Sync& completer) final;

  // fuchsia.hardware.audio.signalprocessing support
  //
  void GetTopologies(GetTopologiesCompleter::Sync& completer) final;
  void GetElements(GetElementsCompleter::Sync& completer) final;
  void WatchTopology(WatchTopologyCompleter::Sync& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void WatchElementState(WatchElementStateRequest& request,
                         WatchElementStateCompleter::Sync& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void SetTopology(SetTopologyRequest& request, SetTopologyCompleter::Sync& completer) final {
    completer.Reply(fit::error(ZX_ERR_NOT_SUPPORTED));
  }
  void SetElementState(SetElementStateRequest& request,
                       SetElementStateCompleter::Sync& completer) final {
    completer.Reply(fit::error(ZX_ERR_NOT_SUPPORTED));
  }

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

  std::optional<CodecStartCompleter::Async> codec_start_completer_;
  std::optional<CodecStopCompleter::Async> codec_stop_completer_;

  bool device_has_error_ = false;

  // per-ElementId fields:
  //
  std::unordered_map<ElementId, SetDaiFormatCompleter::Async> set_dai_format_completers_;
  std::unordered_map<ElementId, CreateRingBufferCompleter::Async> create_ring_buffer_completers_;

  // Locks a weak_ptr ring_buffer_server_ to shared_ptr and returns it, or returns nullptr.
  std::shared_ptr<RingBufferServer> TryGetRingBufferServer(ElementId element_id);
  std::unordered_map<ElementId, std::weak_ptr<RingBufferServer>> ring_buffer_servers_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_CONTROL_SERVER_H_
