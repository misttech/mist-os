// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2008-2015 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_THREAD_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_THREAD_H_

#include <arch.h>
#include <debug.h>
#include <lib/backtrace.h>
#include <lib/concurrent/capability_token.h>
#include <lib/fit/function.h>
#include <lib/fxt/thread_ref.h>
#include <lib/kconcurrent/chainlock.h>
#include <lib/relaxed_atomic.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/result.h>
#include <platform.h>
#include <sys/types.h>
#include <zircon/compiler.h>
#include <zircon/listnode.h>
#include <zircon/syscalls/object.h>
#include <zircon/syscalls/scheduler.h>
#include <zircon/types.h>

#include <arch/arch_thread.h>
#include <arch/defines.h>
#include <arch/exception.h>
#include <arch/ops.h>
#include <fbl/canary.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/macros.h>
#include <fbl/null_lock.h>
#include <fbl/wavl_tree_best_node_observer.h>
#include <kernel/cpu.h>
#include <kernel/deadline.h>
#include <kernel/koid.h>
#include <kernel/preempt_disabled_token.h>
#include <kernel/restricted_state.h>
#include <kernel/scheduler_state.h>
#include <kernel/spinlock.h>
#include <kernel/task_runtime_stats.h>
#include <kernel/timer.h>
#include <ktl/array.h>
#include <ktl/atomic.h>
#include <ktl/optional.h>
#include <ktl/string_view.h>
#include <lockdep/thread_lock_state.h>
#include <vm/kstack.h>

class Dpc;
struct BrwLockOps;
class OwnedWaitQueue;
class StackOwnedLoanedPagesInterval;
class ThreadDispatcher;
class VmAspace;
class WaitQueue;
struct WaitQueueLockOps;
struct Thread;
class ThreadDumper;  // A class which is a friend of the various internal thread
                     // state classes and structures, allowing it to access
                     // private variables for debugging purposes during debug
                     // dumps of thread state.

// These forward declarations are needed so that Thread can friend
// them before they are defined.
static inline Thread* arch_get_current_thread();
static inline void arch_set_current_thread(Thread*);

// When blocking this enum indicates the kind of resource ownership that is being waited for
// that is causing the block.
enum class ResourceOwnership {
  // Blocking is either not for any particular resource, or it is to wait for
  // exclusive access to a resource.
  Normal,
  // Blocking is happening whilst waiting for shared read access to a resource.
  Reader,
};

// The types of affinity the scheduler understands.
enum class Affinity { Hard, Soft };

static inline fbl::NullLock is_current_thread_token;

// Whether a block or a sleep can be interrupted.
enum class Interruptible : bool { No, Yes };

// When signaling to a wait queue that the priority of one of its blocked
// threads has changed, this enum is used as a signal indicating whether or not
// the priority change should be propagated down the PI chain (if any) or not.
enum class PropagatePI : bool { No = false, Yes };

// A WaitQueueCollection is the data structure which holds a collection of
// threads which are currently blocked in a wait queue.  The data structure
// imposes a total ordering on the threads meant to represent the order in which
// the threads should be woken, from "most important" to "least important".
//
// One unusual property of the ordering implemented by a WaitQueueCollection is
// that, unlike an ordering determined completely by properties such as thread
// priority or weight, it is dynamic with respect to time.  This is to say that
// while at any instant in time there is always a specific order to the threads,
// as time advances, this order can change.  The ordering itself is determined
// by the nature of the various dynamic scheduling disciplines implemented by
// the Zircon scheduler.
//
// At any specific time |now|, the order of the collection is considered to
// be:
//
// 1) The deadline threads in the collection whose absolute deadlines are in the
//    future, sorted by ascending absolute deadline.  These are the threads who
//    still have a chance of meeting their absolute deadline, with the nearest
//    absolute deadline considered to be the most important.
// 2) The deadline threads in the collection whose absolute deadlines are in the
//    past, sorted by ascending relative deadline.  These are the threads who
//    have been blocked until after their last cycle's absolute deadline.  If
//    all threads were to be woken |now|, the thread with the minimum relative
//    deadline would be the thread which has the new absolute deadline across
//    the set.
// 3) The fair threads in the collection, sorted by their "virtual finish time".
//    This is equal to the start time of the thread plus the scheduler's maximum
//    target latency divided by the thread's weight (normalized to the range
//    (0.0, 1.0].  This is the same ordering imposed by the Scheduler's RunQueue
//    for fair threads, and is intended to prioritize higher weight threads,
//    while still ensuring some level of fairness over time.  The start time
//    represents the last time that a thread entered a run queue, and while high
//    weight threads will be chosen before low weight threads who arrived at
//    similar times, threads who arrived earlier (and have been waiting for
//    longer) will eventually end up being chosen, no matter how much weight
//    other threads in the collection have compared to it.
//    TODO(johngro): Instead of using the start time for the last time a
//    thread entered a RunQueue, should we use the time at which the thread
//    joined the wait queue instead?
//
// In an attempt to make the selection of the "best" thread in a wait queue as
// efficient as we can, in light of the dynamic nature of the total ordering, we
// use an "augmented" WAVL tree as our data structure, much like the scheduler's
// RunQueue.  The tree keeps all of its threads sorted according to a primary
// key representing the minimum absolute deadline or a modified version its
// virtual finish time, depending on the thread's scheduling discipline).
//
// The virtual finish time of threads is modified so that the MSB of the time is
// always set. This guarantees that fair threads _always_ come after in the
// sorting of threads.  Note that we could have also achieved this partitioning
// by tracking fair threads separately from deadline thread in a separate tree
// instance.  We keep things in a single tree (for now) in order to help to
// minimize the size of WaitQueueCollections to help control the size of objects
// in the kernel (such as the Mutex object).
//
// There should be no serious issue with using the MSB of the sort key in this
// fashion.  Absolute timestamps in zircon use signed 64 bit integers, and the
// monotonic clock is set at startup to start from zero, meaning that there is
// no real world case where we would be searching for a deadline thread to
// wake using a timestamp with the MSB set.
//
// Finally, we also maintain an addition augmented invariant such that: For
// every node (X) in the tree, the pointer to the thread with the minimum
// relative deadline in the subtree headed by X is maintained as nodes are
// inserted and removed.
//
// With these invariants in place, finding the best thread to run can be
// computed as follows.
//
// 1) If the left-most member of the tree has the MSB of its sorting key set,
//    then the thread is a fair thread, and there are _no_ deadline threads in
//    the tree.  Additionally, this thread has the minimum virtual finish time
//    across all of the fair threads in the tree, and therefore is the "best"
//    thread to unblock.  When the tree is in this state, selection is O(1).
// 2) Otherwise, there are deadline threads in the tree.  The tree is searched
//    to find the first thread whose absolute deadline is in the future,
//    relative to |now|.  If such a thread exists, then it is the "best" thread
//    to run right now and it is selected.  When the tree is in this state,
//    selection is O(log).
// 3) If there are no threads whose deadlines are in the future, the pointer to
//    the thread with the minimum relative deadline in the tree is chosen,
//    simply by fetching the best-in-subtree pointer maintained in |root()|.
//    While this operation is O(1), when the tree is this state, the over all
//    achieved order was O(log) because of the search which needed to happen
//    during step 2.
//
// Insert and remove order for the tree should be:
// 1) Insertions into the tree are always O(log).
// 2) Unlike a typical WAVL tree, removals of a specific thread from the tree
//    are O(log) instead of being amortized constant.  This is because of the
//    cost of restoring the augmented invariant after removal, which involves
//    walking from the point of removal up to the root of the tree.
//
class WaitQueueCollection {
 private:
  // fwd decls
  struct BlockedThreadTreeTraits;
  struct MinRelativeDeadlineTraits;

 public:
  using Key = ktl::pair<uint64_t, uintptr_t>;

  // Encapsulation of all the per-thread state for the WaitQueueCollection data structure.
  class ThreadState {
   public:
    ThreadState() = default;

    ~ThreadState();

    // Disallow copying.
    ThreadState(const ThreadState&) = delete;
    ThreadState& operator=(const ThreadState&) = delete;

    bool InWaitQueue() const { return blocked_threads_tree_node_.InContainer(); }

    zx_status_t BlockedStatus() const { return blocked_status_; }

    void Block(Thread* current_thread, Interruptible interruptible, zx_status_t status)
        TA_REQ(chainlock_transaction_token, ChainLockable::GetLock(*current_thread));

    void Unsleep(Thread* thread, zx_status_t status) TA_REQ(chainlock_transaction_token)
        TA_REL(ChainLockable::GetLock(*thread));

    void AssertNoOwnedWaitQueues() const {}

    void AssertNotBlocked() const {
      DEBUG_ASSERT(blocking_wait_queue_ == nullptr);
      DEBUG_ASSERT(!InWaitQueue());
    }

    WaitQueue* blocking_wait_queue() { return blocking_wait_queue_; }
    const WaitQueue* blocking_wait_queue() const { return blocking_wait_queue_; }
    Interruptible interruptible() const { return interruptible_; }

   private:
    // WaitQueues, WaitQueueCollections, and their List types, can
    // directly manipulate the contents of the per-thread state, for now.
    friend struct BrwLockOps;
    friend class OwnedWaitQueue;
    friend class Scheduler;
    friend class WaitQueue;
    friend class WaitQueueCollection;
    friend struct WaitQueueCollection::BlockedThreadTreeTraits;
    friend struct WaitQueueCollection::MinRelativeDeadlineTraits;

    // Dumping routines are allowed to see inside us.
    friend class ThreadDumper;

    // If blocked, a pointer to the WaitQueue the Thread is on.
    WaitQueue* blocking_wait_queue_ = nullptr;

    // Node state for existing in WaitQueueCollection::threads_
    fbl::WAVLTreeNodeState<Thread*> blocked_threads_tree_node_;

    // Primary key used for determining our position in the collection of
    // blocked threads. Pre-computed during insert in order to save a time
    // during insert, rebalance, and search operations.
    uint64_t blocked_threads_tree_sort_key_{0};

    // State variable holding the pointer to the thread in our subtree with the
    // minimum relative deadline (if any).
    Thread* subtree_min_rel_deadline_thread_{nullptr};

    // Return code if woken up abnormally from suspend, sleep, or block.
    zx_status_t blocked_status_ = ZX_OK;

    // Are we allowed to be interrupted on the current thing we're blocked/sleeping on?
    Interruptible interruptible_ = Interruptible::No;

    // Storage used by an OwnedWaitQueue, but held within a thread
    // instance, while that thread is blocked in the wait queue.
    SchedulerState::WaitQueueInheritedSchedulerState inherited_scheduler_state_storage_{};
  };

  constexpr WaitQueueCollection() {}
  ~WaitQueueCollection() {
    Validate();
    DEBUG_ASSERT(threads_.is_empty());
  }

  void Validate() const {
    // TODO(johngro): We could perform a more rigorous check of the two maintained
    // invariants of the threads_ collection, however we probably only want to do
    // so if kSchedulerExtraInvariantValidation is true.
  }

  // Passthrus for the underlying container's size and is_empty methods.
  uint32_t Count() const { return static_cast<uint32_t>(threads_.size()); }
  bool IsEmpty() const { return threads_.is_empty(); }

  // The current minimum inheritable relative deadline of the set of blocked threads.
  SchedDuration MinInheritableRelativeDeadline() const;

  // Peek at the first Thread in the collection.
  Thread* Peek(zx_instant_mono_t now);
  const Thread* Peek(zx_instant_mono_t now) const {
    return const_cast<WaitQueueCollection*>(this)->Peek(now);
  }

  Thread& PeekOnlyThread() {
    DEBUG_ASSERT_MSG(threads_.size() == 1, "Expected size 1, not %zu", threads_.size());
    return threads_.front();
  }

  Thread* PeekFront() { return threads_.is_empty() ? nullptr : &threads_.front(); }

  inline SchedulerState::WaitQueueInheritedSchedulerState* FindInheritedSchedulerStateStorage();

  // Add the Thread into its sorted location in the collection.
  void Insert(Thread* thread) TA_REQ(ChainLockable::GetLock(*thread));

  // Remove the Thread from the collection.
  void Remove(Thread* thread) TA_REQ(ChainLockable::GetLock(*thread));

  // Either lock every thread in the collection, or failed with
  // ChainLock::Result::Backoff, releasing any locks which were obtained in the
  // process. ASSERTs if any cycles are detected by the ChainLock. Used by
  // WaitQueue WakeAll.  Note that it is not possible to statically annotated
  // this, and needs to be used with extreme care.  If LockAll returns success,
  // it is critical that the caller (eventually) drops all of the locks.
  ChainLock::Result LockAll() TA_REQ(chainlock_transaction_token);

  // Const accessor used in some debug/validation code.
  const auto& threads() const { return threads_; }

  // Disallow copying and moving.
  WaitQueueCollection(const WaitQueueCollection&) = delete;
  WaitQueueCollection& operator=(const WaitQueueCollection&) = delete;

  WaitQueueCollection(WaitQueueCollection&&) = delete;
  WaitQueueCollection& operator=(WaitQueueCollection&&) = delete;

 private:
  static constexpr uint64_t kFairThreadSortKeyBit = uint64_t{1} << 63;

  struct BlockedThreadTreeTraits {
    static Key GetKey(const Thread& thread);
    static bool LessThan(Key a, Key b) { return a < b; }
    static bool EqualTo(Key a, Key b) { return a == b; }
    static fbl::WAVLTreeNodeState<Thread*>& node_state(Thread& thread);
  };

  struct MinRelativeDeadlineTraits {
    // WAVLTreeBestNodeObserver template API
    using ValueType = Thread*;
    static ValueType GetValue(const Thread& node);
    static ValueType GetSubtreeBest(const Thread& node);
    static bool Compare(ValueType a, ValueType b);
    static void AssignBest(Thread& node, ValueType val);
    static void ResetBest(Thread& target);
  };

  static inline bool IsFairThreadSortBitSet(const Thread& t)
      TA_REQ_SHARED(ChainLockable::GetLock(t));

  using BlockedThreadTree = fbl::WAVLTree<Key, Thread*, BlockedThreadTreeTraits,
                                          fbl::DefaultObjectTag, BlockedThreadTreeTraits,
                                          fbl::WAVLTreeBestNodeObserver<MinRelativeDeadlineTraits>>;
  BlockedThreadTree threads_;
};

// NOTE: must be inside critical section when using these
class WaitQueue : public ChainLockable {
 public:
  constexpr WaitQueue() : WaitQueue(kMagic) {}
  ~WaitQueue();

  WaitQueue(WaitQueue&) = delete;
  WaitQueue(WaitQueue&&) = delete;
  WaitQueue& operator=(WaitQueue&) = delete;
  WaitQueue& operator=(WaitQueue&&) = delete;

  // Remove a specific thread out of the wait queue it's blocked on, and deal
  // with any PI side effects.  Note: when calling this function:
  //
  // 1) The thread |t| must be actively blocked in the WaitQueue instance
  //    indicated by |this|.
  // 2) In addition to |t|'s lock, all of the ChainLocks downstream of |t|
  //    (starting from |t| and ending at the target of |t|'s PI graph) must be
  //    held.  Note that we are only able to statically assert that the first
  //    two of these locks; |t|'s lock, and the lock of the wait queue |t| is
  //    currently blocked in.
  // 3) During the call UnblockThread, all of the locks identified in #2 (above)
  //    will be released.
  zx_status_t UnblockThread(Thread* t, zx_status_t wait_queue_error)
      TA_REL(get_lock(), ChainLockable::GetLock(*t))
          TA_REQ(chainlock_transaction_token, preempt_disabled_token);

  // Block on a wait queue.
  // The returned status is whatever the caller of WaitQueue::Wake_*() specifies.
  // A deadline other than Deadline::infinite() will abort at the specified time
  // and return ZX_ERR_TIMED_OUT. A deadline in the past will immediately return.
  zx_status_t Block(Thread* const current_thread, const Deadline& deadline,
                    Interruptible interruptible) TA_REL(get_lock())
      TA_REQ(chainlock_transaction_token, ChainLockable::GetLock(*current_thread)) {
    return BlockEtc(current_thread, deadline, 0, ResourceOwnership::Normal, interruptible);
  }

  // Block on a wait queue with a zx_instant_mono_t-typed deadline.
  zx_status_t Block(Thread* const current_thread, zx_instant_mono_t deadline,
                    Interruptible interruptible) TA_REL(get_lock())
      TA_REQ(chainlock_transaction_token, ChainLockable::GetLock(*current_thread)) {
    return BlockEtc(current_thread, Deadline::no_slack(deadline), 0, ResourceOwnership::Normal,
                    interruptible);
  }

  // Block on a wait queue, ignoring existing signals in |signal_mask|.
  // The returned status is whatever the caller of WaitQueue::Wake_*() specifies, or
  // ZX_ERR_TIMED_OUT if the deadline has elapsed or is in the past.
  // This will never timeout when called with a deadline of Deadline::infinite().
  zx_status_t BlockEtc(Thread* current_thread, const Deadline& deadline, uint signal_mask,
                       ResourceOwnership reason, Interruptible interruptible) TA_REL(get_lock())
      TA_REQ(chainlock_transaction_token, ChainLockable::GetLock(*current_thread));

  // Returns the current highest priority blocked thread on this wait queue, or
  // nullptr if no threads are blocked.
  Thread* Peek(zx_instant_mono_t now) TA_REQ(get_lock()) { return collection_.Peek(now); }
  const Thread* Peek(zx_instant_mono_t now) const TA_REQ(get_lock()) {
    return collection_.Peek(now);
  }

  // Release one or more threads from the wait queue.
  // wait_queue_error = what WaitQueue::Block() should return for the blocking thread.
  //
  // Returns true if a thread was woken, and false otherwise.
  bool WakeOne(zx_status_t wait_queue_error) TA_EXCL(chainlock_transaction_token, get_lock());
  void WakeAll(zx_status_t wait_queue_error) TA_EXCL(chainlock_transaction_token, get_lock());

  // Locked versions of the wake calls.  These calls are going to need to obtain
  // locks for each of the threads woken, which could result in needing to back
  // off and start the operation again. Each routine returns a
  // std::optional (bool or u32).
  //
  // ++ If the optional holds a value, then the value is the number of threads
  //    which were woken, and the operation succeeded.
  // ++ Otherwise, the operation failed with a Backoff error and the queue's
  //    lock needs to be dropped before trying again.
  //
  // It is assumed that forming a lock cycle should be impossible.  If such a
  // cycle is detected (like, if the caller was holding the lock of one of the
  // thread's blocked in this queue at the time of the call) it will trigger a
  // DEBUG_ASSERT.
  //
  // Either way, unlike the non-Locked versions of these routines, the wait
  // queue's lock is held for the duration of the operation, instead of being
  // release as soon as possible (during the call to SchedulerUnlock)
  ktl::optional<bool> WakeOneLocked(zx_status_t wait_queue_error)
      TA_REQ(chainlock_transaction_token, get_lock(), preempt_disabled_token);
  ktl::optional<uint32_t> WakeAllLocked(zx_status_t wait_queue_error)
      TA_REQ(chainlock_transaction_token, get_lock(), preempt_disabled_token);

  // Whether the wait queue is currently empty.
  bool IsEmpty() const TA_REQ_SHARED(get_lock()) { return collection_.IsEmpty(); }
  uint32_t Count() const TA_REQ_SHARED(get_lock()) { return collection_.Count(); }

  // Recompute the effective profile of a thread which is known to be blocked in
  // this wait queue, reordering the thread in the queue collection as needed.
  //
  // This method does not deal with the consequences of profile inheritance, and
  // should only ever be called from one of the OwnedWaitQueue's Propagate
  // methods (which will deal with the consequences)
  void UpdateBlockedThreadEffectiveProfile(Thread& t) TA_REQ(get_lock(), t);

  // OwnedWaitQueue needs to be able to call this on WaitQueues to
  // determine if they are base WaitQueues or the OwnedWaitQueue
  // subclass.
  uint32_t magic() const { return magic_; }

 protected:
  explicit constexpr WaitQueue(uint32_t magic) : magic_(magic) {}

  // Inline helpers (defined in wait_queue_internal.h) for
  // WaitQueue::BlockEtc and OwnedWaitQueue::BlockAndAssignOwner to
  // share.
  inline zx_status_t BlockEtcPreamble(Thread* current_thread, const Deadline& deadline,
                                      uint signal_mask, ResourceOwnership reason,
                                      Interruptible interuptible)
      TA_REQ(get_lock(), ChainLockable::GetLock(*current_thread));

  // By the time we have made it to BlockEtcPostamble, we should have dropped
  // the wait_queue lock, and only be holding the lock for current thread (who
  // is about to block).  We have already (successfully) added the thread to the
  // queue and set our blocked state.  All we need to do now is set up our
  // timer, and finally descend into the scheduler in order to block and select
  // a new thread.
  inline zx_status_t BlockEtcPostamble(Thread* current_thread, const Deadline& deadline)
      TA_EXCL(get_lock())
          TA_REQ(chainlock_transaction_token, ChainLockable::GetLock(*current_thread));

  // Dequeue the specified thread and set its blocked_status.  Do not actually
  // schedule the thread to run.
  void DequeueThread(Thread* t, zx_status_t wait_queue_error)
      TA_REQ(get_lock(), ChainLockable::GetLock(*t));

  // Move the specified thread from the source wait queue to the dest wait queue.
  static void MoveThread(WaitQueue* source, WaitQueue* dest, Thread* t)
      TA_REQ(source->get_lock(), dest->get_lock(), ChainLockable::GetLock(*t));

 private:
  // The OwnedWaitQueue subclass also manipulates the collection.
  friend class OwnedWaitQueue;

  // Ideally, the special WaitQueueLock and BrwLock operation(s) could just be
  // member's of the WaitQueue itself, but they need to understand what a
  // Thread::UnblockList is, so they need to be declared after the Thread
  // structure is declared.
  friend struct BrwLockOps;
  friend struct WaitQueueLockOps;

  static void TimeoutHandler(Timer* timer, zx_instant_mono_t now, void* arg);

  // Internal helper for dequeueing a single Thread.
  void Dequeue(Thread* t, zx_status_t wait_queue_error)
      TA_REQ(get_lock(), ChainLockable::GetLock(*t));

  // Validate that the queue of a given WaitQueue is valid.
  void ValidateQueue() TA_REQ_SHARED(get_lock());

  // Note: Wait queues come in 2 flavors (traditional and owned) which are
  // distinguished using the magic number.  The point here is that, unlike
  // most other magic numbers in the system, the wait_queue_t serves a
  // functional purpose beyond checking for corruption debug builds.
  static constexpr uint32_t kMagic = fbl::magic("wait");
  uint32_t magic_;

  WaitQueueCollection collection_ TA_GUARDED(get_lock());
};

// Returns a string constant for the given thread state.
const char* ToString(enum thread_state state);

typedef int (*thread_start_routine)(void* arg);

// Thread trampolines are always called with:
//
// 1) The thread's lock held
// 2) interrupts disabled.
//
// Implementers of trampolines must always remember to unconditionally release
// the current thread's lock, and *then* enable interrupts, before jumping to
// the thread's entry point.
//
// Sadly, there does not seem to be a good way to statically annotate this.
typedef void (*thread_trampoline_routine)() __NO_RETURN;

// clang-format off
#define THREAD_FLAG_DETACHED                 (1 << 0)
#define THREAD_FLAG_FREE_STRUCT              (1 << 1)
#define THREAD_FLAG_IDLE                     (1 << 2)
#define THREAD_FLAG_VCPU                     (1 << 3)
#define THREAD_FLAG_RESTRICTED_KICK_PENDING  (1 << 4)

#define THREAD_SIGNAL_KILL                   (1 << 0)
#define THREAD_SIGNAL_SUSPEND                (1 << 1)
#define THREAD_SIGNAL_POLICY_EXCEPTION       (1 << 2)
#define THREAD_SIGNAL_RESTRICTED_KICK        (1 << 3)
#define THREAD_SIGNAL_SAMPLE_STACK           (1 << 4)
// clang-format on

// thread priority
#define NUM_PRIORITIES (32)
#define LOWEST_PRIORITY (0)
#define HIGHEST_PRIORITY (NUM_PRIORITIES - 1)
#define DPC_PRIORITY (NUM_PRIORITIES - 2)
#define IDLE_PRIORITY LOWEST_PRIORITY
#define LOW_PRIORITY (NUM_PRIORITIES / 4)
#define DEFAULT_PRIORITY (NUM_PRIORITIES / 2)
#define HIGH_PRIORITY ((NUM_PRIORITIES / 4) * 3)

// stack size
#ifdef CUSTOM_DEFAULT_STACK_SIZE
#define DEFAULT_STACK_SIZE CUSTOM_DEFAULT_STACK_SIZE
#else
#define DEFAULT_STACK_SIZE ARCH_DEFAULT_STACK_SIZE
#endif

class PreemptionState {
 public:
  // Counters contained in state_ are limited to 15 bits.
  static constexpr uint32_t kMaxCountValue = 0x7fff;
  // The preempt disable count is in the lowest 15 bits.
  static constexpr uint32_t kPreemptDisableMask = kMaxCountValue;
  // The eager resched disable count is in the next highest 15 bits.
  static constexpr uint32_t kEagerReschedDisableShift = 15;
  static constexpr uint32_t kEagerReschedDisableMask = kMaxCountValue << kEagerReschedDisableShift;
  // Finally, the timeslice extension flags are the highest 2 bits.
  static constexpr uint32_t kTimesliceExtensionFlagsShift = 30;
  static constexpr uint32_t kTimesliceExtensionFlagsMask =
      ~(kPreemptDisableMask | kEagerReschedDisableMask);
  enum TimesliceExtensionFlags {
    // Thread has a timeslice extension that may or may not be Active.
    Present = 0b01 << kTimesliceExtensionFlagsShift,
    // Thread has an Active (in use) timeslice extension.
    Active = 0b10 << kTimesliceExtensionFlagsShift,
  };

  cpu_mask_t preempts_pending() const { return preempts_pending_.load(); }
  void preempts_pending_clear() { preempts_pending_.store(0); }
  void preempts_pending_add(cpu_mask_t mask) { preempts_pending_.fetch_or(mask); }

  bool PreemptIsEnabled() const {
    // Preemption is enabled iff both counts are zero and there's no runtime
    // extension.
    return state_.load() == 0;
  }

  uint32_t PreemptDisableCount() const { return PreemptDisableCount(state_.load()); }
  uint32_t EagerReschedDisableCount() const { return EagerReschedDisableCount(state_.load()); }

  // PreemptDisable() increments the preempt disable counter for the current
  // thread. While preempt disable is non-zero, preemption of the thread is
  // disabled, including preemption from interrupt handlers. During this time,
  // any call to Reschedule() will only record that a reschedule is pending, and
  // won't do a context switch.
  //
  // Note that this does not disallow blocking operations (e.g.
  // mutex.Acquire()). Disabling preemption does not prevent switching away from
  // the current thread if it blocks.
  //
  // A call to PreemptDisable() must be matched by a later call to
  // PreemptReenable() to decrement the preempt disable counter.
  void PreemptDisable() {
    const uint32_t old_state = state_.fetch_add(1);
    ASSERT(PreemptDisableCount(old_state) < kMaxCountValue);
  }

  // PreemptReenable() decrements the preempt disable counter and flushes any
  // pending local preemption operation.  Callers must ensure that they are
  // calling from a context where blocking is allowed, as the call may result in
  // the immediate preemption of the calling thread.
  void PreemptReenable() {
    const uint32_t old_state = state_.fetch_sub(1);
    ASSERT(PreemptDisableCount(old_state) > 0);

    // First, check for the expected situation of dropping the preempt count to zero
    // with a zero eager resched disable count and no timeslice extension.
    if (old_state == 1) {
      FlushPending(Flush::FlushLocal);
      return;
    }

    // Things must be more complicated.  Check for the various situations in
    // decreasing order of likeliness.

    // Are either of the counters non-zero?
    if (EagerReschedDisableCount(old_state) > 0 || PreemptDisableCount(old_state) > 1) {
      // We've got a non-zero count in one of the counters.
      return;
    }

    // The counters are both zero.  At this point, we must have a timeslice
    // extension installed.  This extension may be inactive, active and
    // not-yet-expired, or active and expired.

    // Is there an active extension?
    if (HasActiveTimesliceExtension(old_state)) {
      // Has it expired?
      if (ClearActiveTimesliceExtensionIfExpired()) {
        // It has.  We can flush.
        DEBUG_ASSERT(PreemptIsEnabled());
        FlushPending(Flush::FlushLocal);
        return;
      }
    }

    // We have an extension that's either inactive or active+unexpired.
  }

  void PreemptDisableAnnotated() TA_ACQ(preempt_disabled_token) {
    preempt_disabled_token.Acquire();
    PreemptDisable();
  }

  void PreemptReenableAnnotated() TA_REL(preempt_disabled_token) {
    preempt_disabled_token.Release();
    PreemptReenable();
  }

  static void AssertPreemptionDisabled() TA_ASSERT(preempt_disabled_token) {
    preempt_disabled_token.AssertHeld();
  }

  // PreemptReenableDelayFlush() decrements the preempt disable counter, but
  // deliberately does _not_ flush any pending local preemption operation.
  // Instead, if local preemption has become enabled again after the count
  // drops, and the local pending bit is set, the method will clear the bit and
  // return true.  Otherwise, it will return false.
  //
  // This method may only be called when interrupts are disabled and blocking is
  // not allowed.
  //
  // Callers of this method are "taking" ownership of the responsibility to
  // ensure that preemption on the local CPU takes place in the near future
  // after the call if the method returns true.
  //
  // Use of this method is strongly discouraged outside of top-level interrupt
  // glue and early threading setup.
  //
  // TODO(johngro): Consider replacing the bool return type with a move-only
  // RAII type which wraps the bool, and ensures that preemption event _must_
  // happen, either by having the user call a method on the object to manually
  // force the preemption event, or when the object destructs.
  [[nodiscard]] bool PreemptReenableDelayFlush() {
    DEBUG_ASSERT(arch_ints_disabled());
    DEBUG_ASSERT(arch_blocking_disallowed());

    const uint32_t old_state = state_.fetch_sub(1);
    ASSERT(PreemptDisableCount(old_state) > 0);

    // First, check for the expected situation of dropping the preempt count to zero
    // with a zero eager resched disable count and no timeslice extension.
    if (old_state == 1) {
      const cpu_mask_t local_mask = cpu_num_to_mask(arch_curr_cpu_num());
      const cpu_mask_t prev_mask = preempts_pending_.fetch_and(~local_mask);
      return (local_mask & prev_mask) != 0;
    }

    if (EagerReschedDisableCount(old_state) > 0 || PreemptDisableCount(old_state) > 1) {
      // We've got a non-zero count in one of the counters.
      return false;
    }

    // The counters are both zero.  At this point, we must have a timeslice
    // extension installed.  This extension may be inactive, active and
    // not-yet-expired, or active and expired.

    // Is there an active extension?
    if (HasActiveTimesliceExtension(old_state)) {
      // Has it expired?
      if (ClearActiveTimesliceExtensionIfExpired()) {
        // It has.
        DEBUG_ASSERT(PreemptIsEnabled());
        const cpu_mask_t local_mask = cpu_num_to_mask(arch_curr_cpu_num());
        const cpu_mask_t prev_mask = preempts_pending_.fetch_and(~local_mask);
        return (local_mask & prev_mask) != 0;
      }
    }

    // We have an extension that's either inactive or active+unexpired.
    return false;
  }

  // EagerReschedDisable() increments the eager resched disable counter for the
  // current thread. When early resched disable is non-zero, issuing local and
  // remote preemptions is disabled, including from interrupt handlers. During
  // this time, any call to Reschedule() or other scheduler entry points that
  // imply a reschedule will only record the pending reschedule for the affected
  // CPU, but will not perform reschedule IPIs or a local context switch.
  //
  // As with PreemptDisable, blocking operations are still allowed while
  // eager resched disable is non-zero.
  //
  // A call to EagerReschedDisable() must be matched by a later call to
  // EagerReschedReenable() to decrement the eager resched disable counter.
  void EagerReschedDisable() {
    const uint32_t old_state = state_.fetch_add(1 << kEagerReschedDisableShift);
    ASSERT(EagerReschedDisableCount(old_state) < kMaxCountValue);
  }

  // EagerReschedReenable() decrements the eager resched disable counter and
  // flushes pending local and/or remote preemptions if enabled, respectively.
  void EagerReschedReenable() {
    const uint32_t old_state = state_.fetch_sub(1 << kEagerReschedDisableShift);
    ASSERT(EagerReschedDisableCount(old_state) > 0);

    // First check the expected case.
    if (old_state == 1 << kEagerReschedDisableShift) {
      // Counts are both zero and there's no timeslice extension.
      //
      // Flushing all might reschedule this CPU, make sure it's OK to block.
      FlushPending(Flush::FlushAll);
      return;
    }

    if (EagerReschedDisableCount(old_state) > 1) {
      // Nothing to do since eager resched disable implies preempt disable.
      return;
    }

    // We know we can at least flush remote.  Can we also flush local?
    if (PreemptDisableCount(old_state) > 0) {
      // Nope, we've got a non-zero preempt disable count.
      FlushPending(Flush::FlushRemote);
      return;
    }

    // Is there an active extension?
    if (HasActiveTimesliceExtension(old_state)) {
      // Has it expired?
      if (ClearActiveTimesliceExtensionIfExpired()) {
        // Yes, preempt disable count is zero and the active extension has
        // expired.  We can flush all.
        DEBUG_ASSERT(PreemptIsEnabled());
        FlushPending(Flush::FlushAll);
        return;
      }
      // Extension is active, can't flush local.
    }

    // We have an inactive extension or an unexpired active extension.  Either
    // way, we can flush remote, but not local.
    FlushPending(Flush::FlushRemote);
  }

  void EagerReschedDisableAnnotated() TA_ACQ(preempt_disabled_token) {
    preempt_disabled_token.Acquire();
    EagerReschedDisable();
  }

  void EagerReschedReenableAnnotated() TA_REL(preempt_disabled_token) {
    preempt_disabled_token.Release();
    EagerReschedReenable();
  }

  // Sets a timeslice extension if one is not already set.
  //
  // This method should only be called in normal thread context.
  //
  // Returns false if a timeslice extension was already present or if the
  // supplied duration is <= 0.
  //
  // Note: It OK to call this from a context where preemption is (hard)
  // disabled.  If preemption is requested while the preempt disable count is
  // non-zero and a timeslice extension is in place, the extension will be
  // activated, but preemption will not occur until the count has dropped to
  // zero and the extension has expired or has been clear.
  bool SetTimesliceExtension(zx_duration_mono_t extension_duration) {
    if (extension_duration <= 0) {
      return false;
    }

    uint32_t state = state_.load();
    if (HasTimesliceExtension(state)) {
      return false;
    }
    timeslice_extension_.store(extension_duration);
    // Make sure that the timeslice extension value becomes visible to an
    // interrupt handler in this thread prior to the state_ flag becoming
    // visible.  See comment at |timeslice_extension_|.
    ktl::atomic_signal_fence(ktl::memory_order_release);
    state_.fetch_or(TimesliceExtensionFlags::Present);
    return true;
  }

  // Unconditionally clears any timeslice extension.
  //
  // This method must be called in normal thread context because it may trigger
  // local preemption.
  void ClearTimesliceExtension() {
    // Clear any present timeslice extension.
    const uint32_t old_state = state_.fetch_and(~kTimesliceExtensionFlagsMask);
    // Are the counters both zero?
    if ((old_state & ~kTimesliceExtensionFlagsMask) == 0) {
      FlushPending(Flush::FlushLocal);
    }
  }

  // PreemptSetPending() marks a pending preemption for the given CPUs.
  //
  // This is similar to Reschedule(), except that it may only be used inside an
  // interrupt handler while interrupts and preemption are disabled, between
  // PreemptDisable() and PreemptReenable(). It is similar to Reschedule(),
  // except that it does not need to be called with thread's lock held.
  void PreemptSetPending(cpu_mask_t reschedule_mask = cpu_num_to_mask(arch_curr_cpu_num())) {
    DEBUG_ASSERT(arch_ints_disabled());
    DEBUG_ASSERT(arch_blocking_disallowed());
    DEBUG_ASSERT(!PreemptIsEnabled());

    preempts_pending_.fetch_or(reschedule_mask);

    // Are we pending for the local CPU?
    if (reschedule_mask & cpu_num_to_mask(arch_curr_cpu_num() == 0)) {
      // Nope.
      return;
    }

    EvaluateTimesliceExtension();
  }

  // Evaluate the thread's timeslice extension (if present), activating or
  // expiring it as necessary.
  //
  // Returns whether preemption is enabled.
  bool EvaluateTimesliceExtension() {
    const uint32_t old_state = state_.load();
    if (old_state == 0) {
      // No counts, no extension.  The common case.
      return true;
    }

    if (!HasTimesliceExtension(old_state)) {
      // No extension, but we have a non-zero count.
      return false;
    }

    if (HasActiveTimesliceExtension(old_state)) {
      if (!ClearActiveTimesliceExtensionIfExpired()) {
        return false;
      }
      // The active extension has expired.  If the counts are both zero, then
      // we're ready for preemption.
      return (old_state & ~kTimesliceExtensionFlagsMask) == 0;
    }

    // We have a not-yet-active extension.  Time to activate it.
    //
    // See comment at |timeslice_extension_| for why the signal fence is needed.
    ktl::atomic_signal_fence(ktl::memory_order_acquire);
    const zx_duration_mono_t extension_duration = timeslice_extension_.load();
    if (extension_duration <= 0) {
      // Already expired.
      state_.fetch_and(~kTimesliceExtensionFlagsMask);
      return (old_state & ~kTimesliceExtensionFlagsMask) == 0;
    }
    const zx_instant_mono_t deadline =
        zx_time_add_duration(current_mono_time(), extension_duration);
    timeslice_extension_deadline_.store(deadline);
    // See comment at |timeslice_extension_deadline_| for why the signal fence
    // is needed.
    ktl::atomic_signal_fence(ktl::memory_order_release);
    state_.fetch_or(TimesliceExtensionFlags::Active);
    SetPreemptionTimerForExtension(deadline);
    return false;
  }

  // Resets the preemption state. This should only be called when reviving an
  // idle/power thread that halted while taking a CPU offline.
  void Reset() {
    state_ = 0;
    preempts_pending_ = 0;
    timeslice_extension_ = 0;
    timeslice_extension_deadline_ = 0;
  }

 private:
  friend class PreemptDisableTestAccess;
  friend concurrent::CapabilityToken<PreemptionState>;

  // Implements the runtime assertion for concurrent::CapabilityToken<PreemptionState>::AssertHeld.
  static void AssertCapabilityHeld(concurrent::CapabilityToken<PreemptionState>::TagType);

  static inline uint32_t EagerReschedDisableCount(uint32_t state) {
    return (state & kEagerReschedDisableMask) >> kEagerReschedDisableShift;
  }

  static inline uint32_t PreemptDisableCount(uint32_t state) { return state & kPreemptDisableMask; }

  static inline bool HasTimesliceExtension(uint32_t state) {
    return (state & TimesliceExtensionFlags::Present) != 0;
  }

  static inline bool HasActiveTimesliceExtension(uint32_t state) {
    return (state & TimesliceExtensionFlags::Active) != 0;
  }

  // A non-inlined helper method to set the preemption timer when a timeslice
  // has been extended.  This must be non-inline to avoid an #include cycle with
  // percpu.h and thread.h.
  static void SetPreemptionTimerForExtension(zx_instant_mono_t deadline);

  // Checks whether the active timeslice extension has expired and if so, clears
  // it and returns true.
  //
  // Should only be called when there is an active timeslice extension.
  bool ClearActiveTimesliceExtensionIfExpired() {
    // Has the extension expired?
    //
    // See comment at |timeslice_extension_deadline_| for why the signal fence is needed.
    ktl::atomic_signal_fence(ktl::memory_order_acquire);
    if (current_mono_time() >= timeslice_extension_deadline_.load()) {
      state_.fetch_and(~kTimesliceExtensionFlagsMask);
      return true;
    }
    return false;
  }

  enum Flush { FlushLocal = 0x1, FlushRemote = 0x2, FlushAll = FlushLocal | FlushRemote };

  // Flushes local, remote, or all pending preemptions.
  //
  // This method is split in two so that the early out case of no pending
  // preemptions may be inlined without creating a header include cycle.
  void FlushPending(Flush flush) {
    // Early out to avoid unnecessarily taking the thread lock. This check races
    // any potential flush due to context switch, however, the context switch can
    // only clear bits that would have been flushed below, no new pending
    // preemptions are possible in the mask bits indicated by |flush|.
    if (likely(preempts_pending_.load() == 0)) {
      return;
    }
    FlushPendingContinued(flush);
  }
  void FlushPendingContinued(Flush flush);

  // state_ contains three fields:
  //
  //  * a 15-bit preempt disable counter (bits 0-14)
  //  * a 15-bit eager resched disable counter (bits 15-29)
  //  * a 2-bit for TimesliceExtensionFlags (bits 30-31)
  //
  // This is a single field so that both counters and the flags can be compared
  // against zero with a single memory access and comparison.
  //
  // state_'s counts are modified by interrupt handlers, but the counts are
  // always restored to their original value before the interrupt handler
  // returns, so modifications are not visible to the interrupted thread.
  RelaxedAtomic<uint32_t> state_{};

  // preempts_pending_ tracks pending reschedules to both local and remote CPUs
  // due to activity in the context of the current thread.
  //
  // This value can be changed asynchronously by an interrupt handler.
  //
  // preempts_pending_ should only be non-zero:
  //  * if PreemptDisableCount() or EagerReschedDisable() are non-zero, or
  //  * after PreemptDisableCount() or EagerReschedDisable() have been
  //    decremented, while preempts_pending_ is being checked.
  RelaxedAtomic<cpu_mask_t> preempts_pending_{};

  // The maximum duration of the thread's timeslice extension.
  //
  // This field is only valid when |state_|'s
  // |kTimeSliceExtensionFlags::Present| flag it set.
  //
  // This field may only be accessed by its owning thread or in an interrupt
  // context of the owning thread.  When reading this field, be sure to issue an
  // atomic_signal_fence (compiler barrier) with acquire semantics after
  // observing the Present flag.  Likewise, when writing this field, use an
  // atomic_signal_fence with release semantics prior to setting the Present
  // flag.  By using these fences, we ensure the flag and field value remain in
  // sync.
  RelaxedAtomic<zx_duration_mono_t> timeslice_extension_{};

  // The deadline at which the thread timeslice extension expires.
  //
  // This field is only valid when |kTimeSliceExtensionFlags::Active| flag it
  // set.
  //
  // This field may only be accessed by its owning thread or in an interrupt
  // context of the owning thread.  When reading this field, be sure to issue an
  // atomic_signal_fence (compiler barrier) with acquire semantics after
  // observing the Active flag.  Likewise, when writing this field, use an
  // atomic_signal_fence with release semantics prior to setting the Active
  // flag.  By using these fences, we ensure the flag and field value remain in
  // sync.
  RelaxedAtomic<zx_instant_mono_t> timeslice_extension_deadline_{};
};

// TaskState is responsible for running the task defined by
// |entry(arg)|, and reporting its value to any joining threads.
//
// TODO: the detached state in Thread::flags_ probably belongs here.
class TaskState {
 public:
  TaskState() = default;

  void Init(thread_start_routine entry, void* arg);

  zx_status_t Join(Thread* current_thread, zx_instant_mono_t deadline) TA_REL(get_lock())
      TA_REQ(chainlock_transaction_token, ChainLockable::GetLock(*current_thread));

  // Attempt to wake all of our joiners.  This operation might fail because of a
  // conflict while attempting to obtain the required locks, in which case the
  // caller needs to back off (drop all currently held chain locks) and try
  // again..
  bool TryWakeJoiners(zx_status_t status)
      TA_REQ(chainlock_transaction_token, preempt_disabled_token);

  thread_start_routine entry() const { return entry_; }
  void* arg() const { return arg_; }

  int retcode() const { return retcode_; }
  void set_retcode(int retcode) { retcode_ = retcode; }

  ChainLock& get_lock() TA_RET_CAP(retcode_wait_queue_.get_lock()) {
    return retcode_wait_queue_.get_lock();
  }

 private:
  // Dumping routines are allowed to see inside us.
  friend class ThreadDumper;

  // The Thread's entry point, and its argument.
  thread_start_routine entry_ = nullptr;
  void* arg_ = nullptr;

  // Storage for the return code.
  //
  // TODO(johngro): This is logically protected by our thread's lock.  What is a
  // good way to represent this with a static annotation?
  int retcode_ = 0;

  // Other threads waiting to join this Thread.
  WaitQueue retcode_wait_queue_;
};

// Keeps track of whether a thread is allowed to allocate memory.
//
// A thread's |MemoryAllocationState| should only be accessed by that thread itself or interrupt
// handlers running in the thread's context.
class MemoryAllocationState {
 public:
  void Disable() {
    ktl::atomic_signal_fence(ktl::memory_order_seq_cst);
    disable_count_ = disable_count_ + 1;
    ktl::atomic_signal_fence(ktl::memory_order_seq_cst);
  }

  void Enable() {
    ktl::atomic_signal_fence(ktl::memory_order_seq_cst);
    DEBUG_ASSERT(disable_count_ > 0);
    disable_count_ = disable_count_ - 1;
    ktl::atomic_signal_fence(ktl::memory_order_seq_cst);
  }

  // Returns true if memory allocation is allowed.
  bool IsEnabled() {
    ktl::atomic_signal_fence(ktl::memory_order_seq_cst);
    return disable_count_ == 0;
  }

 private:
  // Notice that we aren't using atomic operations to access the field.  We don't need atomic
  // operations here as long as...
  //
  // 1. We use atomic_signal_fence to prevent compiler reordering.
  //
  // 2. We use volatile to ensure the compiler actually generates loads and stores for the value (so
  // the interrupt handler can see what the thread see, and vice versa).
  //
  // 3. Upon completion, an interrupt handler that modified the field restores it to the value it
  // held at the start of the interrupt.
  volatile uint32_t disable_count_ = 0;
};

// Stall accounting state of a thread.
//
// Kernel-only threads are never considered for the purpose of memory stall
// accounting (`IgnoredKernelOnly`).
//
// If a user thread is not signalling a stall, it will contribute to the current
// CPU's memory stall accumulator as `Progressing` while running and do not
// contribute (`Inactive`) in any other state.
//
// If a user thread signals that a stall is ongoing, it will contribute to the
// current CPU's accumulator as `Stalling`. If preempted, the contribution will
// persist on the last CPU's accumulator. Normal operation is resumed when the
// thread signals that the stall is over.
enum class ThreadStallState {
  IgnoredKernelOnly,
  Inactive,
  Progressing,
  Stalling,
};

struct Thread : public ChainLockable {
  // TODO(kulakowski) Are these needed?
  // Default constructor/destructor declared to be not-inline in order to
  // avoid circular include dependencies involving Thread, WaitQueue, and
  // OwnedWaitQueue.
  Thread();
  ~Thread();

  static Thread* CreateIdleThread(cpu_num_t cpu_num);

  // Revive the idle/power thread for the given CPU so that it starts fresh after
  // halting while taking the CPU offline. Most state is preserved, including
  // stack memory locations (useful for debugging) and accumulated runtime stats.
  static void ReviveIdlePowerThread(cpu_num_t cpu_num) TA_EXCL(chainlock_transaction_token);

  // Creates a thread with |name| that will execute |entry| at |priority|. |arg|
  // will be passed to |entry| when executed, the return value of |entry| will be
  // passed to Exit().
  // This call allocates a thread and places it in the global thread list. This
  // memory will be freed by either Join() or Detach(), one of these
  // MUST be called.
  // The thread will not be scheduled until Resume() is called.
  static Thread* Create(const char* name, thread_start_routine entry, void* arg, int priority);
  static Thread* Create(const char* name, thread_start_routine entry, void* arg,
                        const SchedulerState::BaseProfile& profile);
  static Thread* CreateEtc(Thread* t, const char* name, thread_start_routine entry, void* arg,
                           const SchedulerState::BaseProfile& profile,
                           thread_trampoline_routine alt_trampoline);

  // Public routines used by debugging code to dump thread state.
  void Dump(bool full) const TA_EXCL(get_lock());
  static void DumpAll(bool full) TA_EXCL(list_lock_);
  static void DumpTid(zx_koid_t tid, bool full) TA_EXCL(list_lock_);

  // Same stuff, but skip the locks when we are panic'ing
  inline void DumpDuringPanic(bool full) const TA_NO_THREAD_SAFETY_ANALYSIS;
  static inline void DumpAllDuringPanic(bool full) TA_NO_THREAD_SAFETY_ANALYSIS;
  static inline void DumpTidDuringPanic(zx_koid_t tid, bool full) TA_NO_THREAD_SAFETY_ANALYSIS;

  // Internal initialization routines. Eventually, these should be private.
  void SecondaryCpuInitEarly();

  // Associate this Thread to the given ThreadDispatcher.
  void SetUsermodeThread(fbl::RefPtr<ThreadDispatcher> user_thread) TA_EXCL(get_lock());

  void AssertIsCurrentThread() const TA_ASSERT(is_current_thread_token) {
    DEBUG_ASSERT(this == Thread::Current::Get());
  }

  // Returns the lock that protects the thread's internal state, particularly with respect to
  // scheduling.
  fbl::NullLock& get_scheduler_variable_lock() const TA_RET_CAP(scheduler_variable_lock_) {
    return scheduler_variable_lock_;
  }
  static auto& get_list_lock() TA_RET_CAP(list_lock_) { return list_lock_; }

  // Get the associated ThreadDispatcher.
  ThreadDispatcher* user_thread() { return user_thread_.get(); }
  const ThreadDispatcher* user_thread() const { return user_thread_.get(); }

  // Returns the koid of the associated ProcessDispatcher for user threads or
  // ZX_KOID_INVLID for kernel threads.
  zx_koid_t pid() const { return pid_; }

  // Returns the koid of the associated ThreadDispatcher for user threads or an
  // independent koid for kernel threads.
  zx_koid_t tid() const { return tid_; }

  // Return the pid/tid of the thread as a tracing thread reference.
  fxt::ThreadRef<fxt::RefType::kInline> fxt_ref() const { return {pid(), tid()}; }

  // Implicit conversion to a tracing thread reference.
  operator fxt::ThreadRef<fxt::RefType::kInline>() const { return fxt_ref(); }

  // Called to mark a thread as schedulable.
  void Resume() TA_EXCL(chainlock_transaction_token);
  zx_status_t Suspend() { return SuspendOrKillInternal(SuspendOrKillOp::Suspend); }
  void Forget();
  zx_status_t RestrictedKick();
  // Marks a thread as detached, in this state its memory will be released once
  // execution is done.
  zx_status_t Detach();
  zx_status_t DetachAndResume();
  // Waits |deadline| time for a thread to complete execution then releases its memory.
  zx_status_t Join(int* retcode, zx_instant_mono_t deadline);
  // Deliver a kill signal to a thread.
  void Kill() { SuspendOrKillInternal(SuspendOrKillOp::Kill); }

  // Checks whether the kill or suspend signal has been raised. If kill has been
  // raised, then `ZX_ERR_INTERNAL_INTR_KILLED` will be returned. If suspend has
  // been raised, then `ZX_ERR_INTERNAL_INTR_RETRY` will be returned. Otherwise,
  // `ZX_OK` will be returned.
  zx_status_t CheckKillOrSuspendSignal() const;

  // Erase this thread from all global lists, where applicable.
  void EraseFromListsLocked() TA_REQ(list_lock_);

  /**
   * @brief Set the base profile for this thread.
   *
   * The chosen scheduling discipline and its associate parameters are defined by the BaseProfile
   * object.
   *
   * @param params The weight to apply to the thread.
   */
  void SetBaseProfile(const SchedulerState::BaseProfile& profile) TA_EXCL(get_lock());

  void* recursive_object_deletion_list() { return recursive_object_deletion_list_; }
  void set_recursive_object_deletion_list(void* ptr) { recursive_object_deletion_list_ = ptr; }

  // Get/set the mask of valid CPUs that thread may run on. If a new mask
  // is set, the thread will be migrated to satisfy the new constraint.
  //
  // Affinity comes in two flavours:
  //
  //   * "hard affinity", which will always be respected by the scheduler.
  //     The scheduler will panic if it can't satisfy this affinity.
  //
  //   * "soft affinity" indicating where the thread should ideally be scheduled.
  //     The scheduler will respect the mask unless there are no other
  //     options (e.g., the soft affinity and hard affinity don't contain
  //     any common CPUs).
  //
  // If the two masks conflict, the hard affinity wins.
  //
  // Setting the affinity mask returns the previous value to make pinning and
  // restoring operations simpler and more efficient.
  //
  // See scheduler.h for the declaration of the affinity setters.

  cpu_mask_t SetCpuAffinity(cpu_mask_t affinity) TA_EXCL(get_lock());
  cpu_mask_t GetCpuAffinity() const TA_EXCL(get_lock());
  cpu_mask_t SetSoftCpuAffinity(cpu_mask_t affinity) TA_EXCL(get_lock());
  cpu_mask_t GetSoftCpuAffinity() const TA_EXCL(get_lock());

  enum class MigrateStage {
    // The stage before the thread has migrated. Called from the old CPU to save state.
    Save,
    // The stage after the thread has migrated. Called from the new CPU to restore state.
    Restore,
    // The Thread is exiting. Can be called from any CPU.
    Exiting,
  };
  // The migrate function will be invoked twice when a thread is migrate between
  // CPUs. Firstly when the thread is removed from the old CPUs scheduler,
  // secondly when the thread is rescheduled on the new CPU. When the migrate
  // function is called, |thread_lock| is held.
  //
  // TODO(johngro): what to do about this?  It is all fine and good to say that
  // a MigrateFn needs to have the thread's lock held when it is called, but
  // there does not seem to be any good way to annotate this as a property of
  // the fit::inline function's callable type. Perhaps the best thing to do is
  // annotate the migrate function member itself with being guarded by the
  // thread's lock, so that invocation (at least) would require holding the
  // lock?
  //
  // Implementers of migrate functions should be able to annotate their
  // callbacks as requiring the thread's lock in order to be invoked, but
  // unfortunately this does not work along side of the fit::inline_function's
  // type.  If a user attempts to annotate their lambda as such, the
  // fit::inline_function fails expansion when its `operator()` attempt to call
  // the lambda which requires that the lock be held.  If there was some way
  // using the static lock annotation system to have the operator() inherit
  // these annotations from its templated target type, we could avoid the issue,
  // but that does not seem to be an option.

  using MigrateFn = fit::inline_function<void(Thread* thread, MigrateStage stage), sizeof(void*)>;
  void SetMigrateFn(MigrateFn migrate_fn) TA_EXCL(list_lock_, get_lock());
  void SetMigrateFnLocked(MigrateFn migrate_fn) TA_REQ(list_lock_, get_lock());
  void CallMigrateFnLocked(MigrateStage stage) TA_REQ(get_lock());

  // Call |migrate_fn| for each thread that was last run on the current CPU.
  // Note: no locks should be held during this operation, and interrupts need to
  // be disabled.
  static void CallMigrateFnForCpu(cpu_num_t cpu);

  // The context switch function will be invoked when a thread is context
  // switched to or away from. This will be called when a thread is about to be
  // run on a CPU, after it's stopped from running on a CPU, or about to exit.
  using ContextSwitchFn = fit::inline_function<void(), sizeof(void*)>;
  void SetContextSwitchFn(ContextSwitchFn context_switch_fn) TA_EXCL(get_lock());
  void SetContextSwitchFnLocked(ContextSwitchFn context_switch_fn) TA_REQ(get_lock());
  // Call |context_switch_fn| for this thread.
  void CallContextSwitchFnLocked() TA_REQ(get_lock()) {
    if (unlikely(context_switch_fn_)) {
      context_switch_fn_();
    }
  }

  void OwnerName(char (&out_name)[ZX_MAX_NAME_LEN]) const;
  // Return the number of nanoseconds a thread has been running for.
  zx_duration_mono_t Runtime() const TA_EXCL(get_lock());

  // Last cpu this thread was running on, or INVALID_CPU if it has never run.
  cpu_num_t LastCpu() const TA_EXCL(get_lock());
  cpu_num_t LastCpuLocked() const TA_REQ_SHARED(get_lock());

  // Return true if thread has been signaled.
  bool IsSignaled() { return signals() != 0; }
  bool IsIdle() const { return !!(flags_ & THREAD_FLAG_IDLE); }

  // Returns true if this Thread's user state has been saved.
  //
  // Caller must hold the thread's lock.
  bool IsUserStateSavedLocked() const TA_REQ(get_lock()) { return user_state_saved_; }

  // Callback for the Timer used for SleepEtc.
  static void SleepHandler(Timer* timer, zx_instant_mono_t now, void* arg);
  void HandleSleep(Timer* timer, zx_instant_mono_t now);

  // Request a thread to check if it should sample its backtrace. When the thread returns to
  // usermode, it will take a sample of its userstack if sampling is enabled.
  static void SignalSampleStack(Timer* t, zx_instant_mono_t, void* per_cpu_state);

  // All of these operations implicitly operate on the current thread.
  struct Current {
    // This is defined below, just after the Thread declaration.
    static inline Thread* Get();

    // Scheduler routines to be used by regular kernel code.
    static void Yield();
    static void Preempt();
    static void Reschedule() TA_EXCL(chainlock_transaction_token);
    static void Exit(int retcode) __NO_RETURN;
    static void Kill();
    static void BecomeIdle() __NO_RETURN;

    // Wait until the deadline has occurred.
    //
    // If interruptible, may return early with ZX_ERR_INTERNAL_INTR_KILLED if
    // thread is signaled for kill.
    static zx_status_t SleepEtc(const Deadline& deadline, Interruptible interruptible,
                                zx_instant_mono_t now) TA_EXCL(chainlock_transaction_token);
    // Non-interruptible version of SleepEtc.
    static zx_status_t Sleep(zx_instant_mono_t deadline);
    // Non-interruptible relative delay version of Sleep.
    static zx_status_t SleepRelative(zx_duration_mono_t delay);
    // Interruptible version of Sleep.
    static zx_status_t SleepInterruptible(zx_instant_mono_t deadline);

    // Transition the current thread to the THREAD_SUSPENDED state.
    static void DoSuspend();

    // Write the current thread's stack to the assigned sampler buffers
    static void DoSampleStack(GeneralRegsSource source, void* gregs);

    // |policy_exception_code| should be a ZX_EXCP_POLICY_CODE_* value.
    static void SignalPolicyException(uint32_t policy_exception_code,
                                      uint32_t policy_exception_data);

    // Process any pending thread signals.
    //
    // This method may never return if the thread has a pending kill signal.
    //
    // Interrupt state - This method modifies interrupt state.  It is critical that this method be
    // called with interrupts disabled to eliminate a "lost wakeup" race condition.  While
    // interrupts must be disabled prior to call this method, the method may re-enable them during
    // the processing of certain signals.  This method guarantees that if it does return, it will do
    // so with interrupts disabled.
    static void ProcessPendingSignals(GeneralRegsSource source, void* gregs);

    // Migrates the current thread to the CPU identified by target_cpu.
    static void MigrateToCpu(cpu_num_t target_cpuid);

    static void SetName(const char* name);

    static PreemptionState& preemption_state() {
      return Thread::Current::Get()->preemption_state();
    }

    static MemoryAllocationState& memory_allocation_state() {
      return Thread::Current::Get()->memory_allocation_state_;
    }

    // If a restricted kick is pending on this thread, clear it and return true.
    // Otherwise return false.
    // Must be called with interrupts disabled.
    [[nodiscard]] static bool CheckForRestrictedKick();

    static RestrictedState* restricted_state() {
      return Thread::Current::Get()->restricted_state();
    }

    static VmAspace* active_aspace() { return Thread::Current::Get()->aspace_; }

    static VmAspace* switch_aspace(VmAspace* aspace) TA_NO_THREAD_SAFETY_ANALYSIS {
      return Thread::Current::Get()->switch_aspace(aspace);
    }

    // These three functions handle faults on the address space containing va. If there is no
    // aspace that contains va, or we don't have access to it, a ZX_ERR_NOT_FOUND is returned.
    //
    // Calling any of these methods on a pure kernel thread (i.e. one without an associated
    // `ThreadDispatcher`) is a programming error.
    static zx_status_t PageFault(vaddr_t va, uint flags) {
      return Fault(FaultType::PageFault, va, flags);
    }
    static zx_status_t SoftFault(vaddr_t va, uint flags) {
      return Fault(FaultType::SoftFault, va, flags);
    }
    static zx_status_t AccessedFault(vaddr_t va) { return Fault(FaultType::AccessedFault, va, 0); }

    // Generate a backtrace for the calling thread.
    //
    // |out_bt| will be reset() prior to be filled in and if a backtrace cannot
    // be obtained, it will be left empty.
    static void GetBacktrace(Backtrace& out_bt);

    // Generate a backtrace for the calling thread starting at frame pointer |fp|.
    //
    // |out_bt| will be reset() prior to be filled in and if a backtrace cannot
    // be obtained, it will be left empty.
    static void GetBacktrace(vaddr_t fp, Backtrace& out_bt);

    static void Dump(bool full) { Thread::Current::Get()->Dump(full); }
    static void DumpDuringPanic(bool full) TA_NO_THREAD_SAFETY_ANALYSIS {
      Thread::Current::Get()->DumpDuringPanic(full);
    }

   private:
    // Handles a virtual memory fault in the address space that contains va. If there is no aspace
    // that contains va, or we don't have access to it, a ZX_ERR_NOT_FOUND is returned.
    enum class FaultType : uint8_t { PageFault, SoftFault, AccessedFault };
    static zx_status_t Fault(FaultType type, vaddr_t va, uint flags);
  };  // struct Current;

  // Trait for the global Thread list.
  struct ThreadListTrait {
    static fbl::DoublyLinkedListNodeState<Thread*>& node_state(Thread& thread) {
      return thread.thread_list_node_;
    }
  };
  using List = fbl::DoublyLinkedListCustomTraits<Thread*, ThreadListTrait>;

  // Traits for the temporary unblock list, used to batch-unblock threads.
  struct UnblockListTrait {
    static fbl::DoublyLinkedListNodeState<Thread*>& node_state(Thread& thread) {
      return thread.unblock_list_node_;
    }
  };
  using UnblockList = fbl::DoublyLinkedListCustomTraits<Thread*, UnblockListTrait>;

  // Traits for the save state list, used to run the Save stage of migration functions.
  struct SaveStateListTrait {
    static fbl::SinglyLinkedListNodeState<Thread*>& node_state(Thread& thread) {
      return thread.save_state_list_node_;
    }
  };
  using SaveStateList = fbl::SinglyLinkedListCustomTraits<Thread*, SaveStateListTrait>;

  struct Linebuffer {
    size_t pos = 0;
    ktl::array<char, 128> buffer{};
  };

  // Called by the scheduler to update runtime stats.
  void UpdateRuntimeStats(thread_state new_state);

  // Accessors into Thread state. When the conversion to all-private
  // members is complete (bug 54383), we can revisit the overall
  // Thread API.

  thread_state state() const TA_REQ_SHARED(get_lock()) { return scheduler_state_.state(); }

  // The scheduler can set threads to be running, or to be ready to run.
  void set_running() TA_REQ(get_lock()) { scheduler_state_.set_state(THREAD_RUNNING); }
  void set_ready() TA_REQ(get_lock()) { scheduler_state_.set_state(THREAD_READY); }
  // While wait queues can set threads to be blocked.
  void set_blocked() TA_REQ(get_lock()) { scheduler_state_.set_state(THREAD_BLOCKED); }
  void set_blocked_read_lock() TA_REQ(get_lock()) {
    scheduler_state_.set_state(THREAD_BLOCKED_READ_LOCK);
  }
  // The thread can set itself to be sleeping.
  void set_sleeping() TA_REQ(get_lock()) { scheduler_state_.set_state(THREAD_SLEEPING); }
  void set_death() TA_REQ(get_lock()) { scheduler_state_.set_state(THREAD_DEATH); }
  void set_suspended() TA_REQ(get_lock()) { scheduler_state_.set_state(THREAD_SUSPENDED); }

  // Accessors for specific flags_ bits.
  bool detatched() const { return (flags_ & THREAD_FLAG_DETACHED) != 0; }
  void set_detached(bool value) {
    if (value) {
      flags_ |= THREAD_FLAG_DETACHED;
    } else {
      flags_ &= ~THREAD_FLAG_DETACHED;
    }
  }
  bool free_struct() const { return (flags_ & THREAD_FLAG_FREE_STRUCT) != 0; }
  void set_free_struct(bool value) {
    if (value) {
      flags_ |= THREAD_FLAG_FREE_STRUCT;
    } else {
      flags_ &= ~THREAD_FLAG_FREE_STRUCT;
    }
  }
  bool idle() const { return (flags_ & THREAD_FLAG_IDLE) != 0; }
  void set_idle(bool value) {
    if (value) {
      flags_ |= THREAD_FLAG_IDLE;
    } else {
      flags_ &= ~THREAD_FLAG_IDLE;
    }
  }
  bool vcpu() const { return (flags_ & THREAD_FLAG_VCPU) != 0; }
  void set_vcpu(bool value) {
    if (value) {
      flags_ |= THREAD_FLAG_VCPU;
    } else {
      flags_ &= ~THREAD_FLAG_VCPU;
    }
  }

  // Access to the entire flags_ value, for diagnostics.
  unsigned int flags() const { return flags_; }

  unsigned int signals() const { return signals_.load(ktl::memory_order_relaxed); }

  bool has_migrate_fn() const TA_REQ_SHARED(get_lock()) { return migrate_fn_ != nullptr; }
  bool migrate_pending() const TA_REQ_SHARED(get_lock()) { return migrate_pending_; }

  TaskState& task_state() { return task_state_; }
  const TaskState& task_state() const { return task_state_; }

  PreemptionState& preemption_state() { return preemption_state_; }
  const PreemptionState& preemption_state() const { return preemption_state_; }

  SchedulerState& scheduler_state() TA_REQ(get_lock()) { return scheduler_state_; }
  const SchedulerState& scheduler_state() const TA_REQ_SHARED(get_lock()) {
    return scheduler_state_;
  }

  SchedulerQueueState& scheduler_queue_state() TA_REQ(scheduler_variable_lock_) {
    return scheduler_queue_state_;
  }
  const SchedulerQueueState& scheduler_queue_state() const TA_REQ(scheduler_variable_lock_) {
    return scheduler_queue_state_;
  }

  WaitQueueCollection::ThreadState& wait_queue_state() TA_REQ(get_lock()) {
    return wait_queue_state_;
  }
  const WaitQueueCollection::ThreadState& wait_queue_state() const TA_REQ_SHARED(get_lock()) {
    return wait_queue_state_;
  }

#if WITH_LOCK_DEP
  lockdep::ThreadLockState& lock_state() { return lock_state_; }
  const lockdep::ThreadLockState& lock_state() const { return lock_state_; }
#endif

  RestrictedState* restricted_state() { return restricted_state_.get(); }
  void set_restricted_state(ktl::unique_ptr<RestrictedState> restricted_state) {
    restricted_state_ = ktl::move(restricted_state);
  }

  arch_thread& arch() { return arch_; }
  const arch_thread& arch() const { return arch_; }

  KernelStack& stack() { return stack_; }
  const KernelStack& stack() const { return stack_; }

  // Rules for the aspace_ member.
  //
  // 1) Raw VmAspace pointers held by Thread object are guaranteed to always be
  //    "alive" from a C++ perspective (the object has not been destructed, and
  //    the memory is still alive).  The actual VmAspace object is a ref-counted
  //    object which is guaranteed to be alive because:
  // 1a) It is a member of a thread's process (the main aspace, as well as the
  //     restricted mode aspace), and a thread's process object cannot be
  //     destroyed while the thread is still alive.  Or --
  // 1b) It is a special statically allocated aspace object, such as the EFI
  //     address space, or the kernel address space.
  // 2) Only the current thread is permitted to mutate its own aspace_ member.
  //    The only exception to this is during initialization, before the thread
  //    is running, by VmAspace::AttachToThread.
  // 3) The currently running thread is allowed to access its own currently
  //    assigned aspace without needing any locks.
  // 4) Any other thread who wishes to access a thread's aspace must do so via
  //    either GetAspaceRef or GetAspaceRefLocked, which will return a RefPtr to
  //    the thread's current aspace as long as the thread has not entered the
  //    THREAD_DEATH state.
  // 5) Other threads who access a thread's aspace via GetAspaceRef cannot
  //    assume that either the thread or the aspace is still alive after the
  //    thread's lock is dropped.  At best, they know that the RefPtr is keeping
  //    the VmAspace object alive from a C++ perspective, nothing more.
  // 6) The scheduler and low level context switch routines are allowed to
  //    directly access the aspace_ of both the old and new threads involved in the
  //    context switch.  Neither one is currently really running (the aspace_
  //    members cannot change), and both threads have to be alive enough that
  //    their process is still alive, and therefore so is their aspace_ (see
  //    #1).  Note; it is possible that the old thread involved in a context
  //    switch is in the process of dying, but we know that its process cannot
  //    exit (destroying its aspace in the process) until after it has context
  //    switched for the last time.
  fbl::RefPtr<VmAspace> GetAspaceRef() const TA_EXCL(get_lock());
  fbl::RefPtr<VmAspace> GetAspaceRefLocked() const TA_REQ_SHARED(get_lock());
  VmAspace* aspace() TA_REQ(is_current_thread_token) { return aspace_; }
  const VmAspace* aspace() const TA_REQ(is_current_thread_token) { return aspace_; }
  VmAspace* switch_aspace(VmAspace* aspace) TA_REQ(is_current_thread_token) {
    VmAspace* old_aspace = aspace_;
    aspace_ = aspace;
    return old_aspace;
  }

  // Returns the currently active address space, which is the address space currently hosting
  // page tables for the thread.
  //
  // The active address space should be used only when context switching. It should not be used for
  // resolving faults, as it may be a unified aspace that does not keep track of its own mappings.
  //
  // Kernel-only thread -- This will return nullptr, unless a caller has explicitly set aspace_
  // using `switch_aspace`, which is done by a few kernel unittests.
  //
  // User thread -- If the thread is in Restricted Mode, this will return the restricted aspace.
  // Otherwise, it will return the process's normal aspace.
  //
  // Note, the normal aspace is, by definition, the aspace that's active when a thread is in Normal
  // Mode.  All threads not in Restricted Mode are said to be in Normal Mode.  See
  // `ProcessDispatcher::normal_aspace()` for more information.
  VmAspace* active_aspace() { return aspace_; }
  const VmAspace* active_aspace() const { return aspace_; }

  const char* name() const { return name_; }
  // This may truncate |name|, so that it (including a trailing NUL
  // byte) fit in ZX_MAX_NAME_LEN bytes.
  void set_name(ktl::string_view name);

  Linebuffer& linebuffer() { return linebuffer_; }

  using Canary = fbl::Canary<fbl::magic("thrd")>;
  const Canary& canary() const { return canary_; }

  // Generate a backtrace for |this| thread.
  //
  // |this| must be blocked, sleeping or suspended (i.e. not running).
  //
  // |out_bt| will be reset() prior to be filled in and if a backtrace cannot be
  // obtained, it will be left empty.
  void GetBacktrace(Backtrace& out_bt) TA_EXCL(get_lock());

  StackOwnedLoanedPagesInterval* stack_owned_loaned_pages_interval() {
    return stack_owned_loaned_pages_interval_;
  }

  // Returns the last flow id allocated by TakeNextLockFlowId() for this thread.
  uint64_t lock_flow_id() const {
#if LOCK_TRACING_ENABLED
    return lock_flow_id_;
#else
    return 0;
#endif
  }

  // Returns a unique flow id for lock contention tracing. The same value is
  // returned by lock_flow_id() until another id is allocated for this thread
  // by calling this method again.
  uint64_t TakeNextLockFlowId() {
#if LOCK_TRACING_ENABLED
    return lock_flow_id_ = lock_flow_id_generator_ += 1;
#else
    return 0;
#endif
  }

  void RecomputeEffectiveProfile() TA_REQ(get_lock()) {
    scheduler_state_.RecomputeEffectiveProfile();
  }

  SchedulerState::EffectiveProfile SnapshotEffectiveProfileLocked() const
      TA_REQ_SHARED(get_lock()) {
    return scheduler_state_.effective_profile_;
  }

  SchedulerState::EffectiveProfile SnapshotEffectiveProfile() const
      TA_EXCL(chainlock_transaction_token, get_lock());

  SchedulerState::BaseProfile SnapshotBaseProfileLocked() const TA_REQ_SHARED(get_lock()) {
    return scheduler_state_.base_profile_;
  }

  SchedulerState::BaseProfile SnapshotBaseProfile() const
      TA_EXCL(chainlock_transaction_token, get_lock());

  ThreadStallState memory_stall_state() const TA_REQ(preempt_disabled_token) {
    return memory_stall_state_;
  }
  void set_memory_stall_state(ThreadStallState value) TA_REQ(preempt_disabled_token) {
    memory_stall_state_ = value;
  }

 private:
  // The architecture-specific methods for getting and setting the
  // current thread may need to see Thread's arch_ member via offsetof.
  friend inline Thread* arch_get_current_thread();
  friend inline void arch_set_current_thread(Thread*);

  // OwnedWaitQueues manipulate wait queue state.
  friend class OwnedWaitQueue;

  // ScopedThreadExceptionContext is the only public way to call
  // SaveUserStateLocked and RestoreUserStateLocked.
  friend class ScopedThreadExceptionContext;

  // StackOwnedLoanedPagesInterval is the only public way to set/clear the
  // stack_owned_loaned_pages_interval().
  friend class StackOwnedLoanedPagesInterval;

  // Dumping routines are allowed to see inside us.
  friend class ThreadDumper;

  // Type used to select which operation (suspend or kill) we want during a call
  // to SuspendOrKillInternal.
  enum class SuspendOrKillOp { Suspend, Kill };

  // The default trampoline used when running the Thread. This can be
  // replaced by the |alt_trampoline| parameter to CreateEtc().
  static void Trampoline() __NO_RETURN;

  // Dpc callback used for cleaning up a detached Thread's resources.
  static void FreeDpc(Dpc* dpc);

  // Save the arch-specific user state.
  //
  // Returns true when the user state will later need to be restored.
  [[nodiscard]] bool SaveUserStateLocked() TA_REQ(get_lock());

  // Restore the arch-specific user state.
  void RestoreUserStateLocked() TA_REQ(get_lock());

  // Common implementation of Suspend and Kill.
  zx_status_t SuspendOrKillInternal(SuspendOrKillOp op) TA_EXCL(chainlock_transaction_token);

  // Returns true if it decides to kill the thread, which must be the
  // current thread. The thread_lock must be held when calling this
  // function.
  //
  // TODO: move this to CurrentThread, once that becomes a subclass of
  // Thread.
  bool CheckKillSignal() TA_REQ(get_lock());

  // These should only be accessed from the current thread.
  bool restricted_kick_pending() const {
    return (flags_ & THREAD_FLAG_RESTRICTED_KICK_PENDING) != 0;
  }
  void set_restricted_kick_pending(bool value) {
    if (value) {
      flags_ |= THREAD_FLAG_RESTRICTED_KICK_PENDING;
    } else {
      flags_ &= ~THREAD_FLAG_RESTRICTED_KICK_PENDING;
    }
  }

  __NO_RETURN void ExitLocked(int retcode) TA_REQ(get_lock());

  static void DumpAllLocked(bool full) TA_REQ(list_lock_);
  static void DumpTidLocked(zx_koid_t tid, bool full) TA_REQ(list_lock_);

 private:
  struct MigrateListTrait {
    static fbl::DoublyLinkedListNodeState<Thread*>& node_state(Thread& thread) {
      return thread.migrate_list_node_;
    }
  };
  using MigrateList = fbl::DoublyLinkedListCustomTraits<Thread*, MigrateListTrait>;

  static inline DECLARE_SPINLOCK(Thread) list_lock_ TA_ACQ_BEFORE(lock_);

  // The global list of threads with migrate functions.
  static MigrateList migrate_list_ TA_GUARDED(list_lock_);

  Canary canary_;

  // These fields are among the most active in the thread. They are grouped
  // together near the front to improve cache locality.
  // mutable ChainLock lock_;
  __NO_UNIQUE_ADDRESS mutable fbl::NullLock scheduler_variable_lock_;
  unsigned int flags_{};
  // TODO(https://fxbug.dev/42077109): Write down memory order requirements for accessing signals_.
  ktl::atomic<unsigned int> signals_{};
  SchedulerState scheduler_state_ TA_GUARDED(get_lock());
  SchedulerQueueState scheduler_queue_state_ TA_GUARDED(scheduler_variable_lock_);
  WaitQueueCollection::ThreadState wait_queue_state_ TA_GUARDED(get_lock());
  fbl::DoublyLinkedList<OwnedWaitQueue*> owned_wait_queues_ TA_GUARDED(get_lock());
  TaskState task_state_;
  PreemptionState preemption_state_;
  MemoryAllocationState memory_allocation_state_;

  // Must only be accessed by "this" thread. May be null.
  ktl::unique_ptr<RestrictedState> restricted_state_;

  // This is part of ensuring that all stack ownership of loaned pages can be boosted in priority
  // via priority inheritance if a higher priority thread is trying to reclaim the loaned pages.
  StackOwnedLoanedPagesInterval* stack_owned_loaned_pages_interval_ = nullptr;

#if WITH_LOCK_DEP
  // state for runtime lock validation when in thread context
  lockdep::ThreadLockState lock_state_;
#endif

  // The current address space this thread is associated with.
  // This can be null if this is a kernel thread.
  // See the comments near active_aspace() for more details.
  RelaxedAtomic<VmAspace*> aspace_{nullptr};

  // Saved by SignalPolicyException() to store the type of policy error, and
  // passed to exception disptach in ProcessPendingSignals().
  uint32_t extra_policy_exception_code_ TA_GUARDED(get_lock()) = 0;
  uint32_t extra_policy_exception_data_ TA_GUARDED(get_lock()) = 0;

  // Is this thread allowed to own wait queues?  Set to false by a thread as
  // it exits, but before it reaches the DEAD state.
  bool can_own_wait_queues_ TA_GUARDED(get_lock()) = true;

  // Strong reference to user thread if one exists for this thread.
  // In the common case freeing Thread will also free ThreadDispatcher when this
  // reference is dropped.
  fbl::RefPtr<ThreadDispatcher> user_thread_;

  // When user_thread_ is set, these values are copied from ThreadDispatcher and
  // its parent ProcessDispatcher. Kernel threads maintain an independent tid.
  zx_koid_t tid_ = KernelObjectId::Generate();
  zx_koid_t pid_ = ZX_KOID_INVALID;

  // Architecture-specific stuff.
  arch_thread arch_{};

  KernelStack stack_;

  // This is used by dispatcher.cc:SafeDeleter.
  void* recursive_object_deletion_list_ = nullptr;

  // This always includes the trailing NUL.
  char name_[ZX_MAX_NAME_LEN]{};

  // Buffering for Debuglog output.
  Linebuffer linebuffer_;

#if LOCK_TRACING_ENABLED
  // The flow id allocated before blocking on the last lock.
  RelaxedAtomic<uint64_t> lock_flow_id_{0};

  // Generates unique flow ids for tracing lock contention.
  inline static RelaxedAtomic<uint64_t> lock_flow_id_generator_{0};
#endif

  // Indicates whether user register state (debug, vector, fp regs, etc.) has been saved to the
  // arch_thread_t as part of thread suspension / exception handling.
  //
  // When a user thread is suspended or generates an exception (synthetic or architectural) that
  // might be observed by another process, we save user register state to the thread's arch_thread_t
  // so that it may be accessed by a debugger.  Upon leaving a suspended or exception state, we
  // restore user register state.
  bool user_state_saved_ TA_GUARDED(get_lock()){false};

  // For threads with migration functions, indicates whether a migration is in progress. When true,
  // the migrate function has been called with Before but not yet with After.
  //
  // TODO(johngro): What to do about this?  Should it be protected by the
  // thread's lock?  Does the thread's lock need to be held exclusively when a
  // thread's migrate function is called?
  bool migrate_pending_ TA_GUARDED(get_lock()){};

  // Provides a way to execute custom logic when a thread must be migrated between CPUs.
  MigrateFn migrate_fn_ TA_GUARDED(get_lock());

  // Provides a way to execute custom logic when a thread is context switched to or away from.
  ContextSwitchFn context_switch_fn_;

  // Used to track threads that have set |migrate_fn_|. This is used to migrate
  // threads before a CPU is taken offline.
  fbl::DoublyLinkedListNodeState<Thread*> migrate_list_node_ TA_GUARDED(list_lock_);

  // Node storage for existing on the global thread list.
  fbl::DoublyLinkedListNodeState<Thread*> thread_list_node_ TA_GUARDED(list_lock_);

  // Node storage for existing on the temporary batch unblock list.
  fbl::DoublyLinkedListNodeState<Thread*> unblock_list_node_ TA_GUARDED(get_lock());

  // Node storage for existing on the save state list.
  fbl::SinglyLinkedListNodeState<Thread*> save_state_list_node_ TA_GUARDED(get_lock());

  // State of the thread with respect to memory stalls.
  //
  // It can only be accessed by the thread's current CPU. Hence, preemption must
  // be disabled while accessing it.
  ThreadStallState memory_stall_state_ TA_GUARDED(preempt_disabled_token) =
      ThreadStallState::IgnoredKernelOnly;
};

// For the moment, the arch-specific current thread implementations need to come here, after the
// Thread definition. One of the arches needs to know the structure of Thread to compute the offset
// that the hardware pointer holds into Thread.
#include <arch/current_thread.h>
Thread* Thread::Current::Get() { return arch_get_current_thread(); }

void arch_dump_thread(const Thread* t) TA_REQ_SHARED(t->get_lock());

class ThreadDumper {
 public:
  static void DumpLocked(const Thread* t, bool full) TA_REQ_SHARED(t->get_lock());
};

inline void Thread::DumpDuringPanic(bool full) const TA_NO_THREAD_SAFETY_ANALYSIS {
  ThreadDumper::DumpLocked(this, full);
}

inline void Thread::DumpAllDuringPanic(bool full) TA_NO_THREAD_SAFETY_ANALYSIS {
  // Skip grabbing the lock if we are panic'ing
  DumpAllLocked(full);
}

inline void Thread::DumpTidDuringPanic(zx_koid_t tid, bool full) TA_NO_THREAD_SAFETY_ANALYSIS {
  // Skip grabbing the lock if we are panic'ing
  DumpTidLocked(tid, full);
}

// TODO(johngro): Remove this when we have addressed https://fxbug.dev/42108673.  Right now, this
// is used in only one place (x86_bringup_aps in arch/x86/smp.cpp) outside of
// thread.cpp.
//
// Normal users should only ever need to call either Thread::Create, or
// Thread::CreateEtc.
void construct_thread(Thread* t, const char* name);

// Other thread-system bringup functions.
void thread_init_early();
void thread_secondary_cpu_entry() __NO_RETURN;
void thread_construct_first(Thread* t, const char* name);

// Call the arch-specific signal handler.
extern "C" void arch_iframe_process_pending_signals(iframe_t* iframe);

// find a thread based on the thread id
// NOTE: used only for debugging, its a slow linear search through the
// global thread list
//
// Note: any thread pointer borrowed by this function is only guaranteed to be
// alive as long as the Thread::list_lock is held.  Once this is dropped, the
// thread could cease to exist at any time, and the pointer must be considered
// to be invalid.
Thread* thread_id_to_thread_slow(zx_koid_t tid) TA_REQ(Thread::get_list_lock());

// RAII helper that installs/removes an exception context and saves/restores user register state.
// The class operates on the current thread.
//
// When a thread takes an exception, this class is used to make user register state available to
// debuggers and exception handlers.
//
// Example Usage:
//
// {
//   ScopedThreadExceptionContext context(...);
//   HandleException();
// }
//
// Note, ScopedThreadExceptionContext keeps track of whether the state has already been saved so
// it's safe to nest them:
//
// void Foo() {
//   ScopedThreadExceptionContext context(...);
//   Bar();
// }
//
// void Bar() {
//   ScopedThreadExceptionContext context(...);
//   Baz();
// }
//
class ScopedThreadExceptionContext {
 public:
  explicit ScopedThreadExceptionContext(const arch_exception_context_t* context);
  ~ScopedThreadExceptionContext();
  DISALLOW_COPY_ASSIGN_AND_MOVE(ScopedThreadExceptionContext);

 private:
  Thread* thread_;
  const arch_exception_context_t* context_;
  bool need_to_remove_;
  bool need_to_restore_;
};

// RAII helper to enforce that a block of code does not allocate memory.
//
// See |Thread::Current::memory_allocation_state()|.
class ScopedMemoryAllocationDisabled {
 public:
  ScopedMemoryAllocationDisabled() { Thread::Current::memory_allocation_state().Disable(); }
  ~ScopedMemoryAllocationDisabled() { Thread::Current::memory_allocation_state().Enable(); }
  DISALLOW_COPY_ASSIGN_AND_MOVE(ScopedMemoryAllocationDisabled);
};

// AssertInWaitQueue asserts (as its name implies) that this thread is
// currently blocked in |wq|, and that the wait queue is locked.  This should
// be sufficient to establish (at runtime) that it is safe to allow
// read-access to the thread's members which are used to track the thread's
// state in the queue.
//
// TODO(johngro) : Link to something documenting the reasoning behind this.
// 1) Consider making these asserts extra-debug-only asserts, so that most
//    builds do not need to suffer the runtime overhead of using them.
// 2) Consider ways to limit the scope of the read-only capability to just the
//    wait queue state (and other things protected by the fact that we are
//    blocked in the wait queue) and nothing else.  While the wait queue state
//    cannot mutate while we exist in the wait queue (at least, not without
//    holding both the wait queue and thread's lock), other things can.
static inline void AssertInWaitQueue(const Thread& t, const WaitQueue& wq) TA_REQ(wq.get_lock())
    TA_ASSERT_SHARED(t.get_lock()) {
  [&]() TA_NO_THREAD_SAFETY_ANALYSIS {
    DEBUG_ASSERT(t.wait_queue_state().InWaitQueue());
    DEBUG_ASSERT(t.state() == THREAD_BLOCKED || t.state() == THREAD_BLOCKED_READ_LOCK);
    DEBUG_ASSERT(t.wait_queue_state().blocking_wait_queue() == &wq);
  }();
}

// Note: This implementation *must* come after the implementation of GetThreadsLock.
inline bool WaitQueueCollection::IsFairThreadSortBitSet(const Thread& t) {
  const uint64_t key = t.wait_queue_state().blocked_threads_tree_sort_key_;
  return (key & kFairThreadSortKeyBit) != 0;
}

////////////////////////////////////////////////////////////////////////////////
//
// Here and below, we need to deal with static analysis.  Right now, we need to
// disable static analysis while we figure it out.
//
////////////////////////////////////////////////////////////////////////////////

// WaitQueue collection trait implementations.  While typically these would be
// implemented in-band in the trait class itself, these definitions must come
// last, after the definition Thread.  This is because the traits (defined in
// WaitQueueCollection) need to understand the layout of Thread in order to be
// able to access both scheduler state and wait queue state variables.
inline WaitQueueCollection::Key WaitQueueCollection::BlockedThreadTreeTraits::GetKey(
    const Thread& thread) TA_NO_THREAD_SAFETY_ANALYSIS {
  // TODO(johngro): consider extending FBL to support a "MultiWAVLTree"
  // implementation which would allow for nodes with identical keys, breaking
  // ties (under the hood) using pointer value.  This way, we would not need to
  // manifest our own pointer in GetKey or in our key type.
  return {thread.wait_queue_state().blocked_threads_tree_sort_key_,
          reinterpret_cast<uintptr_t>(&thread)};
}

inline fbl::WAVLTreeNodeState<Thread*>& WaitQueueCollection::BlockedThreadTreeTraits::node_state(
    Thread& thread) TA_NO_THREAD_SAFETY_ANALYSIS {
  return thread.wait_queue_state().blocked_threads_tree_node_;
}

inline Thread* WaitQueueCollection::MinRelativeDeadlineTraits::GetValue(const Thread& thread)
    TA_NO_THREAD_SAFETY_ANALYSIS {
  // TODO(johngro), consider pre-computing this value so it is just a fetch
  // instead of a branch.
  return (thread.scheduler_state().discipline() == SchedDiscipline::Fair)
             ? nullptr
             : const_cast<Thread*>(&thread);
}

inline Thread* WaitQueueCollection::MinRelativeDeadlineTraits::GetSubtreeBest(const Thread& thread)
    TA_NO_THREAD_SAFETY_ANALYSIS {
  return thread.wait_queue_state().subtree_min_rel_deadline_thread_;
}

inline bool WaitQueueCollection::MinRelativeDeadlineTraits::Compare(Thread* a, Thread* b)
    TA_NO_THREAD_SAFETY_ANALYSIS {
  // The thread pointer value of a non-deadline thread is null, an non-deadline
  // threads are always the worst choice when choosing the thread with the
  // minimum relative deadline.
  // clang-format off
  if (a == nullptr) { return false; }
  if (b == nullptr) { return true; }
  const SchedDuration a_deadline = a->scheduler_state().effective_profile().deadline.deadline_ns;
  const SchedDuration b_deadline = b->scheduler_state().effective_profile().deadline.deadline_ns;
  return (a_deadline < b_deadline) || ((a_deadline == b_deadline) && (a < b));
  // clang-format on
}

inline void WaitQueueCollection::MinRelativeDeadlineTraits::AssignBest(Thread& thread, Thread* val)
    TA_NO_THREAD_SAFETY_ANALYSIS {
  thread.wait_queue_state().subtree_min_rel_deadline_thread_ = val;
}

inline void WaitQueueCollection::MinRelativeDeadlineTraits::ResetBest(Thread& thread)
    TA_NO_THREAD_SAFETY_ANALYSIS {
  // In a debug build, zero out the subtree best as we leave the collection.
  // This can help to find bugs by allowing us to assert that the value is zero
  // during insertion, however it is not strictly needed in a production build
  // and can be skipped.
#ifdef DEBUG_ASSERT_IMPLEMENTED
  thread.wait_queue_state().subtree_min_rel_deadline_thread_ = nullptr;
#endif
}

inline void PreemptionState::AssertCapabilityHeld(
    concurrent::CapabilityToken<PreemptionState>::TagType) {
  DEBUG_ASSERT(Thread::Current::preemption_state().PreemptIsEnabled() == false);
}

inline SchedulerState::WaitQueueInheritedSchedulerState*
WaitQueueCollection::FindInheritedSchedulerStateStorage() TA_NO_THREAD_SAFETY_ANALYSIS {
  return threads_.is_empty()
             ? nullptr
             : &threads_.back().wait_queue_state().inherited_scheduler_state_storage_;
}

struct BrwLockOps {
  // Attempt to lock the set of threads needed for a BrwLock wake operation.  If
  // the first thread to wake is a writer, the set consists of just this thread.
  // If the first thread to wake is a reader, then the set to wake is all of the
  // readers we encounter until we hit another writer, or run out of threads to
  // wake.
  //
  // On success, the list of threads to wake is returned with each thread
  // locked, and with each thread prepared to unblock as if Dequeue had been
  // called (blocked_status has been set to OK, and blocking_wait_queue has been
  // cleared).
  //
  // On failure, no list is returned, and all threads will be unlocked and
  // waiting in the collection.
  struct LockForWakeResult {
    Thread::UnblockList list;
    uint32_t count{0};
  };

  static ktl::optional<LockForWakeResult> LockForWake(WaitQueue& queue, zx_instant_mono_t now)
      TA_REQ(chainlock_transaction_token, queue.get_lock());
};

struct WaitQueueLockOps {
  // Attempt to lock one or all threads for waking.
  //
  // When successful, removes the threads from the wait queue, moving them to a
  // temporary unblock list and setting their wake result in the process.
  // Callers may then perform any internal bookkeeping before dropping the queue
  // lock, and finally waking the threads using Scheduler::Unblock.
  //
  // Returns std::nullopt in the case that a backoff condition is encountered.
  static ktl::optional<Thread::UnblockList> LockForWakeAll(WaitQueue& queue,
                                                           zx_status_t wait_queue_error)
      TA_REQ(chainlock_transaction_token, queue.get_lock());
  static ktl::optional<Thread::UnblockList> LockForWakeOne(WaitQueue& queue,
                                                           zx_status_t wait_queue_error)
      TA_REQ(chainlock_transaction_token, queue.get_lock());
};

// RAII class to mark sections of code as memory stalls.
//
// Nesting is not allowed. It is a programming error to instantiate this class
// if there exists another instance on the call stack.
class ScopedMemoryStall {
 public:
  ScopedMemoryStall();
  ~ScopedMemoryStall();

  // Disallow copying, assigning and moving.
  ScopedMemoryStall(const ScopedMemoryStall&) = delete;
  ScopedMemoryStall(ScopedMemoryStall&&) = delete;
  ScopedMemoryStall& operator=(const ScopedMemoryStall&) = delete;
  ScopedMemoryStall& operator=(ScopedMemoryStall&&) = delete;
};

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_THREAD_H_
