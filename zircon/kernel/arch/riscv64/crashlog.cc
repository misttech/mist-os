// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <stdio.h>

#include <arch/crashlog.h>

void arch_render_crashlog_registers(FILE& target, const crashlog_regs_t& regs) {
  if (regs.iframe == nullptr) {
    fprintf(&target, "RISCV REGISTERS: missing\n");
    return;
  }

  fprintf(&target,
          // clang-format off
          "REGISTERS (v1.0)\n"
          "  pc: %#18" PRIx64 "\n"
          "  ra: %#18" PRIx64 "\n"
          "  sp: %#18" PRIx64 "\n"
          "  gp: %#18" PRIx64 "\n"
          "  tp: %#18" PRIx64 "\n"
          "  t0: %#18" PRIx64 "\n"
          "  t1: %#18" PRIx64 "\n"
          "  t2: %#18" PRIx64 "\n"
          "  s0: %#18" PRIx64 "\n"
          "  s1: %#18" PRIx64 "\n"
          "  a0: %#18" PRIx64 "\n"
          "  a1: %#18" PRIx64 "\n"
          "  a2: %#18" PRIx64 "\n"
          "  a3: %#18" PRIx64 "\n"
          "  a4: %#18" PRIx64 "\n"
          "  a5: %#18" PRIx64 "\n"
          "  a6: %#18" PRIx64 "\n"
          "  a7: %#18" PRIx64 "\n"
          "  s2: %#18" PRIx64 "\n"
          "  s3: %#18" PRIx64 "\n"
          "  s4: %#18" PRIx64 "\n"
          "  s5: %#18" PRIx64 "\n"
          "  s6: %#18" PRIx64 "\n"
          "  s7: %#18" PRIx64 "\n"
          "  s8: %#18" PRIx64 "\n"
          "  s9: %#18" PRIx64 "\n"
          "  s10: %#18" PRIx64 "\n"
          "  s11: %#18" PRIx64 "\n"
          "  t3: %#18" PRIx64 "\n"
          "  t4: %#18" PRIx64 "\n"
          "  t5: %#18" PRIx64 "\n"
          "  t6: %#18" PRIx64 "\n"
          " status: %#18" PRIx64 "\n"
          " cause: %18" PRIi64 "\n"
          " tval: %#18" PRIx64 "\n",
          // clang-format on
          regs.iframe->regs.pc, regs.iframe->regs.ra, regs.iframe->regs.sp, regs.iframe->regs.gp,
          regs.iframe->regs.tp, regs.iframe->regs.t0, regs.iframe->regs.t1, regs.iframe->regs.t2,
          regs.iframe->regs.s0, regs.iframe->regs.s1, regs.iframe->regs.a0, regs.iframe->regs.a1,
          regs.iframe->regs.a2, regs.iframe->regs.a3, regs.iframe->regs.a4, regs.iframe->regs.a5,
          regs.iframe->regs.a6, regs.iframe->regs.a7, regs.iframe->regs.s2, regs.iframe->regs.s3,
          regs.iframe->regs.s4, regs.iframe->regs.s5, regs.iframe->regs.s6, regs.iframe->regs.s7,
          regs.iframe->regs.s8, regs.iframe->regs.s9, regs.iframe->regs.s10, regs.iframe->regs.s11,
          regs.iframe->regs.t3, regs.iframe->regs.t4, regs.iframe->regs.t5, regs.iframe->regs.t6,
          regs.iframe->status, regs.cause, regs.tval);
}
