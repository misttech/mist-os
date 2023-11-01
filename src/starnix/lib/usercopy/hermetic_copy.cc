// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "hermetic_copy.h"

[[gnu::section(".text.entry")]] uintptr_t hermetic_copy(void* dest, void* source, size_t len) {
  for (size_t i = 0; i < len; ++i) {
    reinterpret_cast<char*>(dest)[i] = reinterpret_cast<char*>(source)[i];
  }
  return 0;
}
