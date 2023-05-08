// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <debug.h>
#include <lib/boot-options/boot-options.h>
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
#include <ktl/atomic.h>
#include <ktl/iterator.h>
#include <lk/init.h>
#include <object/thread_dispatcher.h>
#include <vm/vm_aspace.h>

#include <ktl/enforce.h>

// The global ktrace state.
internal::KTraceState KTRACE_STATE;

namespace {

using fxt::operator""_category;

struct CategoryEntry {
  uint32_t bit_number;
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
};

void SetupCategoryBits() {
  for (const CategoryEntry& entry : kCategories) {
    const uint32_t bit_number = entry.category.bit_number;
    if (bit_number == fxt::InternedCategory::kInvalidBitNumber) {
      entry.category.bit_number = entry.bit_number;
      fxt::InternedCategory::RegisterInitialized(entry.category);
    } else {
      dprintf(INFO, "Found category \"%s\" already initialized to 0x%04x!\n",
              entry.category.string(), bit_number);
    }
  }
}

const fxt::InternedString* ktrace_find_probe(const char* name) {
  for (const fxt::InternedString& interned_string : fxt::InternedString::IterateList) {
    if (!strcmp(name, interned_string.string)) {
      return &interned_string;
    }
  }
  return nullptr;
}

void ktrace_add_probe(const fxt::InternedString& interned_string) { interned_string.GetId(); }

void ktrace_report_probes() {
  for (const fxt::InternedString& interned_string : fxt::InternedString::IterateList) {
    fxt_string_record(interned_string.id, interned_string.string,
                      strnlen(interned_string.string, fxt::InternedString::kMaxStringLength));
  }
}

// TODO(fxbug.dev/112751)
void ktrace_report_cpu_pseudo_threads() {
  const uint max_cpus = arch_max_num_cpus();
  char name[32];
  for (uint i = 0; i < max_cpus; i++) {
    snprintf(name, sizeof(name), "cpu-%u", i);
    KTRACE_KERNEL_OBJECT_ALWAYS(ktrace::CpuContextMap::GetCpuKoid(i), ZX_OBJ_TYPE_THREAD, name,
                                ("process", kNoProcess));
  }
}

}  // namespace

namespace ktrace {

zx_koid_t CpuContextMap::cpu_koid_base_{ZX_KOID_INVALID};

void CpuContextMap::Init() {
  if (cpu_koid_base_ == ZX_KOID_INVALID) {
    cpu_koid_base_ = KernelObjectId::GenerateRange(arch_max_num_cpus());
  }
}

}  // namespace ktrace

namespace internal {

KTraceState::~KTraceState() {
  if (buffer_ != nullptr) {
    VmAspace* aspace = VmAspace::kernel_aspace();
    aspace->FreeRegion(reinterpret_cast<vaddr_t>(buffer_));
  }
}

void KTraceState::Init(uint32_t target_bufsize, uint32_t initial_groups) {
  Guard<Mutex> guard(&lock_);
  ASSERT_MSG(target_bufsize_ == 0,
             "Double init of KTraceState instance (tgt_bs %u, new tgt_bs %u)!", target_bufsize_,
             target_bufsize);
  ASSERT(is_started_ == false);

  // Allocations are rounded up to the nearest page size.
  target_bufsize_ = fbl::round_up(target_bufsize, static_cast<uint32_t>(PAGE_SIZE));

  if (initial_groups == 0) {
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
  // Finally, set the group mask to allow non-name writes to proceed.
  SetGroupMask(initial_groups);
  is_started_ = true;
}

zx_status_t KTraceState::Start(uint32_t groups, StartMode mode) {
  Guard<Mutex> guard(&lock_);

  if (groups == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (zx_status_t status = AllocBuffer(); status != ZX_OK) {
    return status;
  }

  // If we are attempting to start in saturating mode, then check to be sure
  // that we were not previously operating in circular mode.  It is not legal to
  // re-start a ktrace buffer in saturating mode which had been operating in
  // circular mode.
  if (mode == StartMode::Saturate) {
    Guard<SpinLock, IrqSave> write_guard{&write_lock_};
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
    Guard<SpinLock, IrqSave> write_guard{&write_lock_};
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
  SetGroupMask(groups);

  DiagsPrintf(INFO, "Enabled category mask: 0x%03x\n", groups);
  DiagsPrintf(INFO, "Trace category states:\n");
  for (const fxt::InternedCategory& category : fxt::InternedCategory::IterateList) {
    DiagsPrintf(INFO, "  %-20s : 0x%03x : %s\n", category.string(), (1u << category.bit_number),
                ktrace_category_enabled(category) ? "enabled" : "disabled");
  }

  return ZX_OK;
}

zx_status_t KTraceState::Stop() {
  Guard<Mutex> guard(&lock_);

  // Start by clearing the group mask and disabling writes.  This will prevent
  // new writers from starting write operations.  The non-write lock prevents
  // another thread from starting another trace session until we have finished
  // the stop operation.
  ClearMaskDisableWrites();

  // Now wait until any lingering write operations have finished.  This
  // should never take any significant amount of time.  If it does, we are
  // probably operating in a virtual environment with a host who is being
  // mean to us.
  zx_time_t absolute_timeout = current_time() + ZX_SEC(1);
  bool stop_synced;
  do {
    stop_synced = inflight_writes() == 0;
    if (!stop_synced) {
      Thread::Current::SleepRelative(ZX_MSEC(1));
    }
  } while (!stop_synced && (current_time() < absolute_timeout));

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
    Guard<SpinLock, IrqSave> write_guard{&write_lock_};
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
    Guard<SpinLock, IrqSave> write_guard{&write_lock_};

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
      // TOOD(fxb/101783): Determine if this should be changed to capture faults and resolve them
      // outside the lock.
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
  ktrace_report_probes();
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
    Guard<SpinLock, IrqSave> write_guard{&write_lock_};
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
    Guard<SpinLock, IrqSave> write_guard{&write_lock_};
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

  Guard<SpinLock, IrqSave> write_guard{&write_lock_};
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

zx_status_t ktrace_control(uint32_t action, uint32_t options, void* ptr) {
  using StartMode = ::internal::KTraceState::StartMode;
  switch (action) {
    case KTRACE_ACTION_START:
    case KTRACE_ACTION_START_CIRCULAR: {
      const StartMode start_mode =
          (action == KTRACE_ACTION_START) ? StartMode::Saturate : StartMode::Circular;
      return KTRACE_STATE.Start(options ? options : KTRACE_GRP_ALL, start_mode);
    }

    case KTRACE_ACTION_STOP:
      return KTRACE_STATE.Stop();

    case KTRACE_ACTION_REWIND:
      return KTRACE_STATE.Rewind();

    case KTRACE_ACTION_NEW_PROBE: {
      const char* const string_in = static_cast<const char*>(ptr);

      const fxt::InternedString* ref = ktrace_find_probe(string_in);
      if (ref != nullptr) {
        return ref->id;
      }

      struct DynamicStringRef {
        explicit DynamicStringRef(const char* string) { memcpy(storage, string, sizeof(storage)); }

        char storage[ZX_MAX_NAME_LEN];
        fxt::InternedString string_ref{storage};
      };

      // TODO(eieio,dje): Figure out how to constrain this to prevent abuse by
      // creating huge numbers of unique probes.
      fbl::AllocChecker alloc_checker;
      DynamicStringRef* dynamic_ref = new (&alloc_checker) DynamicStringRef{string_in};
      if (!alloc_checker.check()) {
        return ZX_ERR_NO_MEMORY;
      }

      ktrace_add_probe(dynamic_ref->string_ref);
      return dynamic_ref->string_ref.id;
    }

    default:
      return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

static void ktrace_init(unsigned level) {
  // There's no utility in setting up the singleton ktrace instance if there are
  // no syscalls to access it. See zircon/kernel/syscalls/debug.cc for the
  // corresponding syscalls. Note that because KTRACE_STATE grpmask starts at 0
  // and will not be changed, the other functions in this file need not check
  // for enabled-ness manually.
  const bool syscalls_enabled = gBootOptions->enable_debugging_syscalls;
  const uint32_t bufsize = syscalls_enabled ? (gBootOptions->ktrace_bufsize << 20) : 0;
  const uint32_t initial_grpmask = gBootOptions->ktrace_grpmask;

  dprintf(INFO, "ktrace_init: syscalls_enabled=%d bufsize=%u grpmask=%x\n", syscalls_enabled,
          bufsize, initial_grpmask);

  if (!bufsize) {
    dprintf(INFO, "ktrace: disabled\n");
    return;
  }

  // Allocate koids for each CPU.
  ktrace::CpuContextMap::Init();

  fxt::InternedString::SetMapStringCallback(fxt_string_record);
  fxt::InternedString::PreRegister();

  SetupCategoryBits();
  fxt::InternedCategory::PreRegister();

  dprintf(INFO, "Trace categories: \n");
  for (const fxt::InternedCategory& category : fxt::InternedCategory::IterateList) {
    dprintf(INFO, "  %-20s : 0x%03x\n", category.string(), (1u << category.bit_number));
  }

  KTRACE_STATE.Init(bufsize, initial_grpmask);

  if (!initial_grpmask) {
    dprintf(INFO, "ktrace: delaying buffer allocation\n");
  }
}

// Finish initialization before starting userspace (i.e. before debug syscalls can occur).
LK_INIT_HOOK(ktrace, ktrace_init, LK_INIT_LEVEL_USER - 1)
