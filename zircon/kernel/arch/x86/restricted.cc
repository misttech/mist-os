// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch.h>
#include <inttypes.h>
#include <stdlib.h>
#include <trace.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls/debug.h>

#include <arch/debugger.h>
#include <arch/regs.h>
#include <arch/vm.h>
#include <arch/x86.h>
#include <arch/x86/descriptor.h>
#include <arch/x86/feature.h>
#include <kernel/restricted.h>
#include <vm/vm_address_region.h>

#define LOCAL_TRACE 0

void RestrictedState::ArchDump(const zx_restricted_state_t& state) {
  printf(" RIP: %#18" PRIx64 "  FL: %#18" PRIx64 "\n", state.ip, state.flags);
  printf(" RAX: %#18" PRIx64 " RBX: %#18" PRIx64 " RCX: %#18" PRIx64 " RDX: %#18" PRIx64 "\n",
         state.rax, state.rbx, state.rcx, state.rdx);
  printf(" RSI: %#18" PRIx64 " RDI: %#18" PRIx64 " RBP: %#18" PRIx64 " RSP: %#18" PRIx64 "\n",
         state.rsi, state.rdi, state.rbp, state.rsp);
  printf("  R8: %#18" PRIx64 "  R9: %#18" PRIx64 " R10: %#18" PRIx64 " R11: %#18" PRIx64 "\n",
         state.r8, state.r9, state.r10, state.r11);
  printf(" R12: %#18" PRIx64 " R13: %#18" PRIx64 " R14: %#18" PRIx64 " R15: %#18" PRIx64 "\n",
         state.r12, state.r13, state.r14, state.r15);
  printf("fs base %#18" PRIx64 " gs base %#18" PRIx64 "\n", state.fs_base, state.gs_base);
}

zx_status_t RestrictedState::ArchValidateStatePreRestrictedEntry(
    const zx_restricted_state_t& state) {
  // validate that RIP is within user space
  if (unlikely(!is_user_accessible(state.ip))) {
    LTRACEF("fail due to bad ip %#" PRIx64 "\n", state.ip);
    return ZX_ERR_BAD_STATE;
  }

  // validate that the rflags saved only contain user settable flags
  if (unlikely((state.flags & ~X86_FLAGS_USER) != 0)) {
    LTRACEF("fail due to flags outside of X86_FLAGS_USER set (%#" PRIx64 ")\n", state.flags);
    return ZX_ERR_BAD_STATE;
  }

  // fs and gs base must be canonical
  if (unlikely(!x86_is_vaddr_canonical(state.fs_base))) {
    LTRACEF("fail due to bad fs base %#" PRIx64 "\n", state.fs_base);
    return ZX_ERR_BAD_STATE;
  }
  if (unlikely(!x86_is_vaddr_canonical(state.gs_base))) {
    LTRACEF("fail due to bad gs base %#" PRIx64 "\n", state.gs_base);
    return ZX_ERR_BAD_STATE;
  }

  // everything else can be whatever value it wants to be. worst case it immediately faults
  // in restricted mode and that's okay.
  return ZX_OK;
}

namespace {

// helper routines to read/write user fs and gs base registers using the optimal
// mechanism. must be called with interrupts disabled around the swapgs sequence.
void get_fsgsbase(uint64_t* fsbase, uint64_t* gsbase) {
  DEBUG_ASSERT(arch_ints_disabled());

  // read the fs/gs base out of the MSRs
  if (likely(x86_feature_test(X86_FEATURE_FSGSBASE))) {
    *fsbase = _readfsbase_u64();
    // the user and kernel base have been swapped, use swapgs to temporarily
    // gain access to the gs register.
    __asm__ __volatile__("swapgs\n");
    uint64_t temp = _readgsbase_u64();
    __asm__ __volatile__("swapgs\n");
    *gsbase = temp;
  } else {
    *fsbase = read_msr(X86_MSR_IA32_FS_BASE);
    *gsbase = read_msr(X86_MSR_IA32_KERNEL_GS_BASE);
  }
}

void set_fsgsbase(uint64_t fsbase, uint64_t gsbase) {
  DEBUG_ASSERT(arch_ints_disabled());

  DEBUG_ASSERT(x86_is_vaddr_canonical(fsbase));
  DEBUG_ASSERT(x86_is_vaddr_canonical(gsbase));
  if (likely(x86_feature_test(X86_FEATURE_FSGSBASE))) {
    _writefsbase_u64(fsbase);
    // the user and kernel base have been swapped, use swapgs to temporarily
    // gain access to the gs register.
    __asm__ __volatile__("swapgs\n");
    _writegsbase_u64(gsbase);
    __asm__ __volatile__("swapgs\n");
  } else {
    write_msr(X86_MSR_IA32_FS_BASE, fsbase);
    write_msr(X86_MSR_IA32_KERNEL_GS_BASE, gsbase);
  }
}

}  // namespace

void RestrictedState::ArchSaveStatePreRestrictedEntry(ArchSavedNormalState& arch_state) {
  // save the normal mode fs/gs base which we'll reload on the way back
  get_fsgsbase(&arch_state.normal_fs_base_, &arch_state.normal_gs_base_);
}

[[noreturn]] void RestrictedState::ArchEnterRestricted(const zx_restricted_state_t& state) {
  DEBUG_ASSERT(arch_ints_disabled());

  // load the user fs/gs base from restricted mode
  set_fsgsbase(state.fs_base, state.gs_base);

  // copy to a kernel iframe_t
  // struct iframe_t {
  //   uint64_t rdi, rsi, rbp, rbx, rdx, rcx, rax;     // pushed by common handler
  //   uint64_t r8, r9, r10, r11, r12, r13, r14, r15;  // pushed by common handler
  //   uint64_t vector;                                // pushed by stub
  //   uint64_t err_code;                              // pushed by interrupt or stub
  //   uint64_t ip, cs, flags;                         // pushed by interrupt
  //   uint64_t user_sp, user_ss;                      // pushed by interrupt
  // };
  iframe_t iframe{};
  iframe.rdi = state.rdi;
  iframe.rsi = state.rsi;
  iframe.rbp = state.rbp;
  iframe.rbx = state.rbx;
  iframe.rdx = state.rdx;
  iframe.rcx = state.rcx;
  iframe.rax = state.rax;
  iframe.r8 = state.r8;
  iframe.r9 = state.r9;
  iframe.r10 = state.r10;
  iframe.r11 = state.r11;
  iframe.r12 = state.r12;
  iframe.r13 = state.r13;
  iframe.r14 = state.r14;
  iframe.r15 = state.r15;
  iframe.ip = state.ip;
  iframe.cs = USER_CODE_64_SELECTOR;
  iframe.flags = state.flags | X86_FLAGS_IF;
  iframe.user_sp = state.rsp;
  iframe.user_ss = USER_DATA_SELECTOR;
  iframe.vector = iframe.err_code = 0;  // iframe.vector/err_code are unused

  // load the new state and exit
  arch_enter_uspace(&iframe);

  __UNREACHABLE;
}

void RestrictedState::ArchRedirectRestrictedExceptionToNormal(
    const ArchSavedNormalState& arch_state, uintptr_t vector_table, uintptr_t context) {
  zx_thread_state_general_regs_t regs = {};
  regs.rip = vector_table;
  regs.rdi = context;
  regs.rsi = ZX_RESTRICTED_REASON_EXCEPTION;
  regs.rflags = X86_FLAGS_IF;
  regs.fs_base = arch_state.normal_fs_base_;
  regs.gs_base = arch_state.normal_gs_base_;

  // This should only fail if we do not have any suspended registers, but this
  // should always be the case at this point during exception handling.
  [[maybe_unused]] zx_status_t status = arch_set_general_regs(Thread::Current().Get(), &regs);
  DEBUG_ASSERT(status == ZX_OK);
}

void RestrictedState::ArchSaveRestrictedExceptionState(zx_restricted_state_t& state) {
  zx_thread_state_general_regs_t regs = {};
  [[maybe_unused]] zx_status_t status = arch_get_general_regs(Thread::Current().Get(), &regs);
  // This should only fail if we do not have any suspended registers, but this
  // should always be the case at this point during exception handling.
  DEBUG_ASSERT(status == ZX_OK);

  state.rdi = regs.rdi;
  state.rsi = regs.rsi;
  state.rbp = regs.rbp;
  state.rbx = regs.rbx;
  state.rdx = regs.rdx;
  state.rcx = regs.rcx;
  state.rax = regs.rax;
  state.rsp = regs.rsp;
  state.r8 = regs.r8;
  state.r9 = regs.r9;
  state.r10 = regs.r10;
  state.r11 = regs.r11;
  state.r12 = regs.r12;
  state.r13 = regs.r13;
  state.r14 = regs.r14;
  state.r15 = regs.r15;
  state.ip = regs.rip;
  state.flags = regs.rflags & X86_FLAGS_USER;

  // read the fs/gs base out of the MSRs. This requires interrupts to be
  // disabled.
  DEBUG_ASSERT(!arch_ints_disabled());
  arch_disable_ints();
  get_fsgsbase(&state.fs_base, &state.gs_base);
  arch_enable_ints();
}

void RestrictedState::ArchSaveRestrictedSyscallState(zx_restricted_state_t& state,
                                                     const syscall_regs_t& regs) {
  DEBUG_ASSERT(arch_ints_disabled());

  // copy state syscall_regs_t to zx_restricted_state
  state.rdi = regs.rdi;
  state.rsi = regs.rsi;
  state.rbp = regs.rbp;
  state.rbx = regs.rbx;
  state.rdx = regs.rdx;
  state.rcx = regs.rcx;
  state.rax = regs.rax;
  state.rsp = regs.rsp;
  state.r8 = regs.r8;
  state.r9 = regs.r9;
  state.r10 = regs.r10;
  state.r11 = regs.r11;
  state.r12 = regs.r12;
  state.r13 = regs.r13;
  state.r14 = regs.r14;
  state.r15 = regs.r15;
  state.ip = regs.rip;
  state.flags = regs.rflags & X86_FLAGS_USER;

  // read the fs/gs base out of the MSRs
  get_fsgsbase(&state.fs_base, &state.gs_base);
}

void RestrictedState::ArchSaveRestrictedIframeState(zx_restricted_state_t& state,
                                                    const iframe_t& frame) {
  DEBUG_ASSERT(arch_ints_disabled());

  // copy state iframe_t to zx_restricted_state
  state.rdi = frame.rdi;
  state.rsi = frame.rsi;
  state.rbp = frame.rbp;
  state.rbx = frame.rbx;
  state.rdx = frame.rdx;
  state.rcx = frame.rcx;
  state.rax = frame.rax;
  state.r8 = frame.r8;
  state.r9 = frame.r9;
  state.r10 = frame.r10;
  state.r11 = frame.r11;
  state.r12 = frame.r12;
  state.r13 = frame.r13;
  state.r14 = frame.r14;
  state.r15 = frame.r15;

  state.ip = frame.ip;
  state.flags = frame.flags & X86_FLAGS_USER;
  state.rsp = frame.user_sp;

  // vector, err_code, cs, and user_ss are unused.

  // read the fs/gs base out of the MSRs
  get_fsgsbase(&state.fs_base, &state.gs_base);
}

[[noreturn]] void RestrictedState::ArchEnterFull(const ArchSavedNormalState& arch_state,
                                                 uintptr_t vector_table, uintptr_t context,
                                                 uint64_t code) {
  DEBUG_ASSERT(arch_ints_disabled());

  // load the user fs/gs base from normal mode
  set_fsgsbase(arch_state.normal_fs_base_, arch_state.normal_gs_base_);

  // set up a mostly blank iframe and return back to normal mode
  iframe_t iframe{};
  iframe.rdi = context;
  iframe.rsi = code;
  iframe.ip = vector_table;
  iframe.cs = USER_CODE_64_SELECTOR;
  iframe.flags = X86_FLAGS_IF;
  iframe.user_ss = USER_DATA_SELECTOR;

  // load the new state and exit
  arch_enter_uspace(&iframe);

  __UNREACHABLE;
}
