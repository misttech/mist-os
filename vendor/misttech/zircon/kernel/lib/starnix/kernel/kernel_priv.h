// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2014 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_KERNEL_PRIV_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_KERNEL_PRIV_H_

// Individual files do `#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)`.
// This lets one edit just that one file to switch that `0` to `1`,
// or edit this file to replace `local` with `1` and get all those
// files at once.
#define STARNIX_KERNEL_GLOBAL_TRACE(local) (local | 0)

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_KERNEL_PRIV_H_
