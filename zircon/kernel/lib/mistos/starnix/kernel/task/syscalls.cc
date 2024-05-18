// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/syscalls.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>

#include <linux/errno.h>

namespace {

fbl::RefPtr<starnix::Task> get_task_or_current(const starnix::CurrentTask& current_task,
                                               pid_t pid) {
  if (pid == 0) {
    return current_task.task();
  } else {
    // TODO(security): Should this use get_task_if_owner_or_has_capabilities() ?
    return current_task->get_task(pid);
  }
}

}  // namespace

namespace starnix {

using namespace starnix_uapi;

fit::result<Errno, pid_t> sys_getpid(const CurrentTask& current_task) {
  return fit::ok(current_task->get_pid());
}

fit::result<Errno, pid_t> sys_gettid(const CurrentTask& current_task) {
  return fit::ok(current_task->get_tid());
}
fit::result<Errno, pid_t> sys_getppid(const CurrentTask& current_task) {
  auto tg = current_task->thread_group();

  Guard<Mutex> lock(tg->tg_rw_lock());
  return fit::ok(tg->get_ppid());
}

fit::result<Errno, pid_t> sys_getpgid(const CurrentTask& current_task, pid_t pid) {
  fbl::RefPtr<starnix::Task> task = get_task_or_current(current_task, pid);
  if (!task) {
    return fit::error(errno(ESRCH));
  }

  auto tg = task->thread_group();
  Guard<Mutex> lock(tg->tg_rw_lock());
  // selinux_hooks::check_getpgid_access(current_task, &task)?;
  auto pgid = tg->process_group()->leader();
  return fit::ok(pgid);
}

}  // namespace starnix
