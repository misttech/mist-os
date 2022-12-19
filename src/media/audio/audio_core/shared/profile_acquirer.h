// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_SHARED_PROFILE_ACQUIRER_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_SHARED_PROFILE_ACQUIRER_H_

#include <lib/zx/profile.h>
#include <lib/zx/result.h>
#include <stdint.h>

#include "src/media/audio/audio_core/shared/mix_profile_config.h"

namespace media::audio {

zx::result<zx::profile> AcquireHighPriorityProfile(const MixProfileConfig& mix_profile_config);
zx::result<zx::profile> AcquireAudioCoreImplProfile();
zx::result<zx::profile> AcquireRelativePriorityProfile(uint32_t priority);

}  // namespace media::audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_SHARED_PROFILE_ACQUIRER_H_
