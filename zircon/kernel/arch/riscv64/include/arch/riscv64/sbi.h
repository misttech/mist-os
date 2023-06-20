// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_SBI_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_SBI_H_

#include <lib/arch/riscv64/sbi-call.h>
#include <lib/arch/riscv64/sbi.h>
#include <stdint.h>

#include <arch/riscv64.h>

// Init functions
void riscv64_sbi_early_init();
void riscv64_sbi_init();

// Various wrappers for SBI routines used in the kernel.
// Mostly follows the SBI api, documented at
// https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/riscv-sbi.adoc
inline arch::RiscvSbiRet sbi_set_timer(uint64_t stime_value) {
  return arch::RiscvSbi::SetTimer(stime_value);
}

arch::RiscvSbiRet sbi_send_ipi(arch::HartMask mask, arch::HartMaskBase mask_base);
arch::RiscvSbiRet sbi_hart_start(arch::HartId hart_id, paddr_t start_addr, uint64_t priv);
arch::RiscvSbiRet sbi_hart_stop();
arch::RiscvSbiRet sbi_remote_sfence_vma(cpu_mask_t cpu_mask, uintptr_t start, uintptr_t size);
arch::RiscvSbiRet sbi_remote_sfence_vma_asid(cpu_mask_t cpu_mask, uintptr_t start, uintptr_t size,
                                             uint64_t asid);

// Attempt to shut down the system via the system reset extension. If it fails,
// will panic instead of returning.
[[noreturn]] void sbi_shutdown();

// Attempt to reset down the system via the system reset extension. If it fails,
// will panic instead of returning.
[[noreturn]] void sbi_reset();

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_SBI_H_
