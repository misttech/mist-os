// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_EARLY_BOOT_ZBI_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_EARLY_BOOT_ZBI_H_

#include <lib/zbitl/view.h>

#include <ktl/byte.h>
#include <ktl/span.h>

// Represents a data ZBI in early ZBI boot when caches might be left disabled,
// during which access is guaranteed to be coherent with any disabled cache
// state. EarlyBootZbi is stateful and should be passed by reference are moved
// before calling InitMemory(), at which point an instance should be consumed
// with the class no longer being used past that point.
//
// TODO(https://fxbug.dev/408020980): Replace the storage type with one that
// actually ensures this!
using EarlyBootZbi = zbitl::View<ktl::span<const ktl::byte>>;

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_EARLY_BOOT_ZBI_H_
