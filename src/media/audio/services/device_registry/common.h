// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_COMMON_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_COMMON_H_

#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>

#include <vector>

#include "src/media/audio/services/device_registry/basic_types.h"

namespace media_audio {

bool DaiFormatIsSupported(
    ElementId element_id,
    const std::vector<fuchsia_audio_device::ElementDaiFormatSet>& element_dai_format_sets,
    const fuchsia_hardware_audio::DaiFormat& format);

bool RingBufferFormatIsSupported(
    ElementId element_id,
    const std::vector<fuchsia_audio_device::ElementRingBufferFormatSet>&
        element_ring_buffer_format_sets,
    const fuchsia_hardware_audio::Format& format);

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_COMMON_H_
