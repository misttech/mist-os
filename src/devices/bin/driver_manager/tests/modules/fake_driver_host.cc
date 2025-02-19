// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "deps.h"
#include "entry_point.h"

extern "C" int64_t Start(zx_handle_t bootstrap, void* vdso) {
  if (a() == 13) {
    return 0;
  }
  return -1;
}
