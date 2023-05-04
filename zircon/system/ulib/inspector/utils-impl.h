// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#pragma once

#include <stddef.h>
#include <zircon/types.h>

#define MAX_BUILDID_SIZE 64

namespace inspector {

extern void do_print_zx_error(const char* file, int line, const char* what, zx_status_t status);

#define print_zx_error(what, status)                                                              \
  do {                                                                                            \
    ::inspector::do_print_zx_error(__FILE__, __LINE__, (what), static_cast<zx_status_t>(status)); \
  } while (0)

}  // namespace inspector
