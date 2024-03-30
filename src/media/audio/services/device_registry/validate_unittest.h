// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_VALIDATE_UNITTEST_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_VALIDATE_UNITTEST_H_

#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>

#include <vector>

namespace media_audio {

const std::vector<uint8_t> kChannels = {1, 8, 255};
const std::vector<std::pair<uint8_t, fuchsia_hardware_audio::SampleFormat>> kFormats = {
    {1, fuchsia_hardware_audio::SampleFormat::kPcmUnsigned},
    {2, fuchsia_hardware_audio::SampleFormat::kPcmSigned},
    {4, fuchsia_hardware_audio::SampleFormat::kPcmSigned},
    {4, fuchsia_hardware_audio::SampleFormat::kPcmFloat},
    {8, fuchsia_hardware_audio::SampleFormat::kPcmFloat},
};
const std::vector<uint32_t> kFrameRates = {1000, 44100, 48000, 19200};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_VALIDATE_UNITTEST_H_
