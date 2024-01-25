// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <setjmp.h>
#include <stdint.h>

#include "setjmp_impl.h"

// TODO(https://fxbug.dev/42076381): The size has been expanded to accommodate a checksum
// word, but this is not yet used until callers can be expected to use the new
// larger size.
#define JB_COUNT_UNUSED 1

static_assert(sizeof(__jmp_buf) == sizeof(uint64_t) * (JB_COUNT + JB_COUNT_UNUSED),
              "fix __jmp_buf definition");
