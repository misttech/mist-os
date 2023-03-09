// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_SHARED_PROFILE_ACQUIRER_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_SHARED_PROFILE_ACQUIRER_H_

#include <lib/zx/result.h>
#include <lib/zx/thread.h>
#include <stdint.h>

#include <string>

namespace media::audio {

zx::result<> AcquireSchedulerRole(zx::unowned_thread thread, const std::string& role);

}  // namespace media::audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_SHARED_PROFILE_ACQUIRER_H_
