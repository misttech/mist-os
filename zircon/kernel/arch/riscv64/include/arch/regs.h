// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_REGS_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_REGS_H_

#define RISCV64_IFRAME_OFFSET_PC (0 * 8)    // in the x0 slot
#define RISCV64_IFRAME_OFFSET_RA (1 * 8)    // x1
#define RISCV64_IFRAME_OFFSET_SP (2 * 8)    // x2
#define RISCV64_IFRAME_OFFSET_GP (3 * 8)    // x3
#define RISCV64_IFRAME_OFFSET_TP (4 * 8)    // x4
#define RISCV64_IFRAME_OFFSET_T0 (5 * 8)    // x5
#define RISCV64_IFRAME_OFFSET_T1 (6 * 8)    // x6
#define RISCV64_IFRAME_OFFSET_T2 (7 * 8)    // x7
#define RISCV64_IFRAME_OFFSET_S0 (8 * 8)    // x8
#define RISCV64_IFRAME_OFFSET_S1 (9 * 8)    // x9
#define RISCV64_IFRAME_OFFSET_A0 (10 * 8)   // x10
#define RISCV64_IFRAME_OFFSET_A1 (11 * 8)   // x11
#define RISCV64_IFRAME_OFFSET_A2 (12 * 8)   // x12
#define RISCV64_IFRAME_OFFSET_A3 (13 * 8)   // x13
#define RISCV64_IFRAME_OFFSET_A4 (14 * 8)   // x14
#define RISCV64_IFRAME_OFFSET_A5 (15 * 8)   // x15
#define RISCV64_IFRAME_OFFSET_A6 (16 * 8)   // x16
#define RISCV64_IFRAME_OFFSET_A7 (17 * 8)   // x17
#define RISCV64_IFRAME_OFFSET_S2 (18 * 8)   // x18
#define RISCV64_IFRAME_OFFSET_S3 (19 * 8)   // x19
#define RISCV64_IFRAME_OFFSET_S4 (20 * 8)   // x20
#define RISCV64_IFRAME_OFFSET_S5 (21 * 8)   // x21
#define RISCV64_IFRAME_OFFSET_S6 (22 * 8)   // x22
#define RISCV64_IFRAME_OFFSET_S7 (23 * 8)   // x23
#define RISCV64_IFRAME_OFFSET_S8 (24 * 8)   // x24
#define RISCV64_IFRAME_OFFSET_S9 (25 * 8)   // x25
#define RISCV64_IFRAME_OFFSET_S10 (26 * 8)  // x26
#define RISCV64_IFRAME_OFFSET_S11 (27 * 8)  // x27
#define RISCV64_IFRAME_OFFSET_T3 (28 * 8)   // x28
#define RISCV64_IFRAME_OFFSET_T4 (29 * 8)   // x29
#define RISCV64_IFRAME_OFFSET_T5 (30 * 8)   // x30
#define RISCV64_IFRAME_OFFSET_T6 (31 * 8)   // x31
#define RISCV64_IFRAME_OFFSET_STATUS (32 * 8)
#define RISCV64_IFRAME_SIZE ((32 + 2) * 8)

#ifndef __ASSEMBLER__

#include <stdint.h>
#include <stdio.h>
#include <zircon/syscalls/debug.h>

// Registers saved on entering the kernel via architectural exception.
struct alignas(16) iframe_t {
  zx_riscv64_thread_state_general_regs_t regs;
  uint64_t status;
  uint64_t pad;
};

struct arch_exception_context {
  iframe_t* frame;
  uint64_t scause;
  uint32_t user_synth_code;
  uint32_t user_synth_data;
  bool is_page_fault;
};

static_assert(sizeof(iframe_t) % 16u == 0u);
static_assert(sizeof(iframe_t) == RISCV64_IFRAME_SIZE);

static_assert(__offsetof(iframe_t, regs.pc) == RISCV64_IFRAME_OFFSET_PC);
static_assert(__offsetof(iframe_t, regs.ra) == RISCV64_IFRAME_OFFSET_RA);
static_assert(__offsetof(iframe_t, regs.sp) == RISCV64_IFRAME_OFFSET_SP);
static_assert(__offsetof(iframe_t, regs.gp) == RISCV64_IFRAME_OFFSET_GP);
static_assert(__offsetof(iframe_t, regs.tp) == RISCV64_IFRAME_OFFSET_TP);
static_assert(__offsetof(iframe_t, regs.t0) == RISCV64_IFRAME_OFFSET_T0);
static_assert(__offsetof(iframe_t, regs.t1) == RISCV64_IFRAME_OFFSET_T1);
static_assert(__offsetof(iframe_t, regs.t2) == RISCV64_IFRAME_OFFSET_T2);
static_assert(__offsetof(iframe_t, regs.s0) == RISCV64_IFRAME_OFFSET_S0);
static_assert(__offsetof(iframe_t, regs.s1) == RISCV64_IFRAME_OFFSET_S1);
static_assert(__offsetof(iframe_t, regs.a0) == RISCV64_IFRAME_OFFSET_A0);
static_assert(__offsetof(iframe_t, regs.a1) == RISCV64_IFRAME_OFFSET_A1);
static_assert(__offsetof(iframe_t, regs.a2) == RISCV64_IFRAME_OFFSET_A2);
static_assert(__offsetof(iframe_t, regs.a3) == RISCV64_IFRAME_OFFSET_A3);
static_assert(__offsetof(iframe_t, regs.a4) == RISCV64_IFRAME_OFFSET_A4);
static_assert(__offsetof(iframe_t, regs.a5) == RISCV64_IFRAME_OFFSET_A5);
static_assert(__offsetof(iframe_t, regs.a6) == RISCV64_IFRAME_OFFSET_A6);
static_assert(__offsetof(iframe_t, regs.a7) == RISCV64_IFRAME_OFFSET_A7);
static_assert(__offsetof(iframe_t, regs.s2) == RISCV64_IFRAME_OFFSET_S2);
static_assert(__offsetof(iframe_t, regs.s3) == RISCV64_IFRAME_OFFSET_S3);
static_assert(__offsetof(iframe_t, regs.s4) == RISCV64_IFRAME_OFFSET_S4);
static_assert(__offsetof(iframe_t, regs.s5) == RISCV64_IFRAME_OFFSET_S5);
static_assert(__offsetof(iframe_t, regs.s6) == RISCV64_IFRAME_OFFSET_S6);
static_assert(__offsetof(iframe_t, regs.s7) == RISCV64_IFRAME_OFFSET_S7);
static_assert(__offsetof(iframe_t, regs.s8) == RISCV64_IFRAME_OFFSET_S8);
static_assert(__offsetof(iframe_t, regs.s9) == RISCV64_IFRAME_OFFSET_S9);
static_assert(__offsetof(iframe_t, regs.s10) == RISCV64_IFRAME_OFFSET_S10);
static_assert(__offsetof(iframe_t, regs.s11) == RISCV64_IFRAME_OFFSET_S11);
static_assert(__offsetof(iframe_t, regs.t3) == RISCV64_IFRAME_OFFSET_T3);
static_assert(__offsetof(iframe_t, regs.t4) == RISCV64_IFRAME_OFFSET_T4);
static_assert(__offsetof(iframe_t, regs.t5) == RISCV64_IFRAME_OFFSET_T5);
static_assert(__offsetof(iframe_t, regs.t6) == RISCV64_IFRAME_OFFSET_T6);
static_assert(__offsetof(iframe_t, status) == RISCV64_IFRAME_OFFSET_STATUS);

// Registers saved on entering the kernel via syscall.
using syscall_regs_t = iframe_t;

void PrintFrame(FILE* file, const iframe_t& frame);

#endif  // !__ASSEMBLER__

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_REGS_H_
