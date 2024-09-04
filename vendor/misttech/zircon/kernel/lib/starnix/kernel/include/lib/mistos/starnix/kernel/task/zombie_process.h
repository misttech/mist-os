// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_GROUP_DECL_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_GROUP_DECL_H_

#include <fbl/ref_counted.h>

namespace starnix {

class ZombieProcess : public fbl::RefCounted<ZombieProcess> {
  // pid_t pid;
  // pid_t pgid;
  // uid_t pid_tuid;

  // pub exit_info: ProcessExitInfo,

  /// Cumulative time stats for the process and its children.
  // pub time_stats: TaskTimeStats,

  // Whether dropping this ZombieProcess should imply removing the pid from
  // the PidTable
  // bool is_canonical_;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_GROUP_DECL_H_
