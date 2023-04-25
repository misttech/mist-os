// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "unit-lib.h"

namespace ufs {

using InitTest = UfsTest;

TEST_F(InitTest, Basic) { ASSERT_NO_FATAL_FAILURE(RunInit()); }

}  // namespace ufs
