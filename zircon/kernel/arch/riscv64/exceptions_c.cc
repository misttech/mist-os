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
#include <arch/exception.h>
#include <arch/regs.h>
#include <arch/thread.h>
#include <arch/user_copy.h>
#include <kernel/interrupt.h>
#include <kernel/thread.h>
#include <pretty/hexdump.h>
#include <syscalls/syscalls.h>
#include <vm/fault.h>
#include <vm/vm.h>

#define LOCAL_TRACE 0

static zx_status_t try_dispatch_user_data_fault_exception(zx_excp_type_t type, iframe_t* iframe) {
  arch_exception_context_t context = {};
  DEBUG_ASSERT(iframe != nullptr);
  context.frame = iframe;

  arch_enable_ints();
  zx_status_t status = dispatch_user_exception(type, &context);
  arch_disable_ints();
  return status;
}

void arch_iframe_process_pending_signals(iframe_t* iframe) {}

void arch_dump_exception_context(const arch_exception_context_t* context) {}

void arch_fill_in_exception_context(const arch_exception_context_t* arch_context,
                                    zx_exception_report_t* report) {}

zx_status_t arch_dispatch_user_policy_exception(uint32_t policy_exception_code,
                                                uint32_t policy_exception_data) {
  return ZX_OK;
}

bool arch_install_exception_context(Thread* thread, const arch_exception_context_t* context) {
  return true;
}

void arch_remove_exception_context(Thread* thread) {}

void PrintFrame(FILE* f, const iframe_t& frame) {
  fprintf(f, "iframe %p:\n", &frame);
  fprintf(f,
          "epc    %#18" PRIx64 " x1/ra  %#18" PRIx64 " x2/sp   %#18" PRIx64 " x3/gp   %#18" PRIx64
          "\n",
          frame.regs.pc, frame.regs.ra, frame.regs.sp, frame.regs.gp);
  fprintf(f,
          "x4/tp  %#18" PRIx64 " x5/t0  %#18" PRIx64 " x6/t1   %#18" PRIx64 " x7/t2   %#18" PRIx64
          "\n",
          frame.regs.tp, frame.regs.t0, frame.regs.t1, frame.regs.t2);
  fprintf(f,
          "x8/s0  %#18" PRIx64 " x9/s1  %#18" PRIx64 " x10/a0  %#18" PRIx64 " x11/a1  %#18" PRIx64
          "\n",
          frame.regs.s0, frame.regs.s1, frame.regs.a0, frame.regs.a1);
  fprintf(f,
          "x12/a2 %#18" PRIx64 " x13/a3 %#18" PRIx64 " x14/a4  %#18" PRIx64 " x15/a5  %#18" PRIx64
          "\n",
          frame.regs.a2, frame.regs.a3, frame.regs.a4, frame.regs.a5);
  fprintf(f,
          "x16/a6 %#18" PRIx64 " x17/a7 %#18" PRIx64 " x18/s2  %#18" PRIx64 " x19/s3  %#18" PRIx64
          "\n",
          frame.regs.a6, frame.regs.a7, frame.regs.s2, frame.regs.s3);
  fprintf(f,
          "x20/s4 %#18" PRIx64 " x21/s5 %#18" PRIx64 " x22/s6  %#18" PRIx64 " x23/s7  %#18" PRIx64
          "\n",
          frame.regs.s4, frame.regs.s5, frame.regs.s6, frame.regs.s7);
  fprintf(f,
          "x24/s8 %#18" PRIx64 " x25/s9 %#18" PRIx64 " x26/s10 %#18" PRIx64 " x27/s11 %#18" PRIx64
          "\n",
          frame.regs.s8, frame.regs.s9, frame.regs.s10, frame.regs.s11);
  fprintf(f,
          "x28/t3 %#18" PRIx64 " x29/t4 %#18" PRIx64 " x30/t5  %#18" PRIx64 " x31/t6  %#18" PRIx64
          "\n",
          frame.regs.t3, frame.regs.t4, frame.regs.t5, frame.regs.t6);
  fprintf(f, "status %#18" PRIx64 "\n", frame.status);
}

static const char* cause_to_string(long cause) {
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
__NO_RETURN __NO_INLINE static void exception_die(iframe_t* iframe, const char* format, ...) {
  platform_panic_start();

  va_list args;
  va_start(args, format);
  vprintf(format, args);
  va_end(args);

  /* fatal exception, die here */
  PrintFrame(stdout, *iframe);

  platform_halt(HALT_ACTION_HALT, ZirconCrashReason::Panic);
}

__NO_RETURN __NO_INLINE static void fatal_exception(long cause, struct iframe_t* frame) {
  if (cause < 0) {
    exception_die(frame, "unhandled interrupt cause %#lx, epc %#lx, tval %#lx cpu %u\n", cause,
                  frame->regs.pc, riscv64_csr_read(RISCV64_CSR_STVAL), arch_curr_cpu_num());
  } else {
    exception_die(frame, "unhandled exception cause %#lx (%s), epc %#lx, tval %#lx, cpu %u\n",
                  cause, cause_to_string(cause), frame->regs.pc,
                  riscv64_csr_read(RISCV64_CSR_STVAL), arch_curr_cpu_num());
  }
}

static void riscv64_page_fault_handler(long cause, struct iframe_t* frame, bool user) {
  vaddr_t tval = riscv64_csr_read(RISCV64_CSR_STVAL);
  uint pf_flags = VMM_PF_FLAG_NOT_PRESENT;
  pf_flags |= (cause == RISCV64_EXCEPTION_STORE_PAGE_FAULT) ? VMM_PF_FLAG_WRITE : 0;
  pf_flags |= (cause == RISCV64_EXCEPTION_INS_PAGE_FAULT) ? VMM_PF_FLAG_INSTRUCTION : 0;
  pf_flags |= user ? VMM_PF_FLAG_USER : 0;

  zx_status_t pf_status = vmm_page_fault_handler(tval, pf_flags);

  if (pf_status != ZX_OK) {
    uint64_t dfr = Thread::Current::Get()->arch().data_fault_resume;
    if (unlikely(dfr)) {
      frame->regs.pc = dfr;
      frame->regs.a1 = tval;
      frame->regs.a2 = pf_flags;
      return;
    }

    // If this is from user space, let the user exception handler get a shot at it.
    if (pf_flags & VMM_PF_FLAG_USER) {
      if (try_dispatch_user_data_fault_exception(ZX_EXCP_FATAL_PAGE_FAULT, frame) == ZX_OK) {
        return;
      }
    } else {
      exception_die(frame, "Page fault in kernel: %s, %s, address %#lx\n",
                    (pf_flags & VMM_PF_FLAG_INSTRUCTION) ? "instruction" : "data",
                    (pf_flags & VMM_PF_FLAG_WRITE) ? "write" : "read", tval);
    }
  }
}

static void riscv64_illegal_instruction_handler(long cause, struct iframe_t* frame, bool user) {
  // TODO-rvbringup: actually implement lazy FPU context switch
  exception_die(frame, "unimplemented illegal instruction handler: from_user %u\n", user);
#if 0
  // If the FPU is already enabled this is bad.
  if ((frame->status & RISCV64_CSR_SSTATUS_FS) != RISCV64_CSR_SSTATUS_FS_OFF) {
    try_dispatch_user_data_fault_exception(ZX_EXCP_UNDEFINED_INSTRUCTION,
        frame);
  }
  // Otherwise we just try to enable the FPU.
  frame->status |= RISCV64_CSR_SSTATUS_FS_INITIAL;
#endif
}

extern "C" syscall_result riscv64_syscall_dispatcher(struct iframe_t* frame) {
  // TODO-rvbringup: implement in exceptions.S
  PANIC_UNIMPLEMENTED;
}

static void riscv64_syscall_handler(struct iframe_t* frame, bool user) {
  if (unlikely(!user)) {
    exception_die(frame, "syscall from supervisor mode\n");
  }

  // TODO-rvbringup: double check that need to do this skip
  frame->regs.pc = frame->regs.pc + 0x4;  // Skip the ecall instruction

  struct syscall_result ret = riscv64_syscall_dispatcher(frame);
  frame->regs.a0 = ret.status;
  if (ret.is_signaled) {
    Thread::Current::ProcessPendingSignals(GeneralRegsSource::Iframe, frame);
  }
}

extern "C" void riscv64_exception_handler(long cause, struct iframe_t* frame) {
  const bool user = (frame->status & RISCV64_CSR_SSTATUS_PP) == 0;

  LTRACEF("hart %u cause %s epc %#lx status %#lx user %u\n", arch_curr_cpu_num(),
          cause_to_string(cause), frame->regs.pc, frame->status, user);

  // TODO-rvbringup: add some kcounters

  // top bit of the cause register determines if it's an interrupt or not
  if (cause < 0) {
    int_handler_saved_state_t state;
    int_handler_start(&state);

    switch (cause & LONG_MAX) {
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
        fatal_exception(cause, frame);
    }

    bool do_preempt = int_handler_finish(&state);
    // TODO-rvbringup: add arch_iframe_process_pending_signals here if from user space
    if (do_preempt) {
      Thread::Current::Preempt();
    }
  } else {
    // all synchronous traps go here
    switch (cause) {
      case RISCV64_EXCEPTION_INS_PAGE_FAULT:
      case RISCV64_EXCEPTION_LOAD_PAGE_FAULT:
      case RISCV64_EXCEPTION_STORE_PAGE_FAULT:
        riscv64_page_fault_handler(cause, frame, user);
        break;
      case RISCV64_EXCEPTION_ILLEGAL_INS:
        riscv64_illegal_instruction_handler(cause, frame, user);
        break;
      case RISCV64_EXCEPTION_ENV_CALL_U_MODE:
        riscv64_syscall_handler(frame, user);
        break;
      default:
        fatal_exception(cause, frame);
    }
  }
}
