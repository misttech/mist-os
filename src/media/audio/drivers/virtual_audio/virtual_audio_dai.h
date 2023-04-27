// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_DAI_H_
#define SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_DAI_H_

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.virtualaudio/cpp/wire.h>
#include <lib/affine/transform.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/simple-audio-stream/simple-audio-stream.h>
#include <lib/zx/clock.h>
#include <lib/zx/result.h>

#include <cstdio>
#include <deque>

#include <audio-proto/audio-proto.h>
#include <fbl/ref_ptr.h>

#include "src/media/audio/drivers/virtual_audio/virtual_audio_device_impl.h"
#include "src/media/audio/drivers/virtual_audio/virtual_audio_driver.h"

namespace virtual_audio {

class VirtualAudioDai;
using VirtualAudioDaiDeviceType =
    ddk::Device<VirtualAudioDai, ddk::Messageable<fuchsia_hardware_audio::DaiConnector>::Mixin>;

class VirtualAudioDai : public VirtualAudioDaiDeviceType,
                        public ddk::internal::base_protocol,
                        public fidl::Server<fuchsia_hardware_audio::Dai>,
                        public VirtualAudioDriver {
 public:
  VirtualAudioDai(const VirtualAudioDeviceImpl::Config& cfg,
                  std::weak_ptr<VirtualAudioDeviceImpl> owner, zx_device_t* parent)
      : VirtualAudioDaiDeviceType(parent), parent_(std::move(owner)) {
    ddk_proto_id_ = ZX_PROTOCOL_DAI;
    sprintf(instance_name_, "virtual-audio-dai-%d", instance_count_++);
    zx_status_t status = DdkAdd(ddk::DeviceAddArgs(instance_name_));
    ZX_ASSERT_MSG(status == ZX_OK, "DdkAdd failed");
  }
  async_dispatcher_t* dispatcher() override {
    return fdf::Dispatcher::GetCurrent()->async_dispatcher();
  }
  void ShutdownAndRemove() override { DdkAsyncRemove(); }
  void DdkRelease() {}

  // VirtualAudioDriver overrides.
  using ErrorT = fuchsia_virtualaudio::Error;
  fit::result<ErrorT, CurrentFormat> GetFormatForVA() override;
  fit::result<ErrorT, CurrentGain> GetGainForVA() override;
  fit::result<ErrorT, CurrentBuffer> GetBufferForVA() override;
  fit::result<ErrorT, CurrentPosition> GetPositionForVA() override;

  fit::result<ErrorT> SetNotificationFrequencyFromVA(uint32_t notifications_per_ring) override;
  fit::result<ErrorT> ChangePlugStateFromVA(bool plugged) override;
  fit::result<ErrorT> AdjustClockRateFromVA(int32_t ppm_from_monotonic) override;

 protected:
  // FIDL LLCPP method for fuchsia.hardware.audio.DaiConnector.
  void Connect(ConnectRequestView request, ConnectCompleter::Sync& completer) override {
    fidl::BindServer(dispatcher(), std::move(request->dai_protocol), this);
  }

  // FIDL natural C++ methods for fuchsia.hardware.audio.Dai.
  void Reset(ResetCompleter::Sync& completer) override { completer.Reply(); }
  void GetProperties(GetPropertiesCompleter::Sync& completer) override {
    fidl::Arena arena;
    fuchsia_hardware_audio::DaiProperties properties;
    properties.is_input(false);
    completer.Reply(std::move(properties));
  }
  void GetHealthState(GetHealthStateCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void SignalProcessingConnect(SignalProcessingConnectRequest& request,
                               SignalProcessingConnectCompleter::Sync& completer) override {
    request.protocol().Close(ZX_ERR_NOT_SUPPORTED);
  }
  void GetRingBufferFormats(GetRingBufferFormatsCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  void GetDaiFormats(GetDaiFormatsCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void CreateRingBuffer(CreateRingBufferRequest& request,
                        CreateRingBufferCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  // This should never be invalid: this VirtualAudioStream should always be destroyed before
  // its parent. This field is a weak_ptr to avoid a circular reference count.
  const std::weak_ptr<VirtualAudioDeviceImpl> parent_;
  int instance_count_;
  char instance_name_[64];
};

}  // namespace virtual_audio

#endif  // SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_DAI_H_
