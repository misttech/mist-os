// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_COMMON_UNITTEST_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_COMMON_UNITTEST_H_

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>

#include "src/media/audio/services/device_registry/basic_types.h"

namespace media_audio {

fuchsia_hardware_audio::DaiFormat SafeDaiFormatFromElementDaiFormatSets(
    ElementId element_id,
    const std::vector<fuchsia_audio_device::ElementDaiFormatSet>& element_dai_format_sets);
fuchsia_hardware_audio::DaiFormat SafeDaiFormatFromDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets);

fuchsia_hardware_audio::DaiFormat SecondDaiFormatFromElementDaiFormatSets(
    ElementId element_id,
    const std::vector<fuchsia_audio_device::ElementDaiFormatSet>& element_dai_format_sets);
fuchsia_hardware_audio::DaiFormat SecondDaiFormatFromDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets);

fuchsia_hardware_audio::DaiFormat UnsupportedDaiFormatFromElementDaiFormatSets(
    ElementId element_id,
    const std::vector<fuchsia_audio_device::ElementDaiFormatSet>& element_dai_format_sets);
fuchsia_hardware_audio::DaiFormat UnsupportedDaiFormatFromDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets);

fuchsia_audio::Format SafeRingBufferFormatFromElementRingBufferFormatSets(
    ElementId element_id, const std::vector<fuchsia_audio_device::ElementRingBufferFormatSet>&
                              element_ring_buffer_format_sets);
fuchsia_audio::Format SafeRingBufferFormatFromRingBufferFormatSets(
    const std::vector<fuchsia_audio_device::PcmFormatSet>& ring_buffer_format_sets);

fuchsia_hardware_audio::Format SafeDriverRingBufferFormatFromDriverRingBufferFormatSets(
    const std::vector<fuchsia_hardware_audio::SupportedFormats>& driver_ring_buffer_format_sets);
fuchsia_hardware_audio::Format SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
    ElementId element_id,
    const std::vector<std::pair<ElementId, std::vector<fuchsia_hardware_audio::SupportedFormats>>>&
        element_driver_ring_buffer_format_sets);

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_COMMON_UNITTEST_H_
