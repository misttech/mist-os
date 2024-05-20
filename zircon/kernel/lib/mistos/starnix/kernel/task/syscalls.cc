// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/syscalls.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/session.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/util/weak_wrapper.h>

#include <linux/errno.h>

namespace {

util::WeakPtr<starnix::Task> get_task_or_current(const starnix::CurrentTask& current_task,
                                                 pid_t pid) {
  if (pid == 0) {
    return current_task.weak_task();
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
  return fit::ok(current_task->thread_group->read().get_ppid());
}

/*
fn get_task_or_current(current_task: &CurrentTask, pid: pid_t) -> WeakRef<Task> {
    if pid == 0 {
        current_task.weak_task()
    } else {
        // TODO(security): Should this use get_task_if_owner_or_has_capabilities() ?
        current_task.get_task(pid)
    }
}
*/

fit::result<Errno, pid_t> sys_getsid(const CurrentTask& current_task, pid_t pid) {
  util::WeakPtr<Task> weak = get_task_or_current(current_task, pid);
  auto result = Task::from_weak(weak);
  if (result.is_error())
    return result.take_error();
  auto target_task = result.value();
  // security::check_task_getsid(current_task, &target_task)?;
  auto sid = target_task->thread_group->read().process_group->session->leader;
  return fit::ok(sid);
}

fit::result<Errno, pid_t> sys_getpgid(const CurrentTask& current_task, pid_t pid) {
  util::WeakPtr<Task> weak = get_task_or_current(current_task, pid);
  auto result = Task::from_weak(weak);
  if (result.is_error())
    return result.take_error();

  auto task = result.value();
  // selinux_hooks::check_getpgid_access(current_task, &task)?;
  auto pgid = task->thread_group->read().process_group->leader;
  return fit::ok(pgid);
}

}  // namespace starnix
