// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_DEBUG_AGENT_LINUX_TASK_H_
#define SRC_DEVELOPER_DEBUG_DEBUG_AGENT_LINUX_TASK_H_

#include "src/lib/fxl/memory/ref_counted.h"

namespace debug_agent {

// Represents an attached process on Linux.
//
// TODO implement this.
class LinuxTask final : public fxl::RefCountedThreadSafe<LinuxTask> {
 public:
 private:
  FRIEND_REF_COUNTED_THREAD_SAFE(LinuxTask);
  FRIEND_MAKE_REF_COUNTED(LinuxTask);
};

}  // namespace debug_agent

#endif  // SRC_DEVELOPER_DEBUG_DEBUG_AGENT_LINUX_TASK_H_
