// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_HANDOFF_INCLUDE_LIB_HANDOFF_HANDOFF_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_HANDOFF_INCLUDE_LIB_HANDOFF_HANDOFF_H_

#include <lib/mistos/zx/vmo.h>

zx::unowned_vmo GetZbi();

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_HANDOFF_INCLUDE_LIB_HANDOFF_HANDOFF_H_
