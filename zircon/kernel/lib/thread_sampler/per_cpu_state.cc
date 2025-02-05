// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/thread_sampler/per_cpu_state.h>

namespace sampler::internal {
zx::result<> PerCpuState::SetUp(const zx_sampler_config_t& config, PinnedVmObject pinned_memory) {
  const char* name = "sampler_buffer";

  auto mapping_result = VmAspace::kernel_aspace()->RootVmar()->CreateVmMapping(
      0 /* ignored */, pinned_memory.size(), 0 /* align pow2 */,
      VMAR_FLAG_DEBUG_DYNAMIC_KERNEL_MAPPING, pinned_memory.vmo(), pinned_memory.offset(),
      ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE, name);

  if (mapping_result.is_error()) {
    dprintf(INFO, "Failed to map in aspace\n");
    return mapping_result.take_error();
  }

  if (zx_status_t status =
          mapping_result->mapping->MapRange(pinned_memory.offset(), pinned_memory.size(), true);
      status != ZX_OK) {
    dprintf(INFO, "Failed to map range\n");
    return zx::error(status);
  }

  vaddr_t base = mapping_result->base;
  vaddr_t end = base + config.buffer_size;
  vaddr_t ptr = base;

  writer = internal::BufferWriter{ptr, end, ktl::move(mapping_result->mapping),
                                  ktl::move(pinned_memory)};
  period_ = config.period;
  return zx::ok();
}

// Atomically mark a pending write to the write state iff writes are enabled and returns true.
//
// Returns false if a pending write was not set because writes are not enabled.
[[nodiscard]] bool PerCpuState::SetPendingWrite() {
  // We could be racing with a "DisableWrites" here. We need to atomically set the kPendingWrite
  // bit, but only if someone hasn't come along and reset the kWritesEnabled bit from under us.
  //
  // Once we set kPendingWrite, that will prevent the buffer state from being cleaned up until we
  // ResetPendingWrite when we are done writing, even if the kWritesEnabled bit is reset after we
  // claim the PendingWrite.
  uint64_t expected = write_state_.load(ktl::memory_order_relaxed);
  // Ensure we preserve the kPendingTimer bit
  uint64_t desired = expected | kPendingWrite | kWritesEnabled;
  do {
    // We should never have multiple writes on the same cpu occur at once.
    DEBUG_ASSERT((expected & kPendingWrite) == 0);
    // Are writes enabled?
    if ((expected & kWritesEnabled) != kWritesEnabled) {
      return false;
    }
  } while (!write_state_.compare_exchange_weak(expected, desired, ktl::memory_order_acq_rel,
                                               ktl::memory_order_relaxed));
  return true;
}

void PerCpuState::ResetPendingWrite() {
  [[maybe_unused]] uint64_t previous_value =
      write_state_.fetch_and(~kPendingWrite, ktl::memory_order_release);
  DEBUG_ASSERT((previous_value & kPendingWrite) != 0);
}

void PerCpuState::EnableWrites() {
  write_state_.fetch_or(kWritesEnabled, ktl::memory_order_release);
}

void PerCpuState::DisableWrites() {
  write_state_.fetch_and(~kWritesEnabled, ktl::memory_order_release);
}

bool PerCpuState::WritesEnabled() const {
  uint64_t state = write_state_.load(ktl::memory_order_relaxed);
  return (state & kWritesEnabled) != 0;
}

bool PerCpuState::PendingWrites() const {
  // Writers decrement pending writes with release semantics after writing their data. Using acquire
  // semantics here ensures that if we see that there is no longer a pending write, we also see the
  // full contents of what was written.
  uint64_t state = write_state_.load(ktl::memory_order_acquire);
  return (state & kPendingWrite) != 0;
}

bool PerCpuState::PendingTimer() const {
  uint64_t state = write_state_.load(ktl::memory_order_relaxed);
  return (state & kPendingTimer) != 0;
}

void PerCpuState::SetTimer() {
  // We need to be mindful here. We're effectively setting two delayed calls and we need to ensure
  // that we don't accidentally destroy the state from under them if we end the sampling session
  // while they are in flight.
  //
  // We set the timer to trigger Thread::SignalSampleStack after the period elapses. `this` needs
  // to live until the timer goes off or is cancelled. We'd like to pass `this` around rather than
  // going through the global sampler state since that would require acquiring the global sampler
  // lock -- something we don't want to do during interrupt context. While we have access to
  // `this`, we can check if writes are enabled and have state to know for how long to set the
  // timer for.
  //
  // Once the timer goes off and `Thread::SignalSampleStack` is called, we:
  //
  // 1) Set a thread signal on the thread which will eventually call Thread::Current::DoSampleStack.
  // 2) Set this timer again if Writing is still enabled for this PerCpuState.
  //
  // Since Thread::Current::DoSampleStack is called then the thread signal is checked in
  // Thread::Current::ProcessPendingSignals, we have no guarantee that the sampling session still
  // exists (the thread could have been first suspended), or that Thread::Current::DoSampleStack
  // will eventually be called at all for that matter (the thread could be killed first). Thus we
  // can't say, increment a ref count or SamplesInFlight counter and expect to decrement it after
  // we successfully take the sample. Instead, taking the sample will have to acquire the global
  // sampler lock and check if the state is still relevant. This is fine, because it will no
  // longer be in interrupt context.
  //
  // However, for part 2, resetting the timer, we are in interrupt context still and will be
  // calling back to here to set the timer again. To prevent clean up, we set the kPendingTimer bit
  // and keep it set until we find that writes are no longer enabled.

  // We could be racing with a "DisableWrites" here. We need to atomically set the kPendingTimer
  // bit, but only if someone hasn't come along and reset the kWritesEnabled bit from under us.
  uint64_t expected = write_state_.load(ktl::memory_order_relaxed);

  // Even though there can only be one write at a time per cpu, and we set the timer on the same cpu
  // as it triggers on, there could still be a pending write on this cpu. Since we are
  // setting/handling this timer in interrupt context, we may have started writing on the cpu and
  // then got interrupted by the timer and ended up here. Thus we need to be careful here to
  // preserve the kPendingWrite bit.
  uint64_t desired = expected | kPendingTimer | kWritesEnabled;
  do {
    // If writes aren't enabled, we need to reset the kPendingTimerBit so that the stop operation
    // knows there are no more timers on this cpu.
    if ((expected & kWritesEnabled) != kWritesEnabled) {
      // WARNING: the kWritesEnabled and kPendingTimer bits are the only things stopping another
      // thread from destroying this state. After we reset them here, `this` must not be touched.
      // Another cpu could have received a request to destroy the per_cpu_state and created a new
      // sampler.
      write_state_.fetch_and(~kPendingTimer, ktl::memory_order_release);
      return;
    }
  } while (!write_state_.compare_exchange_weak(expected, desired, ktl::memory_order_acq_rel,
                                               ktl::memory_order_relaxed));

  // We got it, we're safe to schedule the timer -- clean up will wait until we reset the
  // kPendingTimer bit.
  Deadline deadline = Deadline::after_mono(period_);
  timer.Set(deadline, Thread::SignalSampleStack, this);
}

bool PerCpuState::CancelTimer() {
  bool canceled = timer.Cancel();
  if (canceled) {
    write_state_.fetch_and(~kPendingTimer, ktl::memory_order_release);
  }
  return canceled;
}
}  // namespace sampler::internal
