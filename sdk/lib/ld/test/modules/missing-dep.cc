// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>

#include "test-start.h"

// The .ifs file missing-dep-dep.ifs creates a stub shared object that defines
// the symbol `missing_dep_sym` and specifies it's soname as libmissing_dep.so.
// This module doesn't exist so we expect a missing module error.

extern "C" int64_t missing_dep_sym();

extern "C" int64_t TestStart() { return missing_dep_sym(); }
