// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_OBSERVER_NOTIFY_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_OBSERVER_NOTIFY_H_

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>

namespace media_audio {

class ObserverNotify {
 public:
  virtual void DeviceIsRemoved() = 0;
  virtual void DeviceHasError() = 0;

  virtual void GainStateChanged(const fuchsia_audio_device::GainState&) = 0;
  virtual void PlugStateChanged(const fuchsia_audio_device::PlugState& new_plug_state,
                                zx::time plug_change_time) = 0;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_OBSERVER_NOTIFY_H_
