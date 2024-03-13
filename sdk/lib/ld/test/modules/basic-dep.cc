// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>

#include "test-start.h"

extern "C" int64_t a();

extern "C" int64_t TestStart() { return a() + 4; }
