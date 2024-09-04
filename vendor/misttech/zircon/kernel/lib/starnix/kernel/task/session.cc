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

SessionMutableState::~SessionMutableState() = default;

Session::~Session() = default;

Session::Session(pid_t _leader) : leader(_leader) {}

fbl::RefPtr<Session> Session::New(pid_t _leader) {
  fbl::AllocChecker ac;

  fbl::RefPtr<Session> session = fbl::AdoptRef(new (&ac) Session(_leader));
  ASSERT(ac.check());

  return ktl::move(session);
}

}  // namespace starnix
