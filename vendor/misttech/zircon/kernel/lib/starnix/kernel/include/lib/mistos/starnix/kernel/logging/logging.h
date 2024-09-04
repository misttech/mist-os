// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_LOGGING_LOGGING_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_LOGGING_LOGGING_H_

#include <lib/mistos/starnix_uapi/errors.h>
#include <zircon/types.h>

#include <ktl/span.h>
#include <object/vm_object_dispatcher.h>

namespace starnix {

// Call this when you get an error that should "never" happen, i.e. if it does that means the
// kernel was updated to produce some other error after this match was written.
Errno impossible_error(zx_status_t status);

void set_zx_name(fbl::RefPtr<VmObjectDispatcher> obj, const ktl::span<const uint8_t>& name);

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_LOGGING_LOGGING_H_
