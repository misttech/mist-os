// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_COMPOSITE_H_
#define SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_COMPOSITE_H_

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.virtualaudio/cpp/wire.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/zx/result.h>

#include <cstdio>
#include <deque>

#include <ddktl/device.h>

#include "src/media/audio/drivers/virtual_audio/virtual_audio_device_impl.h"
#include "src/media/audio/drivers/virtual_audio/virtual_audio_driver.h"

namespace virtual_audio {

class VirtualAudioComposite;
using VirtualAudioCompositeDeviceType =
    ddk::Device<VirtualAudioComposite,
                ddk::Messageable<fuchsia_hardware_audio::CompositeConnector>::Mixin>;

// One ring buffer and one DAI interconnect only are supported by this driver.
class VirtualAudioComposite
    : public VirtualAudioCompositeDeviceType,
      public ddk::internal::base_protocol,
      public fidl::Server<fuchsia_hardware_audio::Composite>,
      public fidl::Server<fuchsia_hardware_audio_signalprocessing::SignalProcessing>,
      public fidl::Server<fuchsia_hardware_audio::RingBuffer>,
      public VirtualAudioDriver {
 public:
  static fuchsia_virtualaudio::Configuration GetDefaultConfig();

  VirtualAudioComposite(fuchsia_virtualaudio::Configuration config,
                        std::weak_ptr<VirtualAudioDeviceImpl> owner, zx_device_t* parent);
  async_dispatcher_t* dispatcher() override {
    return fdf::Dispatcher::GetCurrent()->async_dispatcher();
  }
  void ShutdownAndRemove() override { DdkAsyncRemove(); }
  void DdkRelease() {}

  // VirtualAudioDriver overrides.
  // TODO(fxbug.dev/124865): Add support for GetPositionForVA, SetNotificationFrequencyFromVA,
  // and AdjustClockRateFromVA.
  using ErrorT = fuchsia_virtualaudio::Error;
  fit::result<ErrorT, CurrentFormat> GetFormatForVA() override;
  fit::result<ErrorT, CurrentBuffer> GetBufferForVA() override;

 protected:
  // FIDL LLCPP method for fuchsia.hardware.audio.CompositeConnector.
  void Connect(ConnectRequestView request, ConnectCompleter::Sync& completer) override;

  // FIDL natural C++ methods for fuchsia.hardware.audio.Composite.
  void Reset(ResetCompleter::Sync& completer) override { completer.Reply(zx::ok()); }
  void GetProperties(fidl::Server<fuchsia_hardware_audio::Composite>::GetPropertiesCompleter::Sync&
                         completer) override;
  void GetHealthState(GetHealthStateCompleter::Sync& completer) override {
    completer.Reply(fuchsia_hardware_audio::HealthState{}.healthy(true));
  }
  void SignalProcessingConnect(SignalProcessingConnectRequest& request,
                               SignalProcessingConnectCompleter::Sync& completer) override;
  void GetRingBufferFormats(GetRingBufferFormatsRequest& request,
                            GetRingBufferFormatsCompleter::Sync& completer) override;
  void CreateRingBuffer(CreateRingBufferRequest& request,
                        CreateRingBufferCompleter::Sync& completer) override;
  void GetDaiFormats(GetDaiFormatsRequest& request,
                     GetDaiFormatsCompleter::Sync& completer) override;
  void SetDaiFormat(SetDaiFormatRequest& request, SetDaiFormatCompleter::Sync& completer) override;

  // FIDL natural C++ methods for fuchsia.hardware.audio.RingBuffer.
  void GetProperties(fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetPropertiesCompleter::Sync&
                         completer) override;
  void GetVmo(
      GetVmoRequest& request,
      fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetVmoCompleter::Sync& completer) override;
  void Start(StartCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;
  void WatchClockRecoveryPositionInfo(
      WatchClockRecoveryPositionInfoCompleter::Sync& completer) override;
  void WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) override;
  void SetActiveChannels(fuchsia_hardware_audio::RingBufferSetActiveChannelsRequest& request,
                         SetActiveChannelsCompleter::Sync& completer) override;

  // FIDL natural C++ methods for fuchsia.hardware.audio.signalprocessing.SignalProcessing.
  void GetElements(GetElementsCompleter::Sync& completer) override;
  void WatchElementState(WatchElementStateRequest& request,
                         WatchElementStateCompleter::Sync& completer) override;
  void SetElementState(SetElementStateRequest& request,
                       SetElementStateCompleter::Sync& completer) override;
  void GetTopologies(GetTopologiesCompleter::Sync& completer) override;
  void SetTopology(SetTopologyRequest& request, SetTopologyCompleter::Sync& completer) override;

 private:
  static constexpr size_t kNumberOfElements = 2;
  static constexpr uint64_t kRingBufferId = 123;
  static constexpr uint64_t kDaiId = 456;
  static constexpr uint64_t kTopologyId = 789;

  void ResetRingBuffer();
  void OnRingBufferClosed(fidl::UnbindInfo info);
  void OnSignalProcessingClosed(fidl::UnbindInfo info);
  fuchsia_virtualaudio::RingBuffer& GetRingBuffer(uint64_t id);
  fuchsia_virtualaudio::Composite& composite_config() {
    return config_.device_specific()->composite().value();
  }

  // This should never be invalid: this VirtualAudioStream should always be destroyed before
  // its parent. This field is a weak_ptr to avoid a circular reference count.
  const std::weak_ptr<VirtualAudioDeviceImpl> parent_;
  static int instance_count_;
  char instance_name_[64];

  // One ring buffer and one DAI interconnect only are supported by this driver.
  fzl::VmoMapper ring_buffer_mapper_;
  uint32_t notifications_per_ring_ = 0;
  uint32_t num_ring_buffer_frames_ = 0;
  uint32_t frame_size_ = 4;
  zx::vmo ring_buffer_vmo_;
  bool watch_delay_replied_ = false;
  std::optional<WatchDelayInfoCompleter::Async> delay_info_completer_;
  bool watch_position_replied_ = false;
  std::optional<WatchClockRecoveryPositionInfoCompleter::Async> position_info_completer_;
  bool watch_element_replied_[kNumberOfElements] = {};
  std::optional<WatchElementStateCompleter::Async> element_state_completer_[kNumberOfElements];

  bool ring_buffer_vmo_fetched_ = false;
  bool ring_buffer_started_ = false;
  std::optional<fuchsia_hardware_audio::Format> ring_buffer_format_;
  std::optional<fuchsia_hardware_audio::DaiFormat> dai_format_;
  fuchsia_virtualaudio::Configuration config_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_audio::RingBuffer>> ring_buffer_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_audio_signalprocessing::SignalProcessing>>
      signal_;
};

}  // namespace virtual_audio

#endif  // SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_COMPOSITE_H_
