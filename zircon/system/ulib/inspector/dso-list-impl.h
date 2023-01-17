// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#pragma once

#include <lib/stdcompat/span.h>
#include <stdio.h>
#include <zircon/types.h>

#include "utils-impl.h"

struct inspector_dsoinfo {
  struct inspector_dsoinfo* next;
  zx_vaddr_t base;
  char buildid[MAX_BUILDID_SIZE * 2 + 1];
  bool debug_file_tried;
  zx_status_t debug_file_status;
  char* debug_file;
  char name[];
};

namespace inspector {

// Writes markup context to |f|. If |pcs| is empty, context for all modules will be written.
// Otherwise, context for a module will only be printed if one of the addresses in |pcs| lies within
// the bounds of the module.
void print_markup_context(FILE* f, zx_handle_t process, cpp20::span<uint64_t> pcs);

}  // namespace inspector
