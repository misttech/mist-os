// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "data-constants.h"
#include "private.h"

__EXPORT zx_status_t _zx_clock_get_details_mapped(const void* clock_addr, uint64_t options,
                                                  void* out_details) {
  return ZX_ERR_NOT_SUPPORTED;
}

VDSO_INTERFACE_FUNCTION(zx_clock_get_details_mapped);
