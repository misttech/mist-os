// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_SYSCALLS_PRIV_H_
#define ZIRCON_KERNEL_LIB_MISTOS_SYSCALLS_PRIV_H_

// Individual files do `#define LOCAL_TRACE SYSCALLS_GLOBAL_TRACE(0)`.
// This lets one edit just that one file to switch that `0` to `1`,
// or edit this file to replace `local` with `1` and get all those
// files at once.
#define SYSCALLS_GLOBAL_TRACE(local) (local | 0)

#endif  // ZIRCON_KERNEL_LIB_MISTOS_SYSCALLS_PRIV_H_
