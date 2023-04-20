// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/drivers/virtual_audio/virtual_audio_dai.h"

#include <lib/ddk/debug.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

namespace virtual_audio {

fit::result<VirtualAudioDai::ErrorT, CurrentFormat> VirtualAudioDai::GetFormatForVA() {
  return fit::error(ErrorT::kNotSupported);
}

fit::result<VirtualAudioDai::ErrorT, CurrentGain> VirtualAudioDai::GetGainForVA() {
  return fit::error(ErrorT::kInvalidArgs);
}

fit::result<VirtualAudioDai::ErrorT, CurrentBuffer> VirtualAudioDai::GetBufferForVA() {
  return fit::error(ErrorT::kNotSupported);
}

fit::result<VirtualAudioDai::ErrorT, CurrentPosition> VirtualAudioDai::GetPositionForVA() {
  return fit::error(ErrorT::kNotSupported);
}

fit::result<VirtualAudioDai::ErrorT> VirtualAudioDai::SetNotificationFrequencyFromVA(
    uint32_t notifications_per_ring) {
  return fit::error(ErrorT::kNotSupported);
}

fit::result<VirtualAudioDai::ErrorT> VirtualAudioDai::ChangePlugStateFromVA(bool plugged) {
  return fit::error(ErrorT::kInvalidArgs);
}

fit::result<VirtualAudioDai::ErrorT> VirtualAudioDai::AdjustClockRateFromVA(
    int32_t ppm_from_monotonic) {
  return fit::error(ErrorT::kInvalidArgs);
}

}  // namespace virtual_audio
