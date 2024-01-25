// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "lib.h"

TEST(RestrictedModeUnifiedStress, SharedRegionTestShort) {
  Orchestrator(zx::sec(10), zx::sec(1), TestMode::Shared);
}

// TODO(https://fxbug.dev/42083004): Enable on other architectures as they support unified aspaces.
#if defined(__x86_64__) || defined(__aarch64__)
TEST(RestrictedModeUnifiedStress, RestrictedRegionTestShort) {
  Orchestrator(zx::sec(10), zx::sec(1), TestMode::Restricted);
}
#endif  // defined(__x86_64__) || defined(__aarch64__)
