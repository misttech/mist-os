// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_SYSCALLS_INCLUDE_LIB_MISTOS_UTIL_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_SYSCALLS_INCLUDE_LIB_MISTOS_UTIL_H_

#include <object/dispatcher.h>
#include <object/handle.h>
#include <object/handle_table.h>

HandleTable& handle_table(ProcessDispatcher* dispatcher);

// Allocates a handle with the given rights to the given dispatcher. The handle is added to the
// calling process' handle table, and its value is returned in out.
zx_status_t MakeAndAddHandle(fbl::RefPtr<Dispatcher> dispatcher, zx_rights_t rights,
                             zx_handle_t* out);
// Allocates a handle with the given rights to the dispatcher enclosed in the given kernel handle.
// The handle is added to the calling process' handle table, and its value is returned in out.
zx_status_t MakeAndAddHandle(KernelHandle<Dispatcher> kernel_handle, zx_rights_t rights,
                             zx_handle_t* out);

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_SYSCALLS_INCLUDE_LIB_MISTOS_UTIL_H_
