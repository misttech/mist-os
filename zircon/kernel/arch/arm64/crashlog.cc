// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <stdio.h>

#include <arch/crashlog.h>

void arch_render_crashlog_registers(FILE& target, const crashlog_regs_t& regs) {
  if (regs.iframe != nullptr) {
    fprintf(&target,
            // clang-format off
            "REGISTERS (v1.0)\n"
            "  x0: %#18" PRIx64 "\n"
            "  x1: %#18" PRIx64 "\n"
            "  x2: %#18" PRIx64 "\n"
            "  x3: %#18" PRIx64 "\n"
            "  x4: %#18" PRIx64 "\n"
            "  x5: %#18" PRIx64 "\n"
            "  x6: %#18" PRIx64 "\n"
            "  x7: %#18" PRIx64 "\n"
            "  x8: %#18" PRIx64 "\n"
            "  x9: %#18" PRIx64 "\n"
            " x10: %#18" PRIx64 "\n"
            " x11: %#18" PRIx64 "\n"
            " x12: %#18" PRIx64 "\n"
            " x13: %#18" PRIx64 "\n"
            " x14: %#18" PRIx64 "\n"
            " x15: %#18" PRIx64 "\n"
            " x16: %#18" PRIx64 "\n"
            " x17: %#18" PRIx64 "\n"
            " x18: %#18" PRIx64 "\n"
            " x19: %#18" PRIx64 "\n"
            " x20: %#18" PRIx64 "\n"
            " x21: %#18" PRIx64 "\n"
            " x22: %#18" PRIx64 "\n"
            " x23: %#18" PRIx64 "\n"
            " x24: %#18" PRIx64 "\n"
            " x25: %#18" PRIx64 "\n"
            " x26: %#18" PRIx64 "\n"
            " x27: %#18" PRIx64 "\n"
            " x28: %#18" PRIx64 "\n"
            " x29: %#18" PRIx64 "\n"
            "  lr: %#18" PRIx64 "\n"
            " usp: %#18" PRIx64 "\n"
            " elr: %#18" PRIx64 "\n"
            "spsr: %#18" PRIx64 "\n"
            " esr: %#18" PRIx32 "\n"
            " far: %#18" PRIx64 "\n"
            "\n",
            // clang-format on
            regs.iframe->r[0], regs.iframe->r[1], regs.iframe->r[2], regs.iframe->r[3],
            regs.iframe->r[4], regs.iframe->r[5], regs.iframe->r[6], regs.iframe->r[7],
            regs.iframe->r[8], regs.iframe->r[9], regs.iframe->r[10], regs.iframe->r[11],
            regs.iframe->r[12], regs.iframe->r[13], regs.iframe->r[14], regs.iframe->r[15],
            regs.iframe->r[16], regs.iframe->r[17], regs.iframe->r[18], regs.iframe->r[19],
            regs.iframe->r[20], regs.iframe->r[21], regs.iframe->r[22], regs.iframe->r[23],
            regs.iframe->r[24], regs.iframe->r[25], regs.iframe->r[26], regs.iframe->r[27],
            regs.iframe->r[28], regs.iframe->r[29], regs.iframe->lr, regs.iframe->usp,
            regs.iframe->elr, regs.iframe->spsr, regs.esr, regs.far);
  } else {
    fprintf(&target, "ARM64 REGISTERS: missing\n");
  }
}
