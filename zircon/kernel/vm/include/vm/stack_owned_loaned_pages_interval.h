// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_STACK_OWNED_LOANED_PAGES_INTERVAL_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_STACK_OWNED_LOANED_PAGES_INTERVAL_H_

#include <kernel/owned_wait_queue.h>
#include <kernel/thread.h>
#include <ktl/optional.h>

struct vm_page;

// This class establishes a RAII style code interval (while an instance of this class is on the
// stack).  During this interval, it is permissible to stack-own a loaned page.
//
// Intervals are allowed to nest.  The outermost interval (technically: first constructed) is the
// interval that applies.
//
// A thread that wants to wait for a loaned page to no longer be stack-owned can call
// WaitUntilContiguousPageNotStackOwned().  The wait will participate in priority inheritance which
// will boost the stack-owning thread to at least the priority of the waiting thread for the
// duration of the wait.
//
// At least for now, instances of this class are only meant to exist on the stack.  Heap allocation
// of an instance of this class is not currently supported, and will fail asserts if the destruction
// thread doesn't match the construction thread (and possibly other asserts).
class StackOwnedLoanedPagesInterval {
 public:
  // No copy or move.
  StackOwnedLoanedPagesInterval(const StackOwnedLoanedPagesInterval& to_copy) = delete;
  StackOwnedLoanedPagesInterval& operator=(const StackOwnedLoanedPagesInterval& to_copy) = delete;
  StackOwnedLoanedPagesInterval(StackOwnedLoanedPagesInterval&& to_move) = delete;
  StackOwnedLoanedPagesInterval& operator=(StackOwnedLoanedPagesInterval&& to_move) = delete;

  // The owning_thread_ state of this interval is either the current thread if
  // this is the outermost interval on the current thread's stack, or it is
  // nullptr.  Blocking threads will only ever interact with the outermost
  // interval; subsequent intervals placed on the stack by the current thread
  // are basically glorified ref-counts.
  StackOwnedLoanedPagesInterval()
      : owning_thread_{Thread::Current::Get()->stack_owned_loaned_pages_interval_ == nullptr
                           ? Thread::Current::Get()
                           : nullptr} {
    // If this was the outermost interval, record that in the current thread's
    // bookkeeping.
    if (owning_thread_ != nullptr) {
      owning_thread_->stack_owned_loaned_pages_interval_ = this;
    }
  }

  ~StackOwnedLoanedPagesInterval() {
    canary_.Assert();
    Thread* current_thread = Thread::Current::Get();
    // only remove if this is the outermost interval, which is likely
    if (likely(current_thread->stack_owned_loaned_pages_interval_ == this)) {
      DEBUG_ASSERT(owning_thread_);
      DEBUG_ASSERT(owning_thread_ == current_thread);
      current_thread->stack_owned_loaned_pages_interval_ = nullptr;
      if (unlikely(is_ready_for_waiter_.load(ktl::memory_order_acquire))) {
        // In the much more rare case that there are any waiters, wake all waiters and clear out the
        // owner before destructing owned_wait_queue_.  We do this out-of-line since it's not the
        // common path.
        WakeWaitersAndClearOwner(current_thread);
      }
      // PrepareForWaiter() was never called, so no need to acquire lock_.  This
      // is very likely.  Done.
    }
  }

  static auto& get_lock() TA_RET_CAP(lock_) { return lock_; }
  static StackOwnedLoanedPagesInterval& current();
  static StackOwnedLoanedPagesInterval* maybe_current();
  static void WaitUntilContiguousPageNotStackOwned(vm_page* page)
      TA_EXCL(chainlock_transaction_token, lock_);

 private:
  static inline DECLARE_SPINLOCK(StackOwnedLoanedPagesInterval) lock_;

  // This sets up to permit a waiter, and asserts that the calling thread is not the constructing
  // thread, since waiting by the constructing/destructing thread would block (or maybe fail).
  void PrepareForWaiter() TA_REQ(lock_, preempt_disabled_token);
  void WakeWaitersAndClearOwner(Thread* current_thread) TA_EXCL(lock_);

  // magic value
  fbl::Canary<fbl::magic("SOPI")> canary_;

  // We stash the owning thread as part of delaying
  // OwnedWaitQueue::AssignOwner(), to avoid putting unnecessary pressure on
  // lock_ when there's no waiter.
  Thread* const owning_thread_;

  // This is atomic because in the common case of no waiter, the thread with
  // StackOwnedLoanedPagesInterval on its stack doesn't need to acquire the
  // lock_ to read false from here during destruction (and so never needs
  // to acquire lock_).
  ktl::atomic<bool> is_ready_for_waiter_{false};

  // In the common case of no waiter, this never gets constructed or destructed.
  // We only construct this on PrepareForWaiter() during
  // WaitUntilContiguousPageNotStackOwned().
  ktl::optional<OwnedWaitQueue> owned_wait_queue_;
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_STACK_OWNED_LOANED_PAGES_INTERVAL_H_
