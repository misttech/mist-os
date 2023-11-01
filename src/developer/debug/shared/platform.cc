// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/shared/platform.h"

namespace debug {

Platform CurrentSystemPlatform() {
#if defined(__Fuchsia__)
  return Platform::kFuchsia;
#elif defined(__linux__)
  return Platform::kLinux;
#elif defined(__APPLE__)
  return Platform::kMac;
#else
#error Need to define your platform.
#endif
}

const char* PlatformToString(Platform p) {
  switch (p) {
    case Platform::kUnknown:
      return "Unknown platform";
    case Platform::kFuchsia:
      return "Fuchsia";
    case Platform::kLinux:
      return "Linux";
    case Platform::kMac:
      return "Mac";
  }
  return "Invalid platform";
}

}  // namespace debug
