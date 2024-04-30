// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "object/thread_dispatcher.h"

#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/task_wrapper.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>

#include <fbl/ref_ptr.h>

const fbl::RefPtr<starnix::TaskWrapper>& ThreadDispatcher::task_wrapper() const { return task_; }
fbl::RefPtr<starnix::TaskWrapper>& ThreadDispatcher::task_wrapper() { return task_; }
