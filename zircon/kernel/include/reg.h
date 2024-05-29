// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2008 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_REG_H_
#define ZIRCON_KERNEL_INCLUDE_REG_H_

#include <lib/mmio-ptr/mmio-ptr.h>
#include <stdint.h>

// TODO(https://fxbug.dev/42072070): Remaining users should be migrated to more modern
// facilities such as hwreg or mmio-ptr directly.

inline void RMWREG32(volatile void* addr, unsigned int startbit, unsigned int width,
                     uint32_t value) {
  MMIO_PTR volatile uint32_t* ptr =
      reinterpret_cast<MMIO_PTR volatile uint32_t*>(reinterpret_cast<uintptr_t>(addr));
  uint32_t reg = MmioRead32(ptr);
  reg &= ~(((1 << width) - 1) << startbit);
  reg |= (value << startbit);
  MmioWrite32(reg, ptr);
}

inline uint32_t readl(uintptr_t addr) {
  return MmioRead32(reinterpret_cast<MMIO_PTR volatile uint32_t*>(addr));
}

inline void writel(uint32_t value, uintptr_t addr) {
  MmioWrite32(value, reinterpret_cast<MMIO_PTR volatile uint32_t*>(addr));
}

inline void writel(uint32_t value, volatile void* addr) {
  writel(value, reinterpret_cast<uintptr_t>(addr));
}

#endif  // ZIRCON_KERNEL_INCLUDE_REG_H_
