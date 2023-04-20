// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_DAI_H_
#define SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_DAI_H_

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
                        public fidl::WireServer<fuchsia_hardware_audio::Dai>,
                        public VirtualAudioDriver {
 public:
  VirtualAudioDai(const VirtualAudioDeviceImpl::Config& cfg,
                  std::weak_ptr<VirtualAudioDeviceImpl> owner, zx_device_t* parent)
      : VirtualAudioDaiDeviceType(parent),
        parent_(std::move(owner)),
        loop_(&kAsyncLoopConfigNeverAttachToThread) {
    ddk_proto_id_ = ZX_PROTOCOL_DAI;
    loop_.StartThread("virtual-audio-dai-driver");
    sprintf(instance_name_, "virtual-audio-dai-%d", instance_count_++);
    zx_status_t status = DdkAdd(ddk::DeviceAddArgs(instance_name_));
    ZX_ASSERT_MSG(status == ZX_OK, "DdkAdd failed");
  }
  async_dispatcher_t* dispatcher() override { return loop_.dispatcher(); }
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
    fidl::BindServer(loop_.dispatcher(), std::move(request->dai_protocol), this);
  }

  // FIDL LLCPP methods for fuchsia.hardware.audio.Dai.
  void Reset(ResetCompleter::Sync& completer) override { completer.Reply(); }
  void GetProperties(GetPropertiesCompleter::Sync& completer) override {
    fidl::Arena arena;
    auto builder = fuchsia_hardware_audio::wire::DaiProperties::Builder(arena);
    builder.is_input(false);
    completer.Reply(builder.Build());
  }
  void GetHealthState(GetHealthStateCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void SignalProcessingConnect(SignalProcessingConnectRequestView request,
                               SignalProcessingConnectCompleter::Sync& completer) override {
    request->protocol.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void GetRingBufferFormats(GetRingBufferFormatsCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  void GetDaiFormats(GetDaiFormatsCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void CreateRingBuffer(CreateRingBufferRequestView request,
                        CreateRingBufferCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  // This should never be invalid: this VirtualAudioStream should always be destroyed before
  // its parent. This field is a weak_ptr to avoid a circular reference count.
  const std::weak_ptr<VirtualAudioDeviceImpl> parent_;
  async::Loop loop_;
  int instance_count_;
  char instance_name_[64];
};

}  // namespace virtual_audio

#endif  // SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_DAI_H_
