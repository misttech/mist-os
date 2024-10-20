// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_HANDLE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_HANDLE_H_

#include <lib/mistos/zx/object.h>

namespace zx {

using handle = object<void>;
using unowned_handle = unowned<handle>;

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_HANDLE_H_
