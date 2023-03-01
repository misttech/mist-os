// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MMU_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MMU_H_

#define MMU_GUEST_SIZE_SHIFT 36

#define MMU_KERNEL_PAGE_SIZE_SHIFT (PAGE_SIZE_SHIFT)
#define MMU_USER_PAGE_SIZE_SHIFT (USER_PAGE_SIZE_SHIFT)
#define MMU_GUEST_PAGE_SIZE_SHIFT (USER_PAGE_SIZE_SHIFT)

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_MMU_H_
