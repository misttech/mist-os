// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_SERVER_H_
#define SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_SERVER_H_

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/zx/result.h>

#include <unordered_map>

#include <ddktl/metadata/audio.h>

#include "src/media/audio/drivers/aml-g12-tdm/aml-tdm-config-device.h"

namespace audio::aml_g12 {

constexpr size_t kNumberOfPipelines = 3;
constexpr size_t kNumberOfTdmEngines = 2 * kNumberOfPipelines;  // 2, 1 for input 1 for output.

struct Engine {
  size_t dai_index;
  std::optional<AmlTdmConfigDevice> device;
  metadata::AmlConfig config;
};

class Server : public fidl::Server<fuchsia_hardware_audio::Composite>,
               public fidl::Server<fuchsia_hardware_audio::RingBuffer>,
               public fidl::Server<fuchsia_hardware_audio_signalprocessing::SignalProcessing> {
 public:
  Server(std::array<std::optional<fdf::MmioBuffer>, kNumberOfTdmEngines> mmios,
         async_dispatcher_t* dispatcher);
  async_dispatcher_t* dispatcher() { return dispatcher_; }

 protected:
  // FIDL natural C++ methods for fuchsia.hardware.audio.Composite.
  void Reset(ResetCompleter::Sync& completer) override;
  void GetProperties(fidl::Server<fuchsia_hardware_audio::Composite>::GetPropertiesCompleter::Sync&
                         completer) override;
  void GetHealthState(GetHealthStateCompleter::Sync& completer) override;
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
  void WatchTopology(WatchTopologyCompleter::Sync& completer) override;
  void SetTopology(SetTopologyRequest& request, SetTopologyCompleter::Sync& completer) override;

 private:
  static constexpr std::array<uint64_t, kNumberOfPipelines> kDaiIds = {1, 2, 3};
  static constexpr std::array<uint64_t, kNumberOfTdmEngines> kRingBufferIds = {4, 5, 6, 7, 8, 9};
  static constexpr uint64_t kTopologyId = 1;

  void OnSignalProcessingClosed(fidl::UnbindInfo info);
  zx_status_t ResetEngine(size_t index);
  zx_status_t ConfigEngine(size_t index, size_t dai_index, bool input, fdf::MmioBuffer mmio);

  async_dispatcher_t* dispatcher_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_audio_signalprocessing::SignalProcessing>>
      signal_;

  std::array<Engine, kNumberOfTdmEngines> engines_;
  std::array<fuchsia_hardware_audio::DaiSupportedFormats, kNumberOfPipelines>
      supported_dai_formats_;
  std::array<std::optional<fuchsia_hardware_audio::DaiFormat>, kNumberOfPipelines>
      current_dai_formats_;
};

}  // namespace audio::aml_g12

#endif  // SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_SERVER_H_
