// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <debug.h>
#include <lib/boot-options/boot-options.h>
#include <lib/fit/defer.h>
#include <lib/fxt/fields.h>
#include <lib/fxt/interned_category.h>
#include <lib/ktrace.h>
#include <lib/ktrace/ktrace_internal.h>
#include <lib/syscalls/zx-syscall-numbers.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <platform.h>
#include <string.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <arch/ops.h>
#include <arch/user_copy.h>
#include <fbl/alloc_checker.h>
#include <hypervisor/ktrace.h>
#include <kernel/koid.h>
#include <kernel/mp.h>
#include <ktl/atomic.h>
#include <ktl/iterator.h>
#include <lk/init.h>
#include <object/thread_dispatcher.h>
#include <vm/vm_aspace.h>

#include <ktl/enforce.h>

namespace {

using fxt::operator""_category;

struct CategoryEntry {
  uint32_t index;
  const fxt::InternedCategory& category;
};

const CategoryEntry kCategories[] = {
    {KTRACE_GRP_META_BIT, "kernel:meta"_category},
    {KTRACE_GRP_LIFECYCLE_BIT, "kernel:lifecycle"_category},
    {KTRACE_GRP_SCHEDULER_BIT, "kernel:sched"_category},
    {KTRACE_GRP_TASKS_BIT, "kernel:tasks"_category},
    {KTRACE_GRP_IPC_BIT, "kernel:ipc"_category},
    {KTRACE_GRP_IRQ_BIT, "kernel:irq"_category},
    {KTRACE_GRP_PROBE_BIT, "kernel:probe"_category},
    {KTRACE_GRP_ARCH_BIT, "kernel:arch"_category},
    {KTRACE_GRP_SYSCALL_BIT, "kernel:syscall"_category},
    {KTRACE_GRP_VM_BIT, "kernel:vm"_category},
    {KTRACE_GRP_RESTRICTED_BIT, "kernel:restricted"_category},
};

void SetupCategoryBits() {
  for (const CategoryEntry& entry : kCategories) {
    if (entry.category.index() == fxt::InternedCategory::kInvalidIndex) {
      entry.category.SetIndex(entry.index);
    } else {
      dprintf(INFO, "Found category \"%s\" already initialized to 0x%04x!\n",
              entry.category.string(), (1u << entry.category.index()));
    }
  }
  // If debug assertions are enabled, validate that all interned categories have been initialized.
  if constexpr (DEBUG_ASSERT_IMPLEMENTED) {
    for (const fxt::InternedCategory& category : fxt::InternedCategory::Iterate()) {
      DEBUG_ASSERT_MSG(category.index() != fxt::InternedCategory::kInvalidIndex,
                       "Interned category %s was not initialized\n", category.string());
    }
  }
}

void ktrace_report_cpu_pseudo_threads() {
  const uint max_cpus = arch_max_num_cpus();
  char name[32];
  for (uint i = 0; i < max_cpus; i++) {
    snprintf(name, sizeof(name), "cpu-%u", i);
    KTRACE_KERNEL_OBJECT_ALWAYS(KTrace::GetCpuKoid(i), ZX_OBJ_TYPE_THREAD, name,
                                ("process", KTrace::kNoProcess));
  }
}

}  // namespace

namespace internal {

KTraceState::~KTraceState() {
  if (buffer_ != nullptr) {
    VmAspace* aspace = VmAspace::kernel_aspace();
    aspace->FreeRegion(reinterpret_cast<vaddr_t>(buffer_));
  }
}

void KTraceState::Init(uint32_t target_bufsize, bool start_tracing) {
  Guard<Mutex> guard(&lock_);
  ASSERT_MSG(target_bufsize_ == 0,
             "Double init of KTraceState instance (tgt_bs %u, new tgt_bs %u)!", target_bufsize_,
             target_bufsize);
  ASSERT(is_started_ == false);

  // Allocations are rounded up to the nearest page size.
  target_bufsize_ = fbl::round_up(target_bufsize, static_cast<uint32_t>(PAGE_SIZE));

  if (!start_tracing) {
    // Writes should be disabled and there should be no in-flight writes.
    [[maybe_unused]] uint64_t observed;
    DEBUG_ASSERT_MSG((observed = write_state_.load(ktl::memory_order_acquire)) == 0, "0x%lx",
                     observed);
    return;
  }

  if (AllocBuffer() != ZX_OK) {
    return;
  }

  EnableWrites();
  // Now that writes have been enabled, we can report the names.
  ReportStaticNames();
  ReportThreadProcessNames();
  is_started_ = true;
}

zx_status_t KTraceState::Start(StartMode mode) {
  Guard<Mutex> guard(&lock_);

  if (zx_status_t status = AllocBuffer(); status != ZX_OK) {
    return status;
  }

  // If we are attempting to start in saturating mode, then check to be sure
  // that we were not previously operating in circular mode.  It is not legal to
  // re-start a ktrace buffer in saturating mode which had been operating in
  // circular mode.
  if (mode == StartMode::Saturate) {
    Guard<TraceDisabledSpinLock, IrqSave> write_guard{&write_lock_};
    if (circular_size_ != 0) {
      return ZX_ERR_BAD_STATE;
    }
  }

  // If we are not yet started, we need to report the current thread and process
  // names.
  if (!is_started_) {
    is_started_ = true;
    EnableWrites();
    ReportStaticNames();
    ReportThreadProcessNames();
  }

  // If we are changing from saturating mode, to circular mode, we need to
  // update our circular bookkeeping.
  {
    Guard<TraceDisabledSpinLock, IrqSave> write_guard{&write_lock_};
    if ((mode == StartMode::Circular) && (circular_size_ == 0)) {
      // Mark the point at which the static data ends and the circular
      // portion of the buffer starts (the "wrap offset").
      DEBUG_ASSERT(wr_ <= bufsize_);
      wrap_offset_ = static_cast<uint32_t>(ktl::min<uint64_t>(bufsize_, wr_));
      circular_size_ = bufsize_ - wrap_offset_;
      wr_ = 0;
    }
  }

  // It's possible that a |ReserveRaw| failure may have disabled writes so make
  // sure they are enabled.
  EnableWrites();
  return ZX_OK;
}

zx_status_t KTraceState::Stop() {
  Guard<Mutex> guard(&lock_);

  // Start by clearing the group mask and disabling writes.  This will prevent
  // new writers from starting write operations.  The non-write lock prevents
  // another thread from starting another trace session until we have finished
  // the stop operation.
  DisableWrites();

  // Now wait until any lingering write operations have finished.  This
  // should never take any significant amount of time.  If it does, we are
  // probably operating in a virtual environment with a host who is being
  // mean to us.
  zx_instant_mono_t absolute_timeout = current_mono_time() + ZX_SEC(1);
  bool stop_synced;
  do {
    stop_synced = inflight_writes() == 0;
    if (!stop_synced) {
      Thread::Current::SleepRelative(ZX_MSEC(1));
    }
  } while (!stop_synced && (current_mono_time() < absolute_timeout));

  if (!stop_synced) {
    return ZX_ERR_TIMED_OUT;
  }

  // Great, we are now officially stopped.  Record this.
  is_started_ = false;
  return ZX_OK;
}

zx_status_t KTraceState::RewindLocked() {
  if (is_started_) {
    return ZX_ERR_BAD_STATE;
  }

  // Writes should be disabled and there should be no in-flight writes.
  [[maybe_unused]] uint64_t observed;
  DEBUG_ASSERT_MSG((observed = write_state_.load(ktl::memory_order_acquire)) == 0, "0x%lx",
                   observed);

  {
    Guard<TraceDisabledSpinLock, IrqSave> write_guard{&write_lock_};
    // 1 magic bytes record: 8 bytes
    // 1 Initialization/tickrate record: 16 bytes
    const size_t fxt_metadata_size = 24;

    // roll back to just after the metadata
    rd_ = 0;
    wr_ = fxt_metadata_size;

    // After a rewind, we are no longer in circular buffer mode.
    wrap_offset_ = 0;
    circular_size_ = 0;

    // We cannot add metadata rewind if we have not allocated a buffer yet.
    if (buffer_ == nullptr) {
      wr_ = 0;
      return ZX_OK;
    }

    // Write our fxt metadata -- our magic number and timestamp resolution.
    uint64_t* recs = reinterpret_cast<uint64_t*>(buffer_);
    // FXT Magic bytes
    recs[0] = 0x0016547846040010;
    // FXT Initialization Record
    recs[1] = 0x21;
    recs[2] = ticks_per_second();
  }

  return ZX_OK;
}

ssize_t KTraceState::ReadUser(user_out_ptr<void> ptr, uint32_t off, size_t len) {
  Guard<Mutex> guard(&lock_);

  // If we were never configured to have a target buffer, our "docs" say that we
  // are supposed to return ZX_ERR_NOT_SUPPORTED.
  //
  // https://fuchsia.dev/fuchsia-src/reference/syscalls/ktrace_read
  if (!target_bufsize_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // We cannot read the buffer while it is in the started state.
  if (is_started_) {
    return ZX_ERR_BAD_STATE;
  }

  // If we are in the lock_, and we are stopped, then new writes must be
  // disabled, and we must have synchronized with any in-flight writes by now.
  [[maybe_unused]] uint64_t observed;
  DEBUG_ASSERT_MSG((observed = write_state_.load(ktl::memory_order_acquire)) == 0, "0x%lx",
                   observed);

  // Grab the write lock, then figure out what we need to copy.  We need to make
  // sure to drop the lock before calling CopyToUser (holding spinlocks while
  // copying to user mode memory is not allowed because of the possibility of
  // faulting).
  //
  // It may appear like a bad thing to drop the lock before performing the copy,
  // but it really should not be much of an issue.  The grpmask is disabled, so
  // no new writes are coming in, and the lock_ is blocking any other threads
  // which might be attempting command and control operations.
  //
  // The only potential place where another thread might contend on the write
  // lock here would be if someone was trying to add a name record to the trace
  // with the |always| flag set (ignoring the grpmask).  This should only ever
  // happen during rewind and start operations which are serialized by lock_.
  struct Region {
    uint8_t* ptr{nullptr};
    size_t len{0};
  };

  struct ToCopy {
    size_t avail{0};
    Region regions[3];
  } to_copy = [this, &off, &len]() -> ToCopy {
    Guard<TraceDisabledSpinLock, IrqSave> write_guard{&write_lock_};

    // The amount of data we have to exfiltrate is equal to the distance between
    // the read and the write pointers, plus the non-circular region of the buffer
    // (if we are in circular mode).
    const uint32_t avail = [this]() TA_REQ(write_lock_) -> uint32_t {
      if (circular_size_ == 0) {
        DEBUG_ASSERT(rd_ == 0);
        DEBUG_ASSERT(wr_ <= bufsize_);
        return static_cast<uint32_t>(wr_);
      } else {
        DEBUG_ASSERT(rd_ <= wr_);
        DEBUG_ASSERT((wr_ - rd_) <= circular_size_);
        return static_cast<uint32_t>(wr_ - rd_) + wrap_offset_;
      }
    }();

    // constrain read to available buffer
    if (off >= avail) {
      return ToCopy{};
    }

    len = ktl::min<size_t>(len, avail - off);

    ToCopy ret{.avail = avail};
    uint32_t ndx = 0;

    // Go ahead report the region(s) of data which need to be copied.
    if (circular_size_ == 0) {
      // Non-circular mode is simple.
      DEBUG_ASSERT(ndx < ktl::size(ret.regions));
      Region& r = ret.regions[ndx++];
      r.ptr = buffer_ + off;
      r.len = len;
    } else {
      // circular mode requires a bit more care.
      size_t remaining = len;

      // Start by consuming the non-circular portion of the buffer, taking into
      // account the offset.
      if (off < wrap_offset_) {
        const size_t todo = ktl::min<size_t>(wrap_offset_ - off, remaining);

        DEBUG_ASSERT(ndx < ktl::size(ret.regions));
        Region& r = ret.regions[ndx++];
        r.ptr = buffer_ + off;
        r.len = todo;

        remaining -= todo;
        off = 0;
      } else {
        off -= wrap_offset_;
      }

      // Now consume as much of the circular payload as we have space for.
      if (remaining) {
        const uint32_t rd_offset = PtrToCircularOffset(rd_ + off);
        DEBUG_ASSERT(rd_offset <= bufsize_);
        size_t todo = ktl::min<size_t>(bufsize_ - rd_offset, remaining);

        {
          DEBUG_ASSERT(ndx < ktl::size(ret.regions));
          Region& r = ret.regions[ndx++];
          r.ptr = buffer_ + rd_offset;
          r.len = todo;
        }

        remaining -= todo;

        if (remaining) {
          DEBUG_ASSERT(remaining <= (rd_offset - wrap_offset_));
          DEBUG_ASSERT(ndx < ktl::size(ret.regions));
          Region& r = ret.regions[ndx++];
          r.ptr = buffer_ + wrap_offset_;
          r.len = remaining;
        }
      }
    }

    return ret;
  }();

  // null read is a query for trace buffer size
  //
  // TODO(johngro):  What are we supposed to return here?  The total number of
  // available bytes, or the total number of bytes which would have been
  // available had we started reading from |off|?  Our "docs" say nothing about
  // this.  For now, I'm just going to maintain the existing behavior and return
  // all of the available bytes, but someday the defined behavior of this API
  // needs to be clearly specified.
  if (!ptr) {
    return to_copy.avail;
  }

  // constrain read to available buffer
  if (off >= to_copy.avail) {
    return 0;
  }

  // Go ahead and copy the data.
  auto ptr8 = ptr.reinterpret<uint8_t>();
  size_t done = 0;
  for (const auto& r : to_copy.regions) {
    if (r.ptr != nullptr) {
      zx_status_t copy_result = ZX_OK;
      // Performing user copies whilst holding locks is not generally allowed, however in this case
      // the entire purpose of lock_ is to serialize these operations and so is safe to be held for
      // this copy.
      //
      // TOOD(https://fxbug.dev/42052646): Determine if this should be changed to capture faults and
      // resolve them outside the lock.
      guard.CallUntracked([&] { copy_result = CopyToUser(ptr8.byte_offset(done), r.ptr, r.len); });
      if (copy_result != ZX_OK) {
        return ZX_ERR_INVALID_ARGS;
      }
    }

    done += r.len;
  }

  // Success!
  return done;
}

void KTraceState::ReportStaticNames() {
  fxt::InternedString::RegisterStrings();
  ktrace_report_cpu_pseudo_threads();
}

void KTraceState::ReportThreadProcessNames() {
  ktrace_report_live_processes();
  ktrace_report_live_threads();
}

zx_status_t KTraceState::AllocBuffer() {
  // The buffer is allocated once, then never deleted.  If it has already been
  // allocated, then we are done.
  {
    Guard<TraceDisabledSpinLock, IrqSave> write_guard{&write_lock_};
    if (buffer_) {
      return ZX_OK;
    }
  }

  // We require that our buffer be a multiple of page size, and non-zero.  If
  // the target buffer size ends up being zero, it is most likely because boot
  // args set the buffer size to zero.  For now, report NOT_SUPPORTED up the
  // stack to signal to usermode tracing (hitting AllocBuffer via Start) that
  // ktracing has been disabled.
  //
  // TODO(johngro): Do this rounding in Init
  target_bufsize_ = static_cast<uint32_t>(target_bufsize_ & ~(PAGE_SIZE - 1));
  if (!target_bufsize_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  DEBUG_ASSERT(is_started_ == false);

  zx_status_t status;
  VmAspace* aspace = VmAspace::kernel_aspace();
  void* ptr;
  if ((status = aspace->Alloc("ktrace", target_bufsize_, &ptr, 0, VmAspace::VMM_FLAG_COMMIT,
                              ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE)) < 0) {
    DiagsPrintf(INFO, "ktrace: cannot alloc buffer %d\n", status);
    return ZX_ERR_NO_MEMORY;
  }

  {
    Guard<TraceDisabledSpinLock, IrqSave> write_guard{&write_lock_};
    buffer_ = static_cast<uint8_t*>(ptr);
    bufsize_ = target_bufsize_;
  }
  DiagsPrintf(INFO, "ktrace: buffer at %p (%u bytes)\n", ptr, target_bufsize_);

  // Rewind will take care of writing the metadata records as it resets the state.
  [[maybe_unused]] zx_status_t rewind_res = RewindLocked();
  DEBUG_ASSERT(rewind_res == ZX_OK);

  return ZX_OK;
}

uint64_t* KTraceState::ReserveRaw(uint32_t num_words) {
  // At least one word must be reserved to store the trace record header.
  DEBUG_ASSERT(num_words >= 1);

  const uint32_t num_bytes = num_words * sizeof(uint64_t);

  constexpr uint64_t kUncommitedRecordTag = 0;
  auto Commit = [](uint64_t* ptr, uint64_t tag) -> void {
    ktl::atomic_ref(*ptr).store(tag, ktl::memory_order_release);
  };

  Guard<TraceDisabledSpinLock, IrqSave> write_guard{&write_lock_};
  if (!bufsize_) {
    return nullptr;
  }

  if (circular_size_ == 0) {
    DEBUG_ASSERT(bufsize_ >= wr_);
    const size_t space = bufsize_ - wr_;

    // if there is not enough space, we are done.
    if (space < num_bytes) {
      return nullptr;
    }

    // We have the space for this record.  Stash the tag with a sentinel value
    // of zero, indicating that there is a reservation here, but that the record
    // payload has not been fully committed yet.
    uint64_t* ptr = reinterpret_cast<uint64_t*>(buffer_ + wr_);
    Commit(ptr, kUncommitedRecordTag);
    wr_ += num_bytes;
    return ptr;
  } else {
    // If there is not enough space in this circular buffer to hold our message,
    // don't even try.  Just give up.
    if (num_bytes > circular_size_) {
      return nullptr;
    }

    while (true) {
      // Start by figuring out how much space we want to reserve.  Typically, we
      // will just reserve the space we need for our record. If, however, the
      // space at the end of the circular buffer is not enough to contiguously
      // hold our record, we reserve that amount of space instead, so that we
      // can put in a placeholder record at the end of the buffer which will be
      // skipped, in addition to our actual record.
      const uint32_t wr_offset = PtrToCircularOffset(wr_);
      const uint32_t contiguous_space = bufsize_ - wr_offset;
      const uint32_t to_reserve = ktl::min(contiguous_space, num_bytes);
      DEBUG_ASSERT((to_reserve > 0) && ((to_reserve & 0x7) == 0));

      // Do we have the space for our reservation?  If not, then
      // move the read pointer forward until we do.
      DEBUG_ASSERT((wr_ >= rd_) && ((wr_ - rd_) <= circular_size_));
      size_t avail = circular_size_ - (wr_ - rd_);
      while (avail < to_reserve) {
        // We have to have space for a header tag.
        const uint32_t rd_offset = PtrToCircularOffset(rd_);
        DEBUG_ASSERT(bufsize_ - rd_offset >= sizeof(uint64_t));

        // Make sure that we read the next tag in the sequence with acquire
        // semantics.  Before committing, records which have been reserved in
        // the trace buffer will have their tag set to zero inside of the write
        // lock. During commit, however, the actual record tag (with non-zero
        // length) will be written to memory atomically with release semantics,
        // outside of the lock.
        uint64_t* rd_tag_ptr = reinterpret_cast<uint64_t*>(buffer_ + rd_offset);
        const uint64_t rd_tag =
            ktl::atomic_ref<uint64_t>(*rd_tag_ptr).load(ktl::memory_order_acquire);
        const uint32_t sz = fxt::RecordFields::RecordSize::Get<uint32_t>(rd_tag) * 8;

        // If our size is 0, it implies that we managed to wrap around and catch
        // the read pointer when it is pointing to a still uncommitted
        // record.  We are not in a position where we can wait.  Simply fail the
        // reservation.
        if (sz == 0) {
          return nullptr;
        }

        // Now go ahead and move read up.
        rd_ += sz;
        avail += sz;
      }

      // Great, we now have space for our reservation.  If we have enough space
      // for our entire record, go ahead and reserve the space now.  Otherwise,
      // stuff in a placeholder which fills all of the remaining contiguous
      // space in the buffer, then try the allocation again.
      uint64_t* ptr = reinterpret_cast<uint64_t*>(buffer_ + wr_offset);
      wr_ += to_reserve;
      if (num_bytes == to_reserve) {
        Commit(ptr, kUncommitedRecordTag);
        return ptr;
      } else {
        DEBUG_ASSERT(num_bytes > to_reserve);
        Commit(ptr, fxt::RecordFields::RecordSize::Make((to_reserve + 7) / 8));
      }
    }
  }
}

}  // namespace internal

//
// Implement the single buffer specializations for KTraceImpl.
//

template <>
void KTraceImpl<BufferMode::kSingle>::Init(uint32_t bufsize, uint32_t initial_grpmask) {
  KTraceGuard guard{&lock_};

  cpu_context_map_.Init();
  internal_state_.Init(bufsize, initial_grpmask);
  set_categories_bitmask(initial_grpmask);
}

template <>
zx_status_t KTraceImpl<BufferMode::kSingle>::Start(uint32_t action, uint32_t categories) {
  if (categories == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  using StartMode = ::internal::KTraceState::StartMode;
  const StartMode start_mode =
      (action == KTRACE_ACTION_START) ? StartMode::Saturate : StartMode::Circular;

  const zx_status_t status = internal_state_.Start(start_mode);
  if (status != ZX_OK) {
    return status;
  }
  // We need to set the categories bitmask after calling internal_state_.Start because that
  // method emits a bunch of static metadata that should not be interspersed with arbitrary
  // trace records.
  set_categories_bitmask(categories);

  DiagsPrintf(INFO, "Enabled category mask: 0x%03x\n", categories);
  DiagsPrintf(INFO, "Trace category states:\n");
  for (const fxt::InternedCategory& category : fxt::InternedCategory::Iterate()) {
    DiagsPrintf(INFO, "  %-20s : 0x%03x : %s\n", category.string(), (1u << category.index()),
                KTrace::CategoryEnabled(category) ? "enabled" : "disabled");
  }

  return ZX_OK;
}

template <>
zx_status_t KTraceImpl<BufferMode::kSingle>::Stop() {
  set_categories_bitmask(0u);
  return internal_state_.Stop();
}

template <>
zx_status_t KTraceImpl<BufferMode::kSingle>::Rewind() {
  return internal_state_.Rewind();
}

template <>
zx::result<internal::KTraceState::PendingCommit> KTraceImpl<BufferMode::kSingle>::Reserve(
    uint64_t header) {
  return internal_state_.Reserve(header);
}

template <>
zx::result<size_t> KTraceImpl<BufferMode::kSingle>::ReadUser(user_out_ptr<void> ptr, uint32_t off,
                                                             size_t len) {
  const ssize_t ret = internal_state_.ReadUser(ptr, off, len);
  if (ret < 0) {
    return zx::error(static_cast<zx_status_t>(ret));
  }
  return zx::ok(ret);
}

template <>
ktl::byte* KTraceImpl<BufferMode::kSingle>::KernelAspaceAllocator::Allocate(uint32_t size) {
  return nullptr;
}

template <>
void KTraceImpl<BufferMode::kSingle>::KernelAspaceAllocator::Free(ktl::byte* ptr) {}

//
// TODO(https://fxbug.dev/404539312): Implement the per-CPU buffer specializations for KTraceImpl.
//

template <>
ktl::byte* KTraceImpl<BufferMode::kPerCpu>::KernelAspaceAllocator::Allocate(uint32_t size) {
  VmAspace* kaspace = VmAspace::kernel_aspace();
  char name[32] = "ktrace-percpu-buffer";
  void* ptr;
  const zx_status_t status = kaspace->Alloc(name, size, &ptr, 0, VmAspace::VMM_FLAG_COMMIT,
                                            ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE);
  if (status != ZX_OK) {
    return nullptr;
  }
  return static_cast<ktl::byte*>(ptr);
}

template <>
void KTraceImpl<BufferMode::kPerCpu>::KernelAspaceAllocator::Free(ktl::byte* ptr) {
  if (ptr != nullptr) {
    VmAspace* kaspace = VmAspace::kernel_aspace();
    kaspace->FreeRegion(reinterpret_cast<vaddr_t>(ptr));
  }
}

template <>
void KTraceImpl<BufferMode::kPerCpu>::DisableWrites() {
  // Start off by disabling writes.
  // It may be possible to do this with relaxed semantics, but we do it with release semantics
  // out of an abundance of caution.
  writes_enabled_.store(false, ktl::memory_order_release);

  // Wait for any in-progress writes to complete.
  // We accomplish this by:
  // 1. Disabling interrupts on this core, thus preventing this thread from being migrated across
  //    CPUs. We could alternatively just disable preemption, but mp_sync_exec requires interrupts
  //    to be disabled when using MP_IPI_TARGET_ALL_BUT_LOCAL.
  // 2. Sending an IPI that runs a no-op to all other cores. Since writes run with interrupts
  //    disabled, the mere fact that a core is able to process an IPI means that it is not
  //    currently performing a trace record write. Additionally, mp_sync_exec issues a memory
  //    barrier that ensures that every other core will see that writes are disabled after
  //    processing the IPI.
  InterruptDisableGuard irq_guard;
  auto wait_for_write_completion = [](void*) {};
  mp_sync_exec(MP_IPI_TARGET_ALL_BUT_LOCAL, 0, wait_for_write_completion, nullptr);
}

template <>
zx_status_t KTraceImpl<BufferMode::kPerCpu>::Allocate() {
  if (percpu_buffers_) {
    return ZX_OK;
  }

  // The number of buffers to initialize and their size should be set before this method is called.
  DEBUG_ASSERT(num_buffers_ != 0);
  DEBUG_ASSERT(buffer_size_ != 0);

  // Allocate the per-CPU SPSC buffer data structures.
  // Initially, store the unique pointer in a local variable. This will ensure that the buffers are
  // destructed upon initialization below.
  fbl::AllocChecker ac;
  ktl::unique_ptr buffers = ktl::make_unique<PerCpuBuffer[]>(&ac, num_buffers_);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  // Initialize each per-CPU buffer by allocating the storage used to back it.
  for (uint32_t i = 0; i < num_buffers_; i++) {
    const zx_status_t status = buffers[i].Init(buffer_size_);
    if (status != ZX_OK) {
      // Any allocated buffers will be destructed when we return.
      DiagsPrintf(INFO, "ktrace: cannot alloc buffer %u: %d\n", i, status);
      return ZX_ERR_NO_MEMORY;
    }
  }

  // Take ownership of the newly created per-CPU buffers.
  percpu_buffers_ = ktl::move(buffers);
  return ZX_OK;
}

template <>
zx_status_t KTraceImpl<BufferMode::kPerCpu>::Start(uint32_t, uint32_t categories) {
  // Allocate the buffers. This will be a no-op if the buffers are already initialized.
  if (zx_status_t status = Allocate(); status != ZX_OK) {
    return status;
  }

  // If writes are already enabled, then a trace session is already in progress and all we need to
  // do is set the categories bitmask and return.
  if (WritesEnabled()) {
    set_categories_bitmask(categories);
    return ZX_OK;
  }

  // Otherwise, enable writes and report static metadata before setting the categories bitmask.
  // The metadata needs to be emitted before we enable arbitrary categories, otherwise the thread
  // and process records will be interspersed with generic trace records.
  EnableWrites();
  ReportMetadata();

  set_categories_bitmask(categories);
  DiagsPrintf(INFO, "Enabled category mask: 0x%03x\n", categories);
  DiagsPrintf(INFO, "Trace category states:\n");
  for (const fxt::InternedCategory& category : fxt::InternedCategory::Iterate()) {
    DiagsPrintf(INFO, "  %-20s : 0x%03x : %s\n", category.string(), (1u << category.index()),
                IsCategoryEnabled(category) ? "enabled" : "disabled");
  }

  return ZX_OK;
}

template <>
void KTraceImpl<BufferMode::kPerCpu>::Init(uint32_t bufsize, uint32_t initial_grpmask) {
  KTraceGuard guard{&lock_};

  ASSERT_MSG(buffer_size_ == 0, "KTrace::Init called twice");
  // Allocate the KOIDs used to annotate CPU trace records.
  cpu_context_map_.Init();

  // Compute the per-CPU buffer size, ensuring that the resulting value is page aligned.
  num_buffers_ = arch_max_num_cpus();
  buffer_size_ = PAGE_ALIGN(bufsize / num_buffers_);

  // If the initial_grpmask was zero, then we can delay allocation of the KTrace buffer.
  if (initial_grpmask == 0) {
    return;
  }
  // Otherwise, begin tracing immediately.
  Start(KTRACE_ACTION_START, initial_grpmask);
}

template <>
zx_status_t KTraceImpl<BufferMode::kPerCpu>::Stop() {
  set_categories_bitmask(0u);
  DisableWrites();
  return ZX_OK;
}

template <>
zx_status_t KTraceImpl<BufferMode::kPerCpu>::Rewind() {
  // Calling Rewind on an uninitialized KTrace buffer is a no-op.
  if (!percpu_buffers_) {
    return ZX_OK;
  }
  // Rewind calls Drain on each per-CPU buffer. As mentioned in the doc comments of that method,
  // it is invalid to call Drain concurrently with a Read, and the method is only guaranteed to
  // fully empty the buffer if there are no concurrent Write operations. We ensure that these
  // prerequisites are met by:
  // 1. Holding the lock_, ensuring that there can be no other readers.
  // 2. Ensuring writes are disabled to prevent any future writes from starting.
  // 3. Performing the Drain within an IPI on each core, ensuring that this operation does not
  //    race with any in-progress writes.
  writes_enabled_.store(false, ktl::memory_order_release);

  auto run_drain = [](void* arg) {
    const cpu_num_t curr_cpu = arch_curr_cpu_num();
    PerCpuBuffer* percpu_buffers = static_cast<PerCpuBuffer*>(arg);
    PerCpuBuffer& curr_cpu_buffer = percpu_buffers[curr_cpu];
    curr_cpu_buffer.Drain();

    // If this is not running on the boot CPU, we're done.
    if (curr_cpu != BOOT_CPU_ID) {
      return;
    }

    // FxtMetadata is a type that contains the metadata records that are expected to be at the
    // beginning of every kernel trace. We place one of these structures in the boot CPU's buffer.
    struct FxtMetadata {
      // Magic Record
      // https://fuchsia.dev/fuchsia-src/reference/tracing/trace-format#magic-number-record
      uint64_t magic;
      // Initialization Record
      // https://fuchsia.dev/fuchsia-src/reference/tracing/trace-format#initialization-record
      uint64_t init_record_header;
      zx_ticks_t ticks_per_second;
    };
    const FxtMetadata fxt_metadata = {
        .magic = 0x0016547846040010,
        .init_record_header = 0x21,
        .ticks_per_second = ticks_per_second(),
    };

    // Reserve space for the metadata record.
    // We just drained this buffer, so this reservation cannot fail.
    zx::result<PerCpuBuffer::Reservation> res = curr_cpu_buffer.Reserve(sizeof(FxtMetadata));
    DEBUG_ASSERT(res.is_ok());
    res->Write(ktl::span<const ktl::byte>(reinterpret_cast<const ktl::byte*>(&fxt_metadata),
                                          sizeof(fxt_metadata)));
    res->Commit();
  };
  mp_sync_exec(MP_IPI_TARGET_ALL, 0, run_drain, percpu_buffers_.get());
  return ZX_OK;
}

template <>
zx::result<KTraceImpl<BufferMode::kPerCpu>::PendingCommit> KTraceImpl<BufferMode::kPerCpu>::Reserve(
    uint64_t header) {
  // Compute the number of bytes we need to reserve from the provided fxt header.
  const uint32_t num_words = fxt::RecordFields::RecordSize::Get<uint32_t>(header);
  const uint32_t num_bytes = num_words * sizeof(uint64_t);

  // Disable interrupts.
  // We have to do this before we check if writes are enabled, otherwise a racing Stop operation
  // may disable writes and send the IPI it uses to verify that we're done writing before we begin
  // our write.
  // We also have to do this before we check which CPU this thread is on, otherwise this thread
  // could be migrated across CPUs after we check the current CPU number, leading to an invalid,
  // cross-CPU write.
  interrupt_saved_state_t saved_state = arch_interrupt_save();
  auto restore_interrupt_state =
      fit::defer([&saved_state]() { arch_interrupt_restore(saved_state); });

  // If writes are disabled, then return an error. We return ZX_ERR_BAD_STATE, because this means
  // that tracing was disabled.
  //
  // It is valid for writes to be disabled immediately after this check. This is ok because Stop,
  // which disables writes, will follow up with an IPI to all cores and wait for those IPIs to
  // return. Because we disabled interrupts prior to this check, that IPI will not return until
  // this write operation is complete.
  if (!WritesEnabled()) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  // Check which CPU we're running on and Reserve a slot in the appropriate SPSC buffer.
  const cpu_num_t cpu_num = arch_curr_cpu_num();
  zx::result<PerCpuBuffer::Reservation> result = percpu_buffers_[cpu_num].Reserve(num_bytes);
  if (result.is_error()) {
    return result.take_error();
  }
  PendingCommit res(ktl::move(result.value()), saved_state, header);

  // The PendingWrite is now responsible for restoring interrupt state.
  restore_interrupt_state.cancel();

  return zx::ok(ktl::move(res));
}

template <>
zx::result<size_t> KTraceImpl<BufferMode::kPerCpu>::ReadUser(user_out_ptr<void> ptr,
                                                             uint32_t offset, size_t len) {
  // Reads must be serialized with respect to all other non-write operations.
  KTraceGuard guard{&lock_};

  // If the per-CPU buffers have not been initialized, there's nothing to do, so return early.
  if (!percpu_buffers_) {
    return zx::ok(0);
  }

  // Eventually, this should support users passing in buffers smaller than the sum of the size of
  // all per-CPU buffers, but for now we do not allow this.
  if (len < (buffer_size_ * num_buffers_)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Iterate through each per-CPU buffer and read its contents.
  size_t bytes_read = 0;
  user_out_ptr<ktl::byte> byte_ptr = ptr.reinterpret<ktl::byte>();
  for (uint32_t i = 0; i < num_buffers_; i++) {
    auto copy_fn = [&](uint32_t offset, ktl::span<ktl::byte> src) {
      // This is safe to do while holding the lock_ because the KTrace lock is a leaf lock that is
      // not acquired during the course of a page fault.
      zx_status_t status = ZX_ERR_BAD_STATE;
      guard.CallUntracked([&]() {
        status =
            byte_ptr.byte_offset(bytes_read + offset).copy_array_to_user(src.data(), src.size());
      });
      return status;
    };
    const zx::result<size_t> result = percpu_buffers_[i].Read(copy_fn, static_cast<uint32_t>(len));
    if (result.is_error()) {
      DiagsPrintf(INFO, "failed to copy out ktrace data: %d\n", result.status_value());
      // If we copied some data from a previous buffer, we have to return the fact that we did so
      // here. Otherwise, that data will be lost.
      if (bytes_read != 0) {
        return zx::ok(bytes_read);
      }
      // Otherwise, return the error.
      return zx::error(result.status_value());
    }
    bytes_read += result.value();
  }
  return zx::ok(bytes_read);
}

// The InitHook is the same for both the single and per-CPU buffer implementation of KTrace.
template <BufferMode Mode>
void KTraceImpl<Mode>::InitHook(unsigned) {
  const uint32_t bufsize = gBootOptions->ktrace_bufsize << 20;
  const uint32_t initial_grpmask = gBootOptions->ktrace_grpmask;

  dprintf(INFO, "ktrace_init: bufsize=%u grpmask=%x\n", bufsize, initial_grpmask);

  if (!bufsize) {
    dprintf(INFO, "ktrace: disabled\n");
    return;
  }

  // Coerce the category ids to match the pre-defined bit mappings of aged ktrace interface.
  // TODO(eieio): Remove this when kernel migrates to IOB-based tracing with extensible categories.
  SetupCategoryBits();

  dprintf(INFO, "Trace categories: \n");
  for (const fxt::InternedCategory& category : fxt::InternedCategory::Iterate()) {
    dprintf(INFO, "  %-20s : 0x%03x\n", category.string(), (1u << category.index()));
  }

  if (!initial_grpmask) {
    dprintf(INFO, "ktrace: delaying buffer allocation\n");
  }

  // Set the callback to emit fxt string records for the set of interned strings when
  // fxt::InternedString::RegisterStrings() is called at the beginning of a trace session.
  // TODO(eieio): Replace this with id allocator allocations when IOB-based tracing is implemented.
  fxt::InternedString::SetRegisterCallback([](const fxt::InternedString& interned_string) {
    fxt::WriteStringRecord(
        &GetInstance(), interned_string.id(), interned_string.string(),
        strnlen(interned_string.string(), fxt::InternedString::kMaxStringLength));
  });

  // Initialize the singleton data structures.
  GetInstance().Init(bufsize, initial_grpmask);
}

// Finish initialization before starting userspace (i.e. before debug syscalls can occur).
LK_INIT_HOOK(ktrace, KTrace::InitHook, LK_INIT_LEVEL_USER - 1)
