// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_KTRACE_INCLUDE_LIB_KTRACE_KTRACE_INTERNAL_H_
#define ZIRCON_KERNEL_LIB_KTRACE_INCLUDE_LIB_KTRACE_KTRACE_INTERNAL_H_

#include <assert.h>
#include <lib/fit/function.h>
#include <lib/fxt/interned_category.h>
#include <lib/fxt/serializer.h>
#include <lib/user_copy/user_ptr.h>
#include <lib/zircon-internal/ktrace.h>
#include <lib/zx/result.h>
#include <stdint.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <arch/user_copy.h>
#include <kernel/lockdep.h>
#include <kernel/mutex.h>
#include <kernel/spinlock.h>
#include <ktl/atomic.h>
#include <ktl/forward.h>
#include <ktl/move.h>

// Fwd decl of tests to allow friendship.
namespace ktrace_tests {
class TestKTraceState;
}

namespace internal {

class KTraceState {
 public:
  ////////////////////////////////////////////////////////////////
  //
  // Notes on KTrace operating modes.
  //
  // KTrace can currently operate in one of two different modes, either
  // "Saturate" or "Circular".
  //
  // During saturating operation, if an attempt is made to write a record to the
  // ktrace buffer, but there is not enough room to write the record, then the
  // buffer has become "saturated".  The record is dropped, and the group mask
  // is cleared, preventing new writes from occurring until the trace is
  // restarted.
  //
  // During circular operation, if an attempt is made to write a record to the
  // ktrace buffer, but there is not enough room to write the record, then old
  // records are discarded from the trace buffer in order to make room for new
  // records.
  //
  // After a rewind operation, but before starting, the buffer is effectively
  // operating in saturating mode for the purposes of recording static data such
  // as the names of probes and threads in the system at the start of tracing.
  // Afterwards, if the trace is then started in circular mode, the KTraceState
  // instance remembers the point in the buffer where the static records ended,
  // and the circular portion of the buffer starts.  Records from the static
  // region of the trace will never be purged from the trace to make room for
  // new records recorded while in circular mode.
  //
  // A trace may be started, stopped, and started again in Saturate mode any
  // number of times without rewinding.  Additionally, a trace which has
  // previously been started in Saturate mode may subsequently be started in
  // Circular mode without rewinding.  All records recorded while in saturate
  // mode will be part of the static region of the buffer.  It is, however, not
  // legal to start a trace in Circular mode, then stop it, and then attempt to
  // start it again in Saturate mode.
  enum class StartMode { Saturate, Circular };

  constexpr KTraceState() = default;
  virtual ~KTraceState();

  // Initialize the KTraceState instance, may only be called once.  Any methods
  // called on a KTraceState instance after construction, but before Init,
  // should behave as no-ops.
  //
  // |target_bufsize| : The target size (in bytes) of the ktrace buffer to be
  // allocated.  Must be a multiple of 8 bytes.
  //
  // |initial_groups| : The initial set of enabled trace groups (see
  // zircon-internal/ktrace.h).  If non-zero, causes Init to attempt to allocate
  // the trace buffer immediately.  If the allocation fails, or the initial
  // group mask is zero, allocation is delayed until the first time that start
  // is called.
  //
  void Init(uint32_t target_bufsize, uint32_t initial_groups) TA_EXCL(lock_, write_lock_);

  [[nodiscard]] zx_status_t Start(uint32_t groups, StartMode mode) TA_EXCL(lock_, write_lock_);
  [[nodiscard]] zx_status_t Stop() TA_EXCL(lock_, write_lock_);
  [[nodiscard]] zx_status_t Rewind() TA_EXCL(lock_, write_lock_) {
    Guard<Mutex> guard(&lock_);
    return RewindLocked();
  }

  ssize_t ReadUser(user_out_ptr<void> ptr, uint32_t off, size_t len) TA_EXCL(lock_, write_lock_);

  uint32_t grpmask() const {
    return static_cast<uint32_t>(grpmask_.load(ktl::memory_order_acquire));
  }

  bool IsCategoryEnabled(const fxt::InternedCategory& category) const {
    const uint32_t bit_number = category.index();
    const uint32_t bitmask =
        bit_number != fxt::InternedCategory::kInvalidIndex ? 1u << bit_number : 0;
    return (bitmask & grpmask()) != 0;
  }

  // Atomically increments in-flight-writes iff writes are enabled and returns true.
  //
  // Returns false if the value was not incremented because writes are not enabled.
  [[nodiscard]] bool IncPendingWrite() {
    uint64_t desired;
    uint64_t expected = write_state_.load(std::memory_order_relaxed);
    do {
      // Are writes enabled?
      if ((expected & kWritesEnabledMask) == 0) {
        return false;
      }
      desired = expected + 1;
    } while (!write_state_.compare_exchange_weak(expected, desired, std::memory_order_acq_rel,
                                                 std::memory_order_relaxed));
    // Did we overflow?
    DEBUG_ASSERT((desired & kWritesInFlightMask) > 0);
    return true;
  }

  void DecPendingWrite() {
    [[maybe_unused]] uint64_t previous_value = write_state_.fetch_sub(1, ktl::memory_order_release);
    DEBUG_ASSERT((previous_value & kWritesInFlightMask) > 0);
  }

  // A RAII that implements the FXT Writer protocol and automatically commits the record after a
  // successful reservation.
  class PendingCommit {
   public:
    PendingCommit(uint64_t* ptr, uint64_t header, KTraceState* ks)
        : ptr_{ptr}, header_{header}, ks_{ks} {}

    // No copy.
    PendingCommit(const PendingCommit&) = delete;
    PendingCommit& operator=(const PendingCommit&) = delete;

    // Yes move.
    PendingCommit(PendingCommit&& other) noexcept { *this = ktl::move(other); }

    PendingCommit& operator=(PendingCommit&& other) noexcept {
      ptr_ = other.ptr_;
      header_ = other.header_;
      ks_ = other.ks_;
      other.ptr_ = nullptr;
      other.ks_ = nullptr;
      return *this;
    }

    // Going out of scope is what triggers the commit.
    ~PendingCommit() {
      if (ptr_ != nullptr) {
        ktl::atomic_ref(*ptr_).store(header_, ktl::memory_order_release);
        ks_->DecPendingWrite();
      }
    }

    void WriteWord(uint64_t word) {
      ptr_[word_offset_] = word;
      word_offset_++;
    }

    void WriteBytes(const void* bytes, size_t num_bytes) {
      const size_t num_words = (num_bytes + 7) / 8;
      // Write 0 to the last word to cover any padding bytes.
      ptr_[word_offset_ + num_words - 1] = 0;
      memcpy(&ptr_[word_offset_], bytes, num_bytes);
      word_offset_ += num_words;
    }

    void Commit() {}

   private:
    size_t word_offset_{1};
    uint64_t* ptr_{nullptr};
    uint64_t header_{0};
    KTraceState* ks_{nullptr};
  };

  // Reserve enough bytes of contiguous space in the buffer to fit the FXT Record described by
  // `header`, if possible.
  zx::result<PendingCommit> Reserve(uint64_t header) {
    if (!IncPendingWrite()) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
    uint64_t* const ptr = ReserveRaw(fxt::RecordFields::RecordSize::Get<uint32_t>(header));
    if (ptr == nullptr) {
      ClearMaskDisableWrites();
      DecPendingWrite();
      return zx::error(ZX_ERR_NO_MEMORY);
    }
    return zx::ok(PendingCommit(ptr, header, this));
  }

 private:
  friend class ktrace_tests::TestKTraceState;

  [[nodiscard]] zx_status_t RewindLocked() TA_REQ(lock_);

  // Add static names (eg syscalls and probes) to the trace buffer.  Called
  // during a rewind operation immediately after resetting the trace buffer.
  // Declared as virtual to facilitate testing.
  virtual void ReportStaticNames() TA_REQ(lock_);

  // Add the names of current live threads and processes to the trace buffer.
  // Called during start operations just before setting the group mask. Declared
  // as virtual to facilitate testing.
  virtual void ReportThreadProcessNames() TA_REQ(lock_);

  // Copy data from kernel memory to user memory.  Used by Read, and overloaded
  // by test code (which needs to copy to kernel memory, not user memory).
  virtual zx_status_t CopyToUser(user_out_ptr<uint8_t> dst, const uint8_t* src, size_t len) {
    return dst.copy_array_to_user(src, len);
  }

  // A small printf stand-in which gives tests the ability to disable diagnostic
  // printing during testing.
  int DiagsPrintf(int level, const char* fmt, ...) const __PRINTFLIKE(3, 4) {
    if (!disable_diags_printfs_ && DPRINTF_ENABLED_FOR_LEVEL(level)) {
      va_list args;
      va_start(args, fmt);
      int result = vprintf(fmt, args);
      va_end(args);
      return result;
    }
    return 0;
  }

  // Attempt to allocate our buffer, if we have not already done so.
  zx_status_t AllocBuffer() TA_REQ(lock_);

  // Reserve the given number of words in the trace buffer. Returns nullptr if the reservation
  // fails.
  uint64_t* ReserveRaw(uint32_t num_words);

  // Set the group mask, but don't modify the writes-enable state.
  void SetGroupMask(uint32_t new_mask) { grpmask_.store(new_mask, ktl::memory_order_release); }

  // Enable writes, but don't modify the group mask.
  void EnableWrites() { write_state_.fetch_or(kWritesEnabledMask, ktl::memory_order_release); }

  // Clear the group mask and disable writes.
  void ClearMaskDisableWrites() {
    grpmask_.store(0, ktl::memory_order_release);
    write_state_.fetch_and(~kWritesEnabledMask, ktl::memory_order_release);
  }

  // Convert an absolute read or write pointer into an offset into the circular
  // region of the buffer.  Note that it is illegal to call this if we are not
  // operating in circular mode.
  uint32_t PtrToCircularOffset(uint64_t ptr) const TA_REQ(write_lock_) {
    DEBUG_ASSERT(circular_size_ > 0);
    return static_cast<uint32_t>((ptr % circular_size_) + wrap_offset_);
  }

  uint32_t inflight_writes() const {
    return static_cast<uint32_t>(write_state_.load(ktl::memory_order_acquire) &
                                 kWritesInFlightMask);
  }

  // Allow diagnostic dprintf'ing or not.  Overridden by test code.
  bool disable_diags_printfs_{false};

  // An atomic state variable which tracks whether writes are enabled (bit 63)
  // and the number of writes in-flight (bits 0-62).
  //
  // Write operations consist of:
  //
  // 1) Optionally observing the group mask with acquire semantics to determine
  //    if the category is enabled for this write.
  // 2) Atomically checking whether writes are enabled and incrementing the
  //    in-flight count with acq/rel semantics.
  // 3) Completing or aborting the write operation.
  // 4) Decrementing the in-flight count portion of the state with release
  //    semantics to indicate that the write is finished.
  //
  // This allows Stop operations to synchronize with any in-flight writes by:
  //
  // 1) Clearing the writes-enabled bit with release semantics.
  // 2) Spinning on the in-flight-writes with acquire semantics until an
  // in-flight count of zero is observed.
  //
  //
  // Notes:
  //
  // * Once a writer has incremented the in-flight count, they must also
  // decrement in a reasonable amount of time to ensure a trace can be stopped.
  //
  // * The algorithm above has a race (the ABA problem) where a writer may end
  // up writing a record of a category that's not enabled.  Consumers of the
  // data are expected to handle finding unexpected records in the trace buffer.
  // The race goes like this: Category A is enabled and a writer is attempting
  // to emit a record for category A.  The writer checks the group mask (step
  // 1), sees that A is enabled and proceeds to step 2.  Prior to executing step
  // 2, a different thread stops the trace, disabling writes and clearing the
  // group mask.  A new trace is started with only category B enabled.  The
  // writer resumes step 2 and atomically checks that writes are enabled,
  // increments the in-flight count and proceeds to write a category A record.

  // Bit-wise AND with |write_state_| to read writes-enabled.
  static constexpr uint64_t kWritesEnabledMask = 1ul << 63;
  // Bit-wise AND with |write_state_| to read in-flight-writes count.
  static constexpr uint64_t kWritesInFlightMask = ~kWritesEnabledMask;

  ktl::atomic<uint64_t> write_state_{0};

  ktl::atomic<uint32_t> grpmask_{0};

  // The target buffer size (in bytes) we would like to use, when we eventually
  // call AllocBuffer.  Set during the call to Init.
  uint32_t target_bufsize_{0};

  // A lock used to serialize all non-write operations.  IOW - this lock ensures
  // that only a single thread at a time may be involved in operations such as
  // Start, Stop, Rewind, and ReadUser
  DECLARE_MUTEX(KTraceState) lock_;
  bool is_started_ TA_GUARDED(lock_){false};

  // The core allocation state of the trace buffer, protected by the write
  // spinlock.  See "Notes on KTrace operating modes" (above) for details on
  // saturate vs. circular mode.  This comment will describe how the bookkeeping
  // maintained in each of the two modes, how wrapping is handled in circular
  // mode, and how space for records in the buffer is reserved and subsequently
  // committed.
  //
  // --== Saturate mode ==--
  //
  // While operating in saturate mode, the value of |circular_size_| and |rd_|
  // will always be 0, and the value of |wrap_offset_| is not defined.  The only
  // important piece of bookkeeping maintained is the value of |wr_|.  |wr_|
  // always points to the offset in the buffer where the next record will be
  // stored, and it should always be <= |bufsize_|.  When reading back records,
  // the first record will always be located at offset 0.
  //
  // --== Circular mode ==--
  //
  // When operating in circular mode, the buffer is partitioned into two
  // regions; a "static" region which contains the records recorded before
  // entering circular mode, and a circular region which contain records written
  // after beginning circular operation.  |circular_size_| must be non-zero, and
  // contains the size (in bytes) of the circular region of the buffer.  The
  // region of the buffer from [0, wrap_offset_) is the static region of the
  // buffer, while the region from [wrap_offset_, bufsize_) is the circular
  // region.  |wrap_offset_| must always be < |bufsize_|.
  //
  // The |rd_| and |wr_| pointers are absolute offsets into the circular region
  // of the buffer, modulo |circular_size_|.  When space in the buffer is
  // reserved for a record, |wr_| is incremented by the size of the record.
  // When a record is purged to make room for new records, |rd_| is incremented.
  // At all times, |rd_| <= |wr_|, and both pointers are monotonically
  // increasing.  The function which maps from one of these pointers to an
  // offset in the buffer (on the range [0, bufsize_)) is given by
  //
  //   f(ptr) = (ptr % circular_size_) + wrap_offset_
  //
  // --== Reserving records and memory ordering ==--
  //
  // In order to write a record to the trace buffer, the writer must first
  // reserve the space to do so.  During this period of time, the |write_lock_|
  // is held while the bookkeeping is handled in order to reserve space.
  // Holding the write lock during reservation guarantees coherent observations
  // of the bookkeeping state by the writers.
  //
  // If the reservation succeeds, the tag field of the reserved record is stored
  // as 0 with release semantics, then the write lock is dropped in order to
  // allow other reservations to take place concurrently while the payload of
  // the record is populated.  Once the writer has finished recording the
  // payload, it must write the final tag value for the record with release
  // semantics.  This finalizes the record, and after this operation, the
  // payload may no longer change.
  //
  // If, while operating in circular mode, an old record needs to be purged in
  // order to make space for a new record, the |rd_| pointer will simply be
  // incremented by the size of the record located at the |rd_| pointer.  The
  // tag of this record must first be read with memory order acquire semantics
  // in order to compute its length so that the |rd_| pointer may be adjusted
  // appropriately.  If, during this observation, the value of the tag is
  // observed to be 0, it means that a writer is attempting to advance the read
  // pointer past a record which has not been fully committed yet.  If this ever
  // happens, the reservation operation fails, and the group mask will be
  // cleared, just like if a reservation had failed in saturating mode.
  //
  // --== Circular mode padding ==--
  //
  // If a record of size X is to be reserved in the trace buffer while operating
  // in circular mode, and the distance between the write pointer and the end of
  // the buffer is too small for the record to be contained contiguously, a
  // "padding" record will be inserted instead.  This is a record with a record type
  // of 0 which contains no payload.  Its only purpose is to pad the buffer
  // out so that the record to be written may exist contiguously in the trace
  // buffer.
  //
  DECLARE_SPINLOCK_WITH_TYPE(KTraceState, TraceDisabledSpinLock) write_lock_;
  uint64_t rd_ TA_GUARDED(write_lock_){0};
  uint64_t wr_ TA_GUARDED(write_lock_){0};
  uint32_t circular_size_ TA_GUARDED(write_lock_){0};
  uint32_t wrap_offset_ TA_GUARDED(write_lock_){0};

  // Note: these don't _actually_ have to be protected by the write lock.
  // Memory ordering consistency for mutators of these variables are protected
  // via lock_, while observations from trace writers are actually protected by
  // a complicated set of arguments based on the stopped/started state of the
  // system, and the acq/rel semantics of the grpmask_ variable.
  //
  // Instead of relying on these complicated and difficult to
  // communicate/enforce invariants, however, we just toss these variables into
  // the write lock and leave it at that.  Trace writers already needed to be
  // inside of the write lock to manipulate the read/write pointers while
  // reserving space.  Mutation of these variables can only happen during
  // start/init when the system is stopped (and there are no writers), so
  // obtaining the write lock to allocate the buffer is basically free since it
  // will never be contested.
  //
  uint8_t* buffer_ TA_GUARDED(write_lock_){nullptr};
  uint32_t bufsize_ TA_GUARDED(write_lock_){0};
};

}  // namespace internal

#endif  // ZIRCON_KERNEL_LIB_KTRACE_INCLUDE_LIB_KTRACE_KTRACE_INTERNAL_H_
