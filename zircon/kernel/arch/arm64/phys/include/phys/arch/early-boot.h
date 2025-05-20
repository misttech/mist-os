// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_ARM64_PHYS_INCLUDE_PHYS_ARCH_EARLY_BOOT_H_
#define ZIRCON_KERNEL_ARCH_ARM64_PHYS_INCLUDE_PHYS_ARCH_EARLY_BOOT_H_

#include <cstddef>
#include <cstdint>

// Synchronize a data range between the data cache and main memory. Intended to
// be used in early boot while the data cache is off (i.e., before
// InitMemory()), ensuring that reads are coherent with those after caches are
// re-enabled.
inline void ArchEarlyBootSyncData(uintptr_t addr, size_t size) {
  // TODO(https://fxbug.dev/408020980): Actually clean this range in the data
  // cache.
}

// Whether any early boot data access synchronization scheme is necessary,
// equivalent to the above routine not being a no-op.
constexpr bool kArchEarlyBootDataSynchronization = true;

#endif  // ZIRCON_KERNEL_ARCH_ARM64_PHYS_INCLUDE_PHYS_ARCH_EARLY_BOOT_H_
