// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/util/system.h"

#include <zircon/limits.h>

uint32_t zx_system_get_page_size(void) { return ZX_PAGE_SIZE; }
