// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/stall.h"

#include <arch/interrupt.h>
#include <kernel/percpu.h>

void StallAccumulator::UpdateWithIrqDisabled(int op_contributors_progressing,
                                             int op_contributors_stalling) {
  // Check argument range.
  ZX_DEBUG_ASSERT(-1 <= op_contributors_progressing && op_contributors_progressing <= +1);
  ZX_DEBUG_ASSERT(-1 <= op_contributors_stalling && op_contributors_stalling <= +1);

  Guard<SpinLock, NoIrqSave> guard{&lock_};
  Consolidate();

  // Apply variations.
  num_contributors_progressing_ += op_contributors_progressing;
  num_contributors_stalling_ += op_contributors_stalling;

  // Check that we are counting correctly and we never decrement below zero.
  ZX_DEBUG_ASSERT(num_contributors_progressing_ != SIZE_MAX);
  ZX_DEBUG_ASSERT(num_contributors_stalling_ != SIZE_MAX);
}

void StallAccumulator::Update(int op_contributors_progressing, int op_contributors_stalling) {
  InterruptDisableGuard guard;
  UpdateWithIrqDisabled(op_contributors_progressing, op_contributors_stalling);
}

StallAccumulator::Stats StallAccumulator::Flush() {
  Guard<SpinLock, IrqSave> guard{&lock_};
  Consolidate();

  Stats result = accumulated_stats_;
  accumulated_stats_ = {};
  return result;
}

void StallAccumulator::Consolidate() {
  zx_time_t now = current_time();
  zx_duration_t time_delta = now - last_consolidate_time_;

  if (num_contributors_stalling_ > 0) {
    accumulated_stats_.total_time_stall_some += time_delta;
  }

  if (num_contributors_stalling_ > 0 && num_contributors_progressing_ == 0) {
    accumulated_stats_.total_time_stall_full += time_delta;
  }

  if (num_contributors_progressing_ > 0 || num_contributors_stalling_ > 0) {
    accumulated_stats_.total_time_active += time_delta;
  }

  last_consolidate_time_ = now;
}

void StallAccumulator::ApplyContextSwitch(Thread *current_thread, Thread *next_thread) {
  // Helper functions to get/set the given thread's memory_stall_state value,
  // which is guarded by preempt_disabled_token. This is necessary because
  // preemption is not formally disabled when we're called (i.e. within the
  // scheduler), but in fact it is, because the scheduler does not preempt
  // itself.
  auto GetMemoryStallState = [](const Thread *thread) TA_NO_THREAD_SAFETY_ANALYSIS {
    return thread->memory_stall_state();
  };
  auto SetMemoryStallState =
      [](Thread *thread, ThreadStallState value)
          TA_NO_THREAD_SAFETY_ANALYSIS { thread->set_memory_stall_state(value); };

  cpu_num_t current_cpu = arch_curr_cpu_num();

  const SchedulerState *next_state = &next_thread->scheduler_state();
  ZX_DEBUG_ASSERT(next_state->curr_cpu() == current_cpu);

  StallAccumulator &local_accumulator = percpu::GetCurrent().memory_stall_accumulator;
  int local_op_progressing = 0;
  int local_op_stalling = 0;

  // If the current thread is not stalling, remove it from the number of progressing threads.
  if (GetMemoryStallState(current_thread) == ThreadStallState::Progressing) {
    SetMemoryStallState(current_thread, ThreadStallState::Inactive);
    local_op_progressing -= 1;
  }

  // If the next thread has been migrated while stalling, move its stall to the current CPU.
  if (GetMemoryStallState(next_thread) == ThreadStallState::Stalling &&
      next_state->last_cpu() != current_cpu) {
    // Unmark the thread as stalling on its last CPU.
    ZX_DEBUG_ASSERT_MSG(next_state->last_cpu() != INVALID_CPU,
                        "Stalling threads must have run at least once");
    StallAccumulator &last_accumulator =
        percpu::Get(next_state->last_cpu()).memory_stall_accumulator;
    last_accumulator.UpdateWithIrqDisabled(0, -1);

    // We now have new a stalling thread tied to the current_cpu.
    local_op_stalling += 1;
  }

  // If the next thread is not stalling, add it to the local counter.
  if (GetMemoryStallState(next_thread) == ThreadStallState::Inactive) {
    SetMemoryStallState(next_thread, ThreadStallState::Progressing);
    local_op_progressing += 1;
  }

  // Propagate changes (we can skip this to be faster if there are no changes).
  if (local_op_progressing != 0 || local_op_stalling != 0) {
    local_accumulator.UpdateWithIrqDisabled(local_op_progressing, local_op_stalling);
  }
}
