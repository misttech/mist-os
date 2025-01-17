// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_H_
#define SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_H_

#include <fidl/fuchsia.virtualaudio/cpp/fidl.h>
#include <lib/ddk/device.h>

#include <memory>
#include <unordered_set>

#include <ddktl/device.h>

namespace virtual_audio {

class VirtualAudioDeviceImpl;

class VirtualAudio;
using VirtualAudioDeviceType = ddk::Device<VirtualAudio, ddk::Unbindable,
                                           ddk::Messageable<fuchsia_virtualaudio::Control>::Mixin>;

class VirtualAudio : public VirtualAudioDeviceType {
 public:
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  explicit VirtualAudio(zx_device_t* parent) : VirtualAudioDeviceType(parent) {}

  void DdkRelease();
  void DdkUnbind(ddk::UnbindTxn txn);

 private:
  zx::result<> Init();

  // Implements virtualaudio.Control.
  void GetDefaultConfiguration(GetDefaultConfigurationRequestView request,
                               GetDefaultConfigurationCompleter::Sync& completer) override;
  void AddDevice(AddDeviceRequestView request, AddDeviceCompleter::Sync& completer) override;
  void GetNumDevices(GetNumDevicesCompleter::Sync& completer) override;
  void RemoveAll(RemoveAllCompleter::Sync& completer) override;

  async_dispatcher_t* dispatcher_ = fdf::Dispatcher::GetCurrent()->async_dispatcher();

  std::unordered_set<std::shared_ptr<VirtualAudioDeviceImpl>> devices_;
};

}  // namespace virtual_audio

#endif  // SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_H_
