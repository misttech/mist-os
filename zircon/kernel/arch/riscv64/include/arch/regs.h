// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_REGS_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_REGS_H_

#ifndef __ASSEMBLER__

#include <stdint.h>
#include <stdio.h>
#include <zircon/syscalls/debug.h>

// Registers saved on entering the kernel via architectural exception.
struct alignas(16) iframe_t {
  zx_riscv64_thread_state_general_regs_t regs;
  uint64_t sstatus;
};

struct arch_exception_context {
  iframe_t* frame;
  uint64_t scause;
  uint32_t user_synth_code;
  uint32_t user_synth_data;
  bool is_page_fault;
};

void PrintFrame(FILE* file, const iframe_t& frame);

// Registers saved on entering the kernel via syscall.
using syscall_regs_t = iframe_t;

#endif  // !__ASSEMBLER__

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_REGS_H_
