// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "object/thread_dispatcher.h"

#include <lib/starnix_zircon/task_wrapper.h>

#include <ktl/move.h>

void ThreadDispatcher::SetTask(fbl::RefPtr<TaskWrapper> task) { starnix_task_ = ktl::move(task); }

// Get the associated Starnix Task.
TaskWrapper* ThreadDispatcher::task() { return starnix_task_.get(); }
const TaskWrapper* ThreadDispatcher::task() const { return starnix_task_.get(); }
