// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/session.h"

#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <zircon/errors.h>

#include <fbl/alloc_checker.h>

namespace starnix {

SessionMutableState::SessionMutableState() = default;

zx_status_t Session::Create(pid_t leader, fbl::RefPtr<Session>* out) {
  fbl::AllocChecker ac;

  fbl::RefPtr<Session> session = fbl::AdoptRef(new (&ac) Session(leader));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *out = std::move(session);
  return ZX_OK;
}

}  // namespace starnix
