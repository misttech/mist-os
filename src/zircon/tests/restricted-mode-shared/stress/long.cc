// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "lib.h"

TEST(RestrictedModeUnifiedStress, SharedRegionTestLong) {
  Orchestrator(zx::sec(3600), zx::sec(30), TestMode::Shared);
}

// TODO(https://fxbug.dev/42083004): Enable on other architectures as they support unified aspaces.
#if defined(__x86_64__) || defined(__aarch64__)
TEST(RestrictedModeUnifiedStress, RestrictedRegionTestLong) {
  Orchestrator(zx::sec(3600), zx::sec(30), TestMode::Restricted);
}
#endif  // defined(__x86_64__) || defined(__aarch64__)
