// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_COMMON_UNITTEST_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_COMMON_UNITTEST_H_

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>

namespace media_audio {

////////////////////////
// Codec-related methods
fuchsia_hardware_audio::DaiFormat SafeDaiFormatFromDaiSupportedFormats(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets);

fuchsia_hardware_audio::DaiFormat SecondDaiFormatFromDaiSupportedFormats(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets);

fuchsia_hardware_audio::DaiFormat UnsupportedDaiFormatFromDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets);

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_COMMON_UNITTEST_H_
