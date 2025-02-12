// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_DPC_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_DPC_H_

#include <lib/kconcurrent/chainlock.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <fbl/intrusive_double_list.h>
#include <kernel/event.h>
#include <kernel/thread.h>

// Deferred Procedure Calls - queue callback to invoke on the current cpu in thread context.
// Dpcs are executed with interrupts enabled, and do not ever migrate cpus while executing.
// A Dpc may not execute on the original current cpu if it is hotunplugged/offlined.
// Dpcs may block, though this may starve other queued work.

class Dpc : public fbl::DoublyLinkedListable<Dpc*, fbl::NodeOptions::AllowCopyMove> {
 public:
  using Func = void(Dpc*);

  explicit Dpc(Func* func = nullptr, void* arg = nullptr) : func_(func), arg_(arg) {}

  template <class ArgType>
  ArgType* arg() {
    return static_cast<ArgType*>(arg_);
  }

  // Queue this object and signal the worker thread to execute it.
  //
  // |Queue| will not block, but it may wait briefly for a spinlock.
  //
  // |Queue| may return before or after the Dpc has executed.  It is the
  // caller's responsibility to ensure that a queued Dpc object is not destroyed
  // prior to its execution.
  //
  // Note: it is important that the thread calling Queue() not be holding its
  // own lock when calling queue if there is _any_ possibility that the DPC
  // thread could be involved in a PI interaction with the thread who is
  // performing the queue operation.
  //
  // Returns ZX_ERR_ALREADY_EXISTS if |this| is already queued.
  zx_status_t Queue() TA_EXCL(chainlock_transaction_token);

  // A single spinlock protects the data members of all DPC queues.
  //
  // Take care to drop this lock before calling any code that might acquire a chainlock.
  DECLARE_SINGLETON_SPINLOCK(Lock);

 private:
  friend class DpcQueue;

  // The DpcQueue this Dpc gets enqueued onto is the only thing to actually Invoke this Dpc,
  // on its worker thread.
  void Invoke() TA_EXCL(Dpc::Lock::Get());

  Func* func_;
  void* arg_;
};

// Each cpu maintains a DpcQueue, in its percpu structure.
class DpcQueue {
 public:
  // Initializes this DpcQueue for the current cpu.
  //
  // The calling thread must have hard-affinity to exactly one CPU.
  void InitForCurrentCpu();

  // Begins the Dpc shutdown process for the owning cpu.
  //
  // Shutting down a Dpc queue is a two-phase process.  This is the first phase.  See
  // |TransitionOffCpu| for the second phase.
  //
  // This method:
  // - tells the owning cpu's Dpc thread to stop servicing its queue then
  // - waits, up to |deadline|, for it to finish any in-progress DPC and join
  //
  // Because this method blocks until the Dpc thread has terminated, it is critical that the caller
  // not hold any locks that might be needed by any previously queued DPCs.  Otheriwse, deadlock may
  // occur.
  //
  // Upon successful completion, this DpcQueue may contain unexecuted Dpcs and new ones
  // may be added by |Queue|.  However, they will not execute (on any cpu) until
  // |TransitionOffCpu| is called.
  //
  // Once |Shutdown| has completed successfully, finish the shutdown process by calling
  // |TransitionOffCpu| on some cpu other than the owning cpu.
  //
  // If |Shutdown| fails, this DpcQueue is left in an undefined state and
  // |TransitionOffCpu| must not be called.
  zx_status_t Shutdown(zx_instant_mono_t deadline);

  // Moves queued Dpcs from |source| to this DpcQueue.
  //
  // This is the second phase of Dpc shutdown.  See |Shutdown|.
  //
  // This must only be called after |Shutdown| has completed successfully.
  //
  // This must only be called on the current cpu.
  void TransitionOffCpu(DpcQueue& source);

  // These are called by Dpc::Queue.
  void EnqueueLocked(Dpc* dpc) TA_REQ(Dpc::Lock::Get());
  void Signal() TA_EXCL(Dpc::Lock::Get());

 private:
  static int WorkerThread(void* unused);
  int Work();

  // The cpu that owns this DpcQueue.
  cpu_num_t cpu_ TA_GUARDED(Dpc::Lock::Get()) = INVALID_CPU;

  // Whether the DpcQueue has been initialized for the owning cpu.
  bool initialized_ TA_GUARDED(Dpc::Lock::Get()) = false;

  // Request the thread_ to stop by setting to true.
  //
  // This guarded by the static global dpc_lock.
  bool stop_ TA_GUARDED(Dpc::Lock::Get()) = false;

  fbl::DoublyLinkedList<Dpc*> list_ TA_GUARDED(Dpc::Lock::Get());

  // Each cpu maintains a dedicated thread for processing Dpcs.
  Thread* thread_ TA_GUARDED(Dpc::Lock::Get()) = nullptr;

  Event event_;
};

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_DPC_H_
