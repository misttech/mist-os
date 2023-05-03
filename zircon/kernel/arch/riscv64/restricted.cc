// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <kernel/restricted_state.h>

void RestrictedState::ArchRedirectRestrictedExceptionToNormal(
    const ArchSavedNormalState& arch_state, uintptr_t vector_table, uintptr_t context) {
  // TODO(https://fxbug.dev/121512): Implement in-thread exceptions on risc-v.
}

void RestrictedState::ArchSaveRestrictedExceptionState(zx_restricted_state_t& state) {
  // TODO(https://fxbug.dev/121512): Implement in-thread exceptions on risc-v.
}
