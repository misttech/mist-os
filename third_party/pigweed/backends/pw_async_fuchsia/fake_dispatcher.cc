// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pw_async_fuchsia/fake_dispatcher.h"

#include "pw_async_fuchsia/util.h"

namespace pw::async::test::backend {

NativeFakeDispatcher::NativeFakeDispatcher(Dispatcher& test_dispatcher)
    : dispatcher_(test_dispatcher) {}

void NativeFakeDispatcher::DestroyLoop() { fake_loop_.Shutdown(); }

chrono::SystemClock::time_point NativeFakeDispatcher::now() { return fake_loop_.Now(); }

void NativeFakeDispatcher::Post(Task& task) { PostAt(task, now()); }

void NativeFakeDispatcher::PostAfter(Task& task, chrono::SystemClock::duration delay) {
  PostAt(task, now() + delay);
}

void NativeFakeDispatcher::PostAt(Task& task, chrono::SystemClock::time_point time) {
  // TODO(fxbug.dev/125112): Return errors once these methods return a Status.
  if (!fake_loop_.Runnable()) {
    Context ctx{.dispatcher = &dispatcher_, .task = &task};
    task(ctx, Status::Cancelled());
    return;
  }
  ::pw::async::backend::NativeTask& native_task = task.native_type();
  native_task.set_due_time(time);
  native_task.dispatcher_ = &dispatcher_;
  fake_loop_.PostTask(&native_task);
}

bool NativeFakeDispatcher::Cancel(Task& task) {
  return fake_loop_.Runnable() && fake_loop_.CancelTask(&task.native_type()) == ZX_OK;
}

void NativeFakeDispatcher::RunUntilIdle() {
  if (stop_requested_) {
    DestroyLoop();
    return;
  }
  fake_loop_.RunUntilIdle();
}

void NativeFakeDispatcher::RunUntil(chrono::SystemClock::time_point end_time) {
  if (stop_requested_) {
    DestroyLoop();
    return;
  }
  fake_loop_.Run(pw_async_fuchsia::TimepointToZxTime(end_time).get(), false);
}

void NativeFakeDispatcher::RunFor(chrono::SystemClock::duration duration) {
  RunUntil(now() + duration);
}

NativeFakeDispatcher::FakeAsyncLoop::FakeAsyncLoop() {
  list_initialize(&task_list_);
  list_initialize(&due_list_);
}

chrono::SystemClock::time_point NativeFakeDispatcher::FakeAsyncLoop::Now() const {
  return pw_async_fuchsia::ZxTimeToTimepoint(zx::time{now_});
}

zx_status_t NativeFakeDispatcher::FakeAsyncLoop::PostTask(async_task_t* task) {
  if (state_ == ASYNC_LOOP_SHUTDOWN) {
    return ZX_ERR_BAD_STATE;
  }

  InsertTask(task);
  if (!dispatching_tasks_ && TaskToNode(task)->prev == &task_list_) {
    // Task inserted at head.  Earliest deadline changed.
    RestartTimer();
  }

  return ZX_OK;
}

zx_status_t NativeFakeDispatcher::FakeAsyncLoop::CancelTask(async_task_t* task) {
  // Note: We need to process cancellations even while the loop is being
  // destroyed in case the client is counting on the handler not being
  // invoked again past this point.  Also, the task we're removing here
  // might be present in the dispatcher's |due_list| if it is pending
  // dispatch instead of in the loop's |task_list| as usual.  The same
  // logic works in both cases.

  list_node_t* node = TaskToNode(task);
  if (!list_in_list(node)) {
    return ZX_ERR_NOT_FOUND;
  }

  // Determine whether the head task was canceled and following task has
  // a later deadline.  If so, we will bump the timer along to that deadline.
  bool must_restart =
      !dispatching_tasks_ && node->prev == &task_list_ &&
      (node->next == &task_list_ || NodeToTask(node->next)->deadline > task->deadline);
  list_delete(node);
  if (must_restart) {
    RestartTimer();
  }

  return ZX_OK;
}

zx_status_t NativeFakeDispatcher::FakeAsyncLoop::RunUntilIdle() {
  zx_status_t status = Run(now_, false);
  if (status == ZX_ERR_TIMED_OUT) {
    status = ZX_OK;
  }
  return status;
}

zx_status_t NativeFakeDispatcher::FakeAsyncLoop::Run(zx_time_t deadline, bool once) {
  zx_status_t status;
  do {
    status = RunOnce(deadline);
  } while (status == ZX_OK && !once);
  return status;
}

void NativeFakeDispatcher::FakeAsyncLoop::InsertTask(async_task_t* task) {
  list_node_t* node;
  for (node = task_list_.prev; node != &task_list_; node = node->prev) {
    if (task->deadline >= NodeToTask(node)->deadline) {
      break;
    }
  }
  list_add_after(node, TaskToNode(task));
}

void NativeFakeDispatcher::FakeAsyncLoop::RestartTimer() {
  zx_time_t deadline = NextDeadline();

  if (deadline == ZX_TIME_INFINITE) {
    // Nothing is left on the queue to fire.
    if (timer_armed_) {
      next_timer_expiration_ = ZX_TIME_INFINITE;  // Simulate timer cancellation.
      timer_armed_ = false;
    }
    return;
  }

  next_timer_expiration_ = deadline;

  if (!timer_armed_) {
    timer_armed_ = true;
  }
}

zx_time_t NativeFakeDispatcher::FakeAsyncLoop::NextDeadline() {
  if (list_is_empty(&due_list_)) {
    list_node_t* head = list_peek_head(&task_list_);
    if (!head) {
      return ZX_TIME_INFINITE;
    }
    async_task_t* task = NodeToTask(head);
    return task->deadline;
  }
  // Fire now.
  return 0ULL;
}

zx_status_t NativeFakeDispatcher::FakeAsyncLoop::RunOnce(zx_time_t deadline) {
  if (state_ == ASYNC_LOOP_SHUTDOWN) {
    return ZX_ERR_BAD_STATE;
  }
  if (state_ != ASYNC_LOOP_RUNNABLE) {
    return ZX_ERR_CANCELED;
  }

  // Simulate timeout of zx_port_wait() syscall.
  if (deadline < next_timer_expiration_) {
    now_ = deadline;
    return ZX_ERR_TIMED_OUT;
  }
  // Otherwise, a timer would have expired at or before `deadline`.
  now_ = next_timer_expiration_;
  next_timer_expiration_ = ZX_TIME_INFINITE;
  return DispatchTasks();
}

zx_status_t NativeFakeDispatcher::FakeAsyncLoop::DispatchTasks() {
  // Dequeue and dispatch one task at a time in case an earlier task wants
  // to cancel a later task which has also come due. Timer restarts are suppressed until we run out
  // of tasks to dispatch.
  if (!dispatching_tasks_) {
    dispatching_tasks_ = true;

    // Extract all of the tasks that are due into |due_list| for dispatch
    // unless we already have some waiting from a previous iteration which
    // we would like to process in order.
    list_node_t* node;
    if (list_is_empty(&due_list_)) {
      zx_time_t due_time = now_;
      list_node_t* tail = nullptr;
      list_for_every(&task_list_, node) {
        if (NodeToTask(node)->deadline > due_time) {
          break;
        }
        tail = node;
      }
      if (tail) {
        list_node_t* head = task_list_.next;
        task_list_.next = tail->next;
        tail->next->prev = &task_list_;
        due_list_.next = head;
        head->prev = &due_list_;
        due_list_.prev = tail;
        tail->next = &due_list_;
      }
    }

    // Dispatch all due tasks.
    while ((node = list_remove_head(&due_list_))) {
      // Invoke the handler.  Note that it might destroy itself.
      async_task_t* task = NodeToTask(node);

      task->handler(nullptr, task, ZX_OK);

      if (state_ != ASYNC_LOOP_RUNNABLE) {
        break;
      }
    }

    dispatching_tasks_ = false;
    timer_armed_ = false;
    RestartTimer();
  }

  return ZX_OK;
}

void NativeFakeDispatcher::FakeAsyncLoop::Shutdown() {
  if (state_ == ASYNC_LOOP_SHUTDOWN) {
    return;
  }
  state_ = ASYNC_LOOP_SHUTDOWN;

  // Cancel any remaining pending tasks on our queues.
  CancelAll();
}

void NativeFakeDispatcher::FakeAsyncLoop::CancelAll() {
  ZX_DEBUG_ASSERT(state_ == ASYNC_LOOP_SHUTDOWN);

  list_node_t* node;

  while ((node = list_remove_head(&due_list_))) {
    async_task_t* task = NodeToTask(node);
    task->handler(nullptr, task, ZX_ERR_CANCELED);
  }
  while ((node = list_remove_head(&task_list_))) {
    async_task_t* task = NodeToTask(node);
    task->handler(nullptr, task, ZX_ERR_CANCELED);
  }
}

}  // namespace pw::async::test::backend
