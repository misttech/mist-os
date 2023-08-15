// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include <bits.h>
#include <debug.h>
#include <inttypes.h>
#include <lib/counters.h>
#include <lib/crashlog.h>
#include <platform.h>
#include <stdio.h>
#include <trace.h>
#include <zircon/syscalls/exception.h>
#include <zircon/types.h>

#include <arch/arch_ops.h>
#include <arch/crashlog.h>
#include <arch/exception.h>
#include <arch/regs.h>
#include <arch/riscv64/user_copy.h>
#include <arch/thread.h>
#include <arch/user_copy.h>
#include <kernel/interrupt.h>
#include <kernel/thread.h>
#include <pretty/hexdump.h>
#include <syscalls/syscalls.h>
#include <vm/fault.h>
#include <vm/vm.h>

#define LOCAL_TRACE 0

namespace {

const char* cause_to_string(int64_t cause) {
  if (cause < 0) {
    switch (cause & LONG_MAX) {
      case RISCV64_INTERRUPT_SSWI:
        return "Software interrupt";
      case RISCV64_INTERRUPT_STIM:
        return "Timer interrupt";
      case RISCV64_INTERRUPT_SEXT:
        return "External interrupt";
    }
  } else {
    switch (cause) {
      case RISCV64_EXCEPTION_IADDR_MISALIGN:
        return "Instruction address misaligned";
      case RISCV64_EXCEPTION_IACCESS_FAULT:
        return "Instruction access fault";
      case RISCV64_EXCEPTION_ILLEGAL_INS:
        return "Illegal instruction";
      case RISCV64_EXCEPTION_BREAKPOINT:
        return "Breakpoint";
      case RISCV64_EXCEPTION_LOAD_ADDR_MISALIGN:
        return "Load address misaligned";
      case RISCV64_EXCEPTION_LOAD_ACCESS_FAULT:
        return "Load access fault";
      case RISCV64_EXCEPTION_STORE_ADDR_MISALIGN:
        return "Store/AMO address misaligned";
      case RISCV64_EXCEPTION_STORE_ACCESS_FAULT:
        return "Store/AMO access fault";
      case RISCV64_EXCEPTION_ENV_CALL_U_MODE:
        return "Environment call from U-mode";
      case RISCV64_EXCEPTION_ENV_CALL_S_MODE:
        return "Environment call from S-mode";
      case RISCV64_EXCEPTION_ENV_CALL_M_MODE:
        return "Environment call from M-mode";
      case RISCV64_EXCEPTION_INS_PAGE_FAULT:
        return "Instruction page fault";
      case RISCV64_EXCEPTION_LOAD_PAGE_FAULT:
        return "Load page fault";
      case RISCV64_EXCEPTION_STORE_PAGE_FAULT:
        return "Store/AMO page fault";
    }
  }
  return "Unknown";
}

// Prints exception details and then panics.
__NO_RETURN __NO_INLINE void exception_die(iframe_t* iframe, int64_t cause, uint64_t tval,
                                           const char* format, ...) {
  platform_panic_start();

  va_list args;
  va_start(args, format);
  vprintf(format, args);
  va_end(args);

  // Print the interrupt frame and some additional details.
  PrintFrame(stdout, *iframe);
  printf("cause  %18" PRIi64 " %s\n", cause, cause_to_string(cause));
  printf("tval   %#18" PRIx64 "\n", tval);
  Thread::Current::Get()->stack().DumpInfo(CRITICAL);

  // Fill in the crashlog.
  g_crashlog.regs.iframe = iframe;
  g_crashlog.regs.cause = cause;
  g_crashlog.regs.tval = tval;

  platform_halt(HALT_ACTION_HALT, ZirconCrashReason::Panic);
}

__NO_RETURN __NO_INLINE void fatal_exception(int64_t cause, uint64_t tval, struct iframe_t* frame) {
  if (cause < 0) {
    exception_die(frame, cause, tval,
                  "unhandled interrupt cause %#lx, epc %#lx, tval %#lx cpu %u\n", frame->regs.pc,
                  riscv64_csr_read(RISCV64_CSR_STVAL), arch_curr_cpu_num());
  } else {
    exception_die(frame, cause, tval,
                  "unhandled exception cause %#lx (%s), epc %#lx, tval %#lx, cpu %u\n", cause,
                  cause_to_string(cause), frame->regs.pc, riscv64_csr_read(RISCV64_CSR_STVAL),
                  arch_curr_cpu_num());
  }
}

zx_status_t try_dispatch_user_exception(zx_excp_type_t type, int64_t cause, uint64_t tval,
                                        iframe_t* frame, uint32_t error_code) {
  // Interrupts need to be disabled from the original exception upon entry
  DEBUG_ASSERT(arch_ints_disabled());

  arch_exception_context_t context = {};

  context.frame = frame;
  context.cause = cause;
  context.tval = tval;
  context.user_synth_code = error_code;
  context.user_synth_data = 0;

  arch_enable_ints();
  zx_status_t status = dispatch_user_exception(type, &context);
  arch_disable_ints();
  return status;
}

void riscv64_page_fault_handler(int64_t cause, uint64_t tval, struct iframe_t* frame, bool user) {
  // TODO-rvbringup: deal with the fact that riscv exceptions don't specify if it's a permission
  // or page-not-present failure.
  uint pf_flags = VMM_PF_FLAG_NOT_PRESENT;
  pf_flags |= (cause == RISCV64_EXCEPTION_STORE_PAGE_FAULT) ? VMM_PF_FLAG_WRITE : 0;
  pf_flags |= (cause == RISCV64_EXCEPTION_INS_PAGE_FAULT) ? VMM_PF_FLAG_INSTRUCTION : 0;
  pf_flags |= user ? VMM_PF_FLAG_USER : 0;

  LTRACEF("Page fault: %s %s %s address %#lx\n", (pf_flags & VMM_PF_FLAG_USER) ? "user" : "kernel",
          (pf_flags & VMM_PF_FLAG_INSTRUCTION) ? "instruction" : "data",
          (pf_flags & VMM_PF_FLAG_WRITE) ? "write" : "read", tval);

  uint64_t dfr = Thread::Current::Get()->arch().data_fault_resume;
  if (unlikely(!user) && unlikely(!dfr)) {
    // Any page fault in kernel mode that's not during user-copy is a bug.
    exception_die(frame, cause, tval, "Page fault in kernel: %s %s address %#lx\n",
                  (pf_flags & VMM_PF_FLAG_INSTRUCTION) ? "instruction" : "data",
                  (pf_flags & VMM_PF_FLAG_WRITE) ? "write" : "read", tval);
  }

  // Check if the current thread was expecting a data fault and we should return to its handler.
  // If the capture bit was set we should run the dfr routine first before calling the page fault
  // handler along with captured data about the exception.
  if (unlikely(dfr & RISCV_CAPTURE_USER_COPY_FAULTS_BIT)) {
    LTRACEF("DFR is set with capture: pc %#lx tval %#lx flags %#x\n", dfr, tval, pf_flags);
    frame->regs.pc = dfr & ~RISCV_CAPTURE_USER_COPY_FAULTS_BIT;
    frame->regs.a1 = tval;
    frame->regs.a2 = pf_flags;
    return;
  }

  arch_enable_ints();
  CPU_STATS_INC(page_faults);
  zx_status_t pf_status = vmm_page_fault_handler(tval, pf_flags);
  arch_disable_ints();
  if (pf_status == ZX_OK) {
    return;
  }

  // Check again that the data fault handler should be run, this time without the captured data.
  if (unlikely(dfr)) {
    LTRACEF("DFR is set without capture: pc %#lx tval %#lx flags %#x\n", dfr, tval, pf_flags);
    DEBUG_ASSERT((dfr & RISCV_CAPTURE_USER_COPY_FAULTS_BIT) == 0);
    frame->regs.pc = dfr;
    return;
  }

  // If this is from user space, let the user exception handler get a shot at it.
  if (user) {
    if (try_dispatch_user_exception(ZX_EXCP_FATAL_PAGE_FAULT, cause, tval, frame,
                                    static_cast<uint32_t>(pf_status)) == ZX_OK) {
      return;
    }
  }

  exception_die(frame, cause, tval, "Page fault: %s %s %s address %#lx\n",
                (pf_flags & VMM_PF_FLAG_USER) ? "user" : "kernel",
                (pf_flags & VMM_PF_FLAG_INSTRUCTION) ? "instruction" : "data",
                (pf_flags & VMM_PF_FLAG_WRITE) ? "write" : "read", tval);
}

void riscv64_misaligned_fault_handler(int64_t cause, uint64_t tval, struct iframe_t* frame,
                                      bool user) {
  if (!user) {
    // Trapped inside the kernel, this is bad.
    exception_die(frame, cause, tval, "misaligned exception in kernel: PC at %#" PRIx64 "\n",
                  frame->regs.pc);
  }

  try_dispatch_user_exception(ZX_EXCP_UNALIGNED_ACCESS, cause, tval, frame, 0);
}

void riscv64_illegal_instruction_handler(int64_t cause, uint64_t tval, struct iframe_t* frame,
                                         bool user) {
  if (!user) {
    // Trapped inside the kernel, this is bad.
    exception_die(frame, cause, tval,
                  "illegal instruction exception in kernel: PC at %#" PRIx64 "\n", frame->regs.pc);
  }
  try_dispatch_user_exception(ZX_EXCP_UNDEFINED_INSTRUCTION, cause, tval, frame, 0);
}

void riscv64_breakpoint_handler(int64_t cause, uint64_t tval, struct iframe_t* frame, bool user) {
  if (!user) {
    // Trapped inside the kernel, this is bad.
    exception_die(frame, cause, tval,
                  "ebreak instruction exception in kernel: PC at %#" PRIx64 "\n", frame->regs.pc);
  }
  try_dispatch_user_exception(ZX_EXCP_SW_BREAKPOINT, cause, tval, frame, 0);
}

void riscv64_syscall_handler(struct iframe_t* frame) {
  // Push the PC forward over the ECALL instruction. By definition the ECALL instruction
  // is 32 bits wide, and cannot be implemented as a compressed 16 bit instruction.
  frame->regs.pc += 0x4;

  syscall_result ret = riscv64_syscall_dispatcher(frame);
  frame->regs.a0 = ret.status;
  if (ret.is_signaled) {
    Thread::Current::ProcessPendingSignals(GeneralRegsSource::Iframe, frame);
  }
}

}  // namespace

extern "C" void riscv64_exception_handler(int64_t cause, struct iframe_t* frame) {
  const bool user = (frame->status & RISCV64_CSR_SSTATUS_PP) == 0;

  LTRACEF("hart %u cause %s epc %#lx status %#lx user %u\n", arch_curr_cpu_num(),
          cause_to_string(cause), frame->regs.pc, frame->status, user);

  // Some basic state checks of the current status register
  uint64_t status = riscv64_csr_read(sstatus);
  DEBUG_ASSERT((status & RISCV64_CSR_SSTATUS_IE) == 0);
  DEBUG_ASSERT((status & RISCV64_CSR_SSTATUS_UBE) == 0);
  DEBUG_ASSERT((status & RISCV64_CSR_SSTATUS_SUM) == 0);
  DEBUG_ASSERT((status & RISCV64_CSR_SSTATUS_MXR) == 0);

  // TODO-rvbringup: add some kcounters

  bool do_preempt = false;

  // top bit of the cause register determines if it's an interrupt or not
  if (cause < 0) {
    int_handler_saved_state_t state;
    int_handler_start(&state);

    switch (cause & INT64_MAX) {
      case RISCV64_INTERRUPT_SSWI:  // software interrupt
        riscv64_software_exception();
        break;
      case RISCV64_INTERRUPT_STIM:  // timer interrupt
        riscv64_timer_exception();
        break;
      case RISCV64_INTERRUPT_SEXT:  // external interrupt
        platform_irq(frame);
        break;
      default:
        // Pass a zero tval here, since it's not supposed to be set for interrupts.
        fatal_exception(cause, 0, frame);
    }

    do_preempt = int_handler_finish(&state);
  } else {
    // All synchronous traps go here.

    // Sample tval here and pass down in case any of the exceptions handlers need it
    // or to be captured in a crash log.
    uint64_t tval = riscv64_csr_read(RISCV64_CSR_STVAL);
    switch (cause) {
      case RISCV64_EXCEPTION_INS_PAGE_FAULT:
      case RISCV64_EXCEPTION_LOAD_PAGE_FAULT:
      case RISCV64_EXCEPTION_STORE_PAGE_FAULT:
        riscv64_page_fault_handler(cause, tval, frame, user);
        break;
      case RISCV64_EXCEPTION_IADDR_MISALIGN:
      case RISCV64_EXCEPTION_LOAD_ADDR_MISALIGN:
      case RISCV64_EXCEPTION_STORE_ADDR_MISALIGN:
        riscv64_misaligned_fault_handler(cause, tval, frame, user);
        break;
      case RISCV64_EXCEPTION_ILLEGAL_INS:
        riscv64_illegal_instruction_handler(cause, tval, frame, user);
        break;
      case RISCV64_EXCEPTION_BREAKPOINT:
        riscv64_breakpoint_handler(cause, tval, frame, user);
        break;
      case RISCV64_EXCEPTION_ENV_CALL_U_MODE:
        if (unlikely(!user)) {
          exception_die(frame, cause, tval, "syscall from supervisor mode\n");
        }
        riscv64_syscall_handler(frame);
        break;
      case RISCV64_EXCEPTION_ENV_CALL_S_MODE:
        exception_die(frame, cause, tval, "syscall from supervisor mode\n");
        break;
      default:
        fatal_exception(cause, tval, frame);
    }
  }

  if (unlikely(user)) {
    // In the case of receiving a kill signal, this function may not return,
    // but the scheduler would have been invoked so it's fine.
    arch_iframe_process_pending_signals(frame);
  }
  if (do_preempt) {
    Thread::Current::Preempt();
  }
}

void arch_iframe_process_pending_signals(iframe_t* iframe) {
  DEBUG_ASSERT(iframe != nullptr);
  Thread::Current::ProcessPendingSignals(GeneralRegsSource::Iframe, iframe);
}

void arch_dump_exception_context(const arch_exception_context_t* context) {
  // Print the common fields first.
  dump_common_exception_context(context);

  // If we don't have a frame, there's nothing more we can print.
  if (!context->frame) {
    printf("no frame to dump\n");
    return;
  }

  printf("iframe %p:\n", context->frame);
  PrintFrame(stdout, *context->frame);
  printf("cause  %18" PRIi64 " %s\n", context->cause, cause_to_string(context->cause));
  printf("tval   %#18" PRIx64 "\n", context->tval);

  // try to dump the user stack
  uintptr_t usp = context->frame->regs.sp;
  if (is_user_accessible(usp)) {
    uint8_t buf[256];
    if (arch_copy_from_user(buf, reinterpret_cast<const void*>(usp), sizeof(buf)) == ZX_OK) {
      printf("bottom of user stack at %#lx:\n", usp);
      hexdump_ex(buf, sizeof(buf), usp);
    }
  }
}

void arch_fill_in_exception_context(const arch_exception_context_t* arch_context,
                                    zx_exception_report_t* report) {
  zx_exception_context_t* zx_context = &report->context;

  zx_context->synth_code = arch_context->user_synth_code;
  zx_context->synth_data = arch_context->user_synth_data;
  zx_context->arch.u.riscv_64.cause = arch_context->cause;
  zx_context->arch.u.riscv_64.tval = arch_context->tval;
}

zx_status_t arch_dispatch_user_policy_exception(uint32_t policy_exception_code,
                                                uint32_t policy_exception_data) {
  arch_exception_context_t context = {};
  context.user_synth_code = policy_exception_code;
  context.user_synth_data = policy_exception_data;
  return dispatch_user_exception(ZX_EXCP_POLICY_ERROR, &context);
}

bool arch_install_exception_context(Thread* thread, const arch_exception_context_t* context) {
  if (!context->frame) {
    // TODO(fxbug.dev/30521): Must be a synthetic exception as they don't (yet) provide the
    // registers.
    return false;
  }

  arch_set_suspended_general_regs(thread, GeneralRegsSource::Iframe, context->frame);
  return true;
}

void arch_remove_exception_context(Thread* thread) { arch_reset_suspended_general_regs(thread); }

void PrintFrame(FILE* f, const iframe_t& frame) {
  // Define a shorter macro to keep the code formatter from badly wrapping the following code.
#define fpr(args...) fprintf(f, args)
  fpr("iframe %p:\n", &frame);
  fpr("epc    %#18" PRIx64 " x1/ra  %#18" PRIx64 " x2/sp   %#18" PRIx64 " x3/gp   %#18" PRIx64 "\n",
      frame.regs.pc, frame.regs.ra, frame.regs.sp, frame.regs.gp);
  fpr("x4/tp  %#18" PRIx64 " x5/t0  %#18" PRIx64 " x6/t1   %#18" PRIx64 " x7/t2   %#18" PRIx64 "\n",
      frame.regs.tp, frame.regs.t0, frame.regs.t1, frame.regs.t2);
  fpr("x8/s0  %#18" PRIx64 " x9/s1  %#18" PRIx64 " x10/a0  %#18" PRIx64 " x11/a1  %#18" PRIx64 "\n",
      frame.regs.s0, frame.regs.s1, frame.regs.a0, frame.regs.a1);
  fpr("x12/a2 %#18" PRIx64 " x13/a3 %#18" PRIx64 " x14/a4  %#18" PRIx64 " x15/a5  %#18" PRIx64 "\n",
      frame.regs.a2, frame.regs.a3, frame.regs.a4, frame.regs.a5);
  fpr("x16/a6 %#18" PRIx64 " x17/a7 %#18" PRIx64 " x18/s2  %#18" PRIx64 " x19/s3  %#18" PRIx64 "\n",
      frame.regs.a6, frame.regs.a7, frame.regs.s2, frame.regs.s3);
  fpr("x20/s4 %#18" PRIx64 " x21/s5 %#18" PRIx64 " x22/s6  %#18" PRIx64 " x23/s7  %#18" PRIx64 "\n",
      frame.regs.s4, frame.regs.s5, frame.regs.s6, frame.regs.s7);
  fpr("x24/s8 %#18" PRIx64 " x25/s9 %#18" PRIx64 " x26/s10 %#18" PRIx64 " x27/s11 %#18" PRIx64 "\n",
      frame.regs.s8, frame.regs.s9, frame.regs.s10, frame.regs.s11);
  fpr("x28/t3 %#18" PRIx64 " x29/t4 %#18" PRIx64 " x30/t5  %#18" PRIx64 " x31/t6  %#18" PRIx64 "\n",
      frame.regs.t3, frame.regs.t4, frame.regs.t5, frame.regs.t6);
  fpr("status %#18" PRIx64 "\n", frame.status);
#undef fpr
}
