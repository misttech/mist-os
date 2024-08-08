// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>

#include "test-start.h"

// The generated .ifs file for missing-sym-dep-a creates a stub shared object
// that defines the symbol `c` (see //sdk/lib/ld/test/modules:missing-sym-dep-a-ifs).
// At link the time, the linker is satisfied that `c` exists.
// That .ifs file specifies that it has the soname libld-dep-a.so, so the linker
// adds a DT_NEEDED on libld-dep-a.so. That module doesn't define `c`, so at
// runtime there will be a missing symbol error.

extern "C" int64_t c();

extern "C" int64_t TestStart() { return c() + 4; }
