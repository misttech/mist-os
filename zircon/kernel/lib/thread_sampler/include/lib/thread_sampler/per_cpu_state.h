// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_THREAD_SAMPLER_INCLUDE_LIB_THREAD_SAMPLER_PER_CPU_STATE_H_
#define ZIRCON_KERNEL_LIB_THREAD_SAMPLER_INCLUDE_LIB_THREAD_SAMPLER_PER_CPU_STATE_H_
#include <lib/thread_sampler/buffer_writer.h>

#include <kernel/lockdep.h>
#include <kernel/mutex.h>
#include <object/io_buffer_dispatcher.h>
#include <vm/pinned_vm_object.h>

// These types are internal implementation details to the thread sampler.
namespace sampler::internal {

// When we start sampling, we assign and pin a VmObject buffer to each CPU for it to write on. We
// also assign a per cpu timer. When the timer triggers, we mark a thread to write a sample to the
// buffer. When the thread will at some point in the future when it is safe to do so, write a
// sample.
//
// This creates the main problem: how do we safely clean up when we stop? If we are handling a stop
// request on one core, it's not safe to immediately clean up the buffers since another core could
// be writing a sample or could be scheduled to write a sample. To do this safely, we have a three
// phase shutdown:
//
// 1) Disable new writes and new timers
// 2) Cancel existing timers and wait for any lingering timers to clear out
// 3) Wait for any in progress writes to stop
//
// Now it's safe to destroy our state.
//
// So then, each cpu state also contains an atomic variable with 3 bits: a writes enabled bit, a
// pending write bit, and a timer pending bit. When a cpu wants to write, it atomically checks if
// the write enabled bit is set, and if so, sets the pending write bit and checks the write-enabled
// bit again before proceeding. When it is done, it resets the pending write bit, ignoring the
// writes enabled bit.
//
// The timer callback receives a pointer to the PerCpuState and the timer pending bit ensures that
// the per cpu state lives longer than the timer callback.
//
// Now, when we want to clean up, on each cpu, we reset the writes enabled bit, knowing that no new
// pending writes or timers can start. We then wait for the pending write bit and pending timer bit
// to reset on each cpu. Only then do we know that no additional writes will occur.
class PerCpuState {
 public:
  constexpr PerCpuState() = default;

  zx::result<> SetUp(const zx_sampler_config_t& config, PinnedVmObject pinned_memory);

  // Atomically mark a pending write to the write state iff writes are enabled and returns true.
  //
  // Returns false if a pending write was not set because writes are not enabled.
  [[nodiscard]] bool SetPendingWrite();
  void ResetPendingWrite();

  void EnableWrites();
  void DisableWrites();

  // True if the kWritesEnabled bit is set
  bool WritesEnabled() const;

  // True if the kPendingWrite bit is set
  bool PendingWrites() const;

  // True if the kPendingTimer bit is set
  bool PendingTimer() const;

  // Atomically set a timer if writes are enabled. When the timer goes off, whatever thread is
  // currently active will be signaled to be sampled.
  void SetTimer();

  // True if the per cpu timer was canceled before it was scheduled else false
  bool CancelTimer();

  // Reserve space in the assigned pinned memory. The AllocatedRecord will ensure the underlying
  // buffers live long enough to write to.
  zx::result<AllocatedRecord> Reserve(uint64_t header) { return writer.Reserve(header); }

 private:
  BufferWriter writer;
  Timer timer;
  zx_duration_mono_t period_{0};

  // Bit-wise AND with |write_state_| to read if there is a current write
  static constexpr uint64_t kPendingWrite = 1ul << 0;

  // Bit-wise AND with |write_state_| to read if a sample is pending via the timer
  static constexpr uint64_t kPendingTimer = 1ul << 1;

  // Bit-wise AND with |write_state_| to read if writes are enabled
  static constexpr uint64_t kWritesEnabled = 1ul << 2;
  ktl::atomic<uint64_t> write_state_{0};
};

}  // namespace sampler::internal
#endif  // ZIRCON_KERNEL_LIB_THREAD_SAMPLER_INCLUDE_LIB_THREAD_SAMPLER_PER_CPU_STATE_H_
