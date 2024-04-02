// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_BASIC_TYPES_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_BASIC_TYPES_H_

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>

#include <array>

namespace media_audio {

// FIDL IDs.
using TokenId = uint64_t;
using ElementId = uint64_t;
using TopologyId = uint64_t;
using ClockDomain = uint32_t;
using UniqueId = std::array<uint8_t, fuchsia_audio_device::kUniqueInstanceIdSize>;

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_BASIC_TYPES_H_
