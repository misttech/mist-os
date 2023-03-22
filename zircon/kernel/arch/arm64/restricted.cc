// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch.h>
#include <inttypes.h>
#include <stdlib.h>
#include <trace.h>

#include <arch/regs.h>
#include <kernel/restricted_state.h>

// Empty implementation of the arch specific routines for restricted mode.
// Since ArchValidateStatePreRestrictedEntry returns an error the other routines should
// either be never called, or do nothing.

void RestrictedState::ArchDump(const zx_restricted_state_t& state) {}

zx_status_t RestrictedState::ArchValidateStatePreRestrictedEntry(
    const zx_restricted_state_t& state) {
  PANIC_UNIMPLEMENTED;
}

void RestrictedState::ArchSaveStatePreRestrictedEntry(ArchSavedNormalState& arch_state) {}

[[noreturn]] void RestrictedState::ArchEnterRestricted(const zx_restricted_state_t& state) {
  PANIC_UNIMPLEMENTED;
}

void RestrictedState::ArchSaveRestrictedSyscallState(zx_restricted_state_t& state,
                                                     const syscall_regs_t& regs) {}

[[noreturn]] void RestrictedState::ArchEnterFull(const ArchSavedNormalState& arch_state,
                                                 uintptr_t vector_table, uintptr_t context,
                                                 uint64_t code) {
  PANIC_UNIMPLEMENTED;
}
