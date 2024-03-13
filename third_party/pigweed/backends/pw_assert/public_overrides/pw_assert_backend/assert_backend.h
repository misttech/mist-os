// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef THIRD_PARTY_PIGWEED_BACKENDS_PW_ASSERT_PUBLIC_OVERRIDES_PW_ASSERT_BACKEND_ASSERT_BACKEND_H_
#define THIRD_PARTY_PIGWEED_BACKENDS_PW_ASSERT_PUBLIC_OVERRIDES_PW_ASSERT_BACKEND_ASSERT_BACKEND_H_

#include <zircon/assert.h>

#define PW_ASSERT_HANDLE_FAILURE(condition) ZX_PANIC(condition)

#endif  // THIRD_PARTY_PIGWEED_BACKENDS_PW_ASSERT_PUBLIC_OVERRIDES_PW_ASSERT_BACKEND_ASSERT_BACKEND_H_
