// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_ASM_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_ASM_H_

// The offset from the physhandoff pointer to the boot hart id field, usable
// from early kernel assembly.
#define RISCV64_PHYSHANDOFF_BOOT_HART_ID_OFFSET 136

#ifndef __ASSEMBLER__

#include <phys/handoff.h>

static_assert(RISCV64_PHYSHANDOFF_BOOT_HART_ID_OFFSET ==
              offsetof(PhysHandoff, arch_handoff.boot_hart_id));

#endif  // !__ASSEMBLER__

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_PHYS_INCLUDE_PHYS_ARCH_ARCH_HANDOFF_ASM_H_
