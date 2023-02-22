// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_OWNED_WAIT_QUEUE_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_OWNED_WAIT_QUEUE_H_

#include <fbl/canary.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/macros.h>
#include <kernel/scheduler_state.h>
#include <kernel/thread.h>
#include <kernel/thread_lock.h>
#include <kernel/wait.h>
#include <ktl/optional.h>

// Owned wait queues are an extension of wait queues which adds the concept of
// ownership for use when profile inheritance semantics are needed.
//
// An owned wait queue maintains an unmanaged pointer to a Thread in order to
// track who owns it at any point in time.  In addition, it contains node state
// which can be used by the owning thread in order to track the wait queues that
// the thread is currently an owner of.  This also makes use of unmanaged
// pointer.
//
// It should be an error for any thread to destruct while it owns an
// OwnedWaitQueue.  Likewise, it should be an error for any wait queue to
// destruct while it has an owner.  These invariants are enforced in the
// destructor for OwnedWaitQueue and Thread.  This enforcement is considered
// to be the reasoning why holding unmanaged pointers is considered to be safe.
//
class OwnedWaitQueue : protected WaitQueue, public fbl::DoublyLinkedListable<OwnedWaitQueue*> {
 public:
  // We want to limit access to our base WaitQueue's methods, but not all of
  // them.  Make public the methods which should be safe for folks to use from
  // the OwnedWaitQueue level of things.
  //
  // This list is pretty short right now, and there are probably other methods
  // which could be added safely (WakeOne, WakeAll, Peek, etc...) we'd rather
  // keep the list as short as possible for now, and only expand it when there
  // is a tangible need, and a thorough review for safety.
  //
  // The general rule of thumb here is that code which knows that it using an
  // OwnedWaitQueue should go through the OWQ specific APIs instead of
  // attempting to use the base WaitQueue APIs.
  using WaitQueue::Count;
  using WaitQueue::IsEmpty;

  // A small helper class which can be injected into Wake and Requeue
  // operations to allow calling code to get a callback for each thread which
  // is either woken, or requeued.  This callback serves two purposes...
  //
  // 1) It allows the caller to perform some limited filtering operations, and
  //    to choose which thread (if any) becomes the new owner of the queue.
  //    See the comments in the |Action| enum member for details.
  // 2) It gives code such as |FutexContext| a chance to perform their own
  //    per-thread bookkeeping as the wait queue code chooses which threads to
  //    either wake or re-queue.
  //
  // Note that during a wake or requeue operation, the threads being
  // considered will each be presented to the user provided Hook (if any)
  // by the OwnedWaitQueue code before deciding whether or not to actually
  // wake or requeue the thread.
  class Hook {
   public:
    // A set of 3 actions which may be taken when considering whether or not
    // to wake or requeue a thread.  If no user supplied Hook is provided
    // for a given operation, the default behavior will be to return
    // Action::SelectAndKeepGoing.
    enum class Action {
      // Do not wake or requeue this thread and stop considering threads.
      Stop,

      // Select this thread to be either woken or requeued, then continue
      // to consider more threads (if any).  Do not assign this thread to
      // be the owner.
      SelectAndKeepGoing,

      // Select this thread to be either woken or requeued, assign it to
      // to be the owner of the queue, then stop considering more threads.
      // It is illegal to wake a thread and assign it as the owner for the
      // queue if at least one thread has already been woken.
      SelectAndAssignOwner,
    };

    using Callback = Action (*)(Thread* thrd, void* ctx);

    Hook() : cbk_(nullptr) {}
    Hook(Callback cbk, void* ctx) : cbk_(cbk), ctx_(ctx) {}

    Action operator()(Thread* thrd) const TA_REQ(thread_lock) {
      return cbk_ ? cbk_(thrd, ctx_) : Action::SelectAndKeepGoing;
    }

   private:
    Callback cbk_;
    void* ctx_;
  };

  // A enum which determines the specific behavior of the Propagate method.
  enum class PropagateOp {
    // Add a single edge from the upstream node to the downstream node, and
    // propagate the effects. This happens either when a thread blocks in an
    // OWQ, or when an owner is assigned to an OWQ.
    AddSingleEdge,

    // Remove a single edge from the upstream node to the downstream node. This
    // happens either when a thread unblocks from an OWQ, or when an OWQ becomes
    // unowned.
    RemoveSingleEdge,

    // The base profile of an upstream node has changed, causing a change to the
    // profile pressure without an explicit join or split operation.
    BaseProfileChanged,
  };

  template <PropagateOp Op>
  struct PropagateOpTag {
    constexpr operator PropagateOp() const { return Op; }
  };

  static constexpr PropagateOpTag<PropagateOp::AddSingleEdge> AddSingleEdgeOp{};
  static constexpr PropagateOpTag<PropagateOp::RemoveSingleEdge> RemoveSingleEdgeOp{};
  static constexpr PropagateOpTag<PropagateOp::BaseProfileChanged> BaseProfileChangedOp{};

  static constexpr uint32_t kOwnedMagic = fbl::magic("ownq");
  constexpr OwnedWaitQueue() : WaitQueue(kOwnedMagic) {}
  ~OwnedWaitQueue();

  // No copy or move is permitted.
  DISALLOW_COPY_ASSIGN_AND_MOVE(OwnedWaitQueue);

  // Release ownership of all wait queues currently owned by |t| and update
  // bookkeeping as appropriate.  This is meant to be called from the thread
  // itself and therefor it is assumed that the thread in question is not
  // blocked on any other wait queues.
  static void DisownAllQueues(Thread* t) TA_REQ(thread_lock);

  // Change a thread's base profile and deal with profile propagation side effects.
  static void SetThreadBaseProfileAndPropagate(Thread& thread,
                                               const SchedulerState::BaseProfile& profile)
      TA_REQ(thread_lock, preempt_disabled_token);

  // const accessor for the owner member.
  Thread* owner() const TA_REQ(thread_lock) { return owner_; }

  // Debug Assert wrapper which skips the thread analysis checks just to
  // assert that a specific queue is unowned.  Used by FutexContext
  void AssertNotOwned() const TA_NO_THREAD_SAFETY_ANALYSIS { DEBUG_ASSERT(owner_ == nullptr); }

  // Assign ownership of this wait queue to |new_owner|, or explicitly release
  // ownership if |new_owner| is nullptr.  No change is made to ownership if it
  // would result in a cycle in the inheritance graph.
  //
  // Note, if the new owner exists, but is dead or dying, it will not be
  // permitted to become the new owner of the wait_queue.  Any existing owner
  // will be replaced with no owner in this situation.
  //
  // Returns ZX_ERR_BAD_STATE if a cycle would have been produced, and ZX_OK
  // otherwise.
  zx_status_t AssignOwner(Thread* new_owner) TA_REQ(thread_lock, preempt_disabled_token);

  // Block the current thread on this wait queue, and re-assign ownership to
  // the specified thread (or remove ownership if new_owner is null);  If a
  // cycle would have been produced by this operation, no changes are made and
  // ZX_ERR_BAD_STATE will be returned.
  //
  // Note, if the new owner exists, but is dead or dying, it will not be
  // permitted to become the new owner of the wait_queue.  Any existing owner
  // will be replaced with no owner in this situation.
  zx_status_t BlockAndAssignOwner(const Deadline& deadline, Thread* new_owner,
                                  ResourceOwnership resource_ownership, Interruptible interruptible)
      TA_REQ(thread_lock, preempt_disabled_token);

  // Wake the up to specified number of threads from the wait queue and then
  // handle the ownership bookkeeping based on what the Hook told us to do.
  // See |Hook::Action| for details.
  void WakeThreads(uint32_t wake_count, Hook on_thread_wake_hook = {})
      TA_REQ(thread_lock, preempt_disabled_token);

  // A specialization of WakeThreads which will...
  //
  // 1) Wake the number of threads indicated by |wake_count|
  // 2) Move the number of threads indicated by |requeue_count| to the |requeue_target|.
  // 3) Update ownership bookkeeping as indicated by |owner_action| and |requeue_owner|.
  //
  // This method is used by futexes in order to implement futex_requeue.  It
  // is wrapped up into a specialized form instead of broken into individual
  // parts in order to minimize any thrash in re-computing effective
  // profiles for PI purposes.  We don't want to re-evaluate ownership or PI
  // pressure until after all of the changes to wait queue have taken place.
  //
  // |requeue_target| *must* be non-null.  If there is no |requeue_target|,
  // use WakeThreads instead.
  //
  // Note, if the |requeue_owner| exists, but is dead or dying, it will not be
  // permitted to become the new owner of the |requeue_target|.  Any existing
  // owner will be replaced with no owner in this situation.
  void WakeAndRequeue(uint32_t wake_count, OwnedWaitQueue* requeue_target, uint32_t requeue_count,
                      Thread* requeue_owner, Hook on_thread_wake_hook = {},
                      Hook on_thread_requeue_hook = {}) TA_REQ(thread_lock, preempt_disabled_token);

  // Accessor used only by the scheduler's PiNodeAdapter to handle bookkeeping
  // during profile inheritance situations.
  SchedulerState::WaitQueueInheritedSchedulerState* inherited_scheduler_state_storage() {
    return inherited_scheduler_state_storage_;
  }

 private:
  // Give permission to the WaitQueue thunk to call the PropagateRemove method
  friend zx_status_t WaitQueue::UnblockThread(Thread* t, zx_status_t wait_queue_error);

  void AssignOwnerInternal(Thread* new_owner) TA_REQ(thread_lock, preempt_disabled_token);

  // Wake the specified number of threads from the wait queue, calling the user
  // supplied on_thread_wake_hook as we go to allow the user to maintain their
  // bookkeeping, and choose a new owner if desired.
  void WakeThreadsInternal(uint32_t wake_count, zx_time_t now, Hook on_thread_wake_hook)
      TA_REQ(thread_lock, preempt_disabled_token);

  void ValidateSchedStateStorageUnconditional();
  void ValidateSchedStateStorage() {
    if constexpr (kSchedulerExtraInvariantValidation) {
      ValidateSchedStateStorageUnconditional();
    }
  }

  void UpdateSchedStateStorageThreadRemoved(Thread& t) TA_REQ(thread_lock) {
    DEBUG_ASSERT(inherited_scheduler_state_storage_ != nullptr);

    SchedulerState::WaitQueueInheritedSchedulerState& old_iss =
        t.wait_queue_state().inherited_scheduler_state_storage_;
    if (&old_iss == inherited_scheduler_state_storage_) {
      inherited_scheduler_state_storage_ = collection_.FindInheritedSchedulerStateStorage();
      if (inherited_scheduler_state_storage_) {
        DEBUG_ASSERT(&old_iss != inherited_scheduler_state_storage_);
        *inherited_scheduler_state_storage_ = old_iss;
      }
      old_iss.Reset();
    }
  }

  void UpdateSchedStateStorageThreadAdded(Thread& t) TA_REQ(thread_lock) {
    if (inherited_scheduler_state_storage_ == nullptr) {
      DEBUG_ASSERT_MSG(this->Count() == 1, "Expected count == 1, instead of %u", this->Count());
      inherited_scheduler_state_storage_ = &t.wait_queue_state().inherited_scheduler_state_storage_;
      inherited_scheduler_state_storage_->AssertIsReset();
    } else {
      DEBUG_ASSERT_MSG(this->Count() > 1, "Expected count > 1, instead of %u", this->Count());
    }
  }

  // Helper function for propagating inherited profile value add and remove
  // operations.  Snapshots and returns the combination of a thread's currently
  // inherited profile values along with its base profile, which should be the
  // profile pressure it is transmitting to the next node in the graph.
  static SchedulerState::InheritedProfileValues SnapshotThreadIpv(Thread& thread)
      TA_REQ(thread_lock);

  // Apply a change in IPV to a thread.  When non-null, |old_ipv| points to the
  // IPV values which need to be removed from the thread's IPVs, while |new_ipv|
  // points to the IPV vales which need to be added to the  thread's IPVs.
  //
  // 1) When a graph edge is added, |old_ipv| will be nullptr as no IPV pressure
  //    was removed.
  // 2) When a graph edge is removed, |new_ipv| will be nullptr as no IPV
  //    pressure was added.
  // 3) When a thread's base profile changes, neither |old_ipv| nor |new_ipv|
  //    will be nullptr, since the change of a thread's base profile is
  //    equivalent to the removal of one set of IPV, and the addition of
  //    another.
  // 4) It is never correct for both |old_ipv| and |new_ipv| to be nullptr.
  //
  static void ApplyIpvDeltaToThread(const SchedulerState::InheritedProfileValues* old_ipv,
                                    const SchedulerState::InheritedProfileValues* new_ipv,
                                    Thread& thread) TA_REQ(thread_lock);

  // Apply a change in IPV to an owned wait queue.  See ApplyIpvDeltaToThread
  // for an explanation of the parameters.
  static void ApplyIpvDeltaToOwq(const SchedulerState::InheritedProfileValues* old_ipv,
                                 const SchedulerState::InheritedProfileValues* new_ipv,
                                 OwnedWaitQueue& owq) TA_REQ(thread_lock);

  // Checks to see if a cycle would be formed if |owner_thread| was to become
  // the owner of |owq| and |blocking_thread| were to block in |owq|.
  //
  // While |owq| and |owner_thread| are required parameters, |blocking_thread|
  // is optional. Users may test to see if a change of ownership would for a
  // cycle even without a thread blocking concurrently by passing nullptr for
  // |blocking_thread|.
  static bool CheckForCycle(const OwnedWaitQueue* owq, const Thread* owner_thread,
                            const Thread* blocking_thread = nullptr) TA_REQ(thread_lock);

  // Begin a propagation operation starting from an upstream thread, and going
  // through a downstream owned wait queue.  Only for use during edge add/remove
  // operations. Base profile changes call FinishPropagate directly when
  // required.  Note: Links between the upstream thread and downstream queue
  // should have already been added/removed by the time that this method is
  // called.
  template <PropagateOp Op>
  static void BeginPropagate(Thread& upstream_node, OwnedWaitQueue& downstream_node,
                             PropagateOpTag<Op>) TA_REQ(thread_lock, preempt_disabled_token);

  // Begin a propagation operation starting from an upstream owned wait queue,
  // and going through a downstream thread.  Only for use during edge add/remove
  // operations. Base profile changes call FinishPropagate directly when
  // required.  Note: Links between the upstream queue and downstream thread
  // should *not* have already been added/removed by the time that this method
  // is called.  The method will handle updating the links.
  template <PropagateOp Op>
  static void BeginPropagate(OwnedWaitQueue& upstream_node, Thread& downstream_node,
                             PropagateOpTag<Op>) TA_REQ(thread_lock, preempt_disabled_token);

  // Finishing handling a propagation operations started from either version of
  // BeginPropagate, or from SetThreadBasePriority.  Traverses the PI chain
  // propagating IPV deltas, and calls into the scheduler to finish the
  // operation once the end of the chain is reached.
  template <PropagateOp Op, typename UpstreamNodeType, typename DownstreamNodeType>
  static void FinishPropagate(UpstreamNodeType& upstream_node, DownstreamNodeType& downstream_node,
                              const SchedulerState::InheritedProfileValues* added_ipv,
                              const SchedulerState::InheritedProfileValues* lost_ipv,
                              PropagateOpTag<Op>) TA_REQ(thread_lock, preempt_disabled_token);

  // Upcast from a WaitQueue to an OwnedWaitQueue if possible, using the magic
  // number to detect the underlying nature of the object.  Returns nullptr if
  // the pointer passed is nullptr, or if the object is not an OwnedWaitQueue.
  static OwnedWaitQueue* DowncastToOwq(WaitQueue* wq) {
    return (wq != nullptr) && (wq->magic() == kOwnedMagic) ? static_cast<OwnedWaitQueue*>(wq)
                                                           : nullptr;
  }

  TA_GUARDED(thread_lock) Thread* owner_ = nullptr;

  // A pointer to a thread (which _must_ be a current member of this owned wait queue's)
  // collection which holds the current inherited scheduler state storage for
  // this wait queue.
  //
  // Anytime that a thread is added to this queue, if the collection was
  // empty, then the thread added becomes the new location for the storage.
  //
  // Anytime that a thread is removed from this queue, if the thread being
  // removed was the active location of the storage, then a new thread needs to
  // be selected and the values moved into the new storage location.
  //
  // The primary purpose of this indirection is to keep the OwnedWaitQueue object
  // relatively-lightweight.  The only time that a wait queue has defined
  // inherited scheduler state is when there are one or more threads blocked in
  // the queue, so storing this bookkeeping in a blocked thread object seems
  // reasonable.
  //
  // TODO(johngro): An alternative approach would be to copy the token-ante
  // system used by Theads and FutexContext.
  //
  // Pros: Easier to reason about code and reduced risk of accidental
  //       use-after-free.  Also, no need to copy bookkeeping from one thread to
  //       another when the currently active thread leaves the OwnedWaitQueue.
  // Cons: Storage of the free tokens would require some central pool system
  //       which would act as a central choke point, which might become a
  //       scalability issue on systems where the processor count is high.
  SchedulerState::WaitQueueInheritedSchedulerState* inherited_scheduler_state_storage_{nullptr};
};

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_OWNED_WAIT_QUEUE_H_
