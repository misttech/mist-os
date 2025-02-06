// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_DFV2_VIRTUAL_AUDIO_COMPOSITE_H_
#define SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_DFV2_VIRTUAL_AUDIO_COMPOSITE_H_

#include <fidl/fuchsia.virtualaudio/cpp/fidl.h>

namespace virtual_audio {

class VirtualAudioComposite {
 public:
  static fuchsia_virtualaudio::Configuration GetDefaultConfig();
};

}  // namespace virtual_audio

#endif  // SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_DFV2_VIRTUAL_AUDIO_COMPOSITE_H_
