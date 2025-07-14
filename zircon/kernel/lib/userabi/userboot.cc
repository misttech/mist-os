// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/boot-options/boot-options.h>
#include <lib/console.h>
#include <lib/counters.h>
#include <lib/crashlog.h>
#include <lib/elfldltl/machine.h>
#include <lib/instrumentation/vmo.h>
#include <lib/userabi/userboot.h>
#include <lib/userabi/userboot_internal.h>
#include <lib/userabi/vdso.h>
#include <lib/zircon-internal/default_stack_size.h>
#include <platform.h>
#include <stdio.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <ktl/bit.h>
#include <ktl/source_location.h>
#include <ktl/utility.h>
#include <lk/init.h>
#include <object/channel_dispatcher.h>
#include <object/handle.h>
#include <object/job_dispatcher.h>
#include <object/message_packet.h>
#include <object/process_dispatcher.h>
#include <object/thread_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <phys/handoff.h>
#include <platform/crashlog.h>
#include <vm/vm_object_paged.h>

#if ENABLE_ENTROPY_COLLECTOR_TEST
#include <lib/crypto/entropy/quality_test.h>
#endif

#include "elf.h"

#include <ktl/enforce.h>

#define RETURN_IF_NOT_OK(expr, ...) \
  do {                              \
    zx_status_t eval_expr = (expr); \
    if (eval_expr != ZX_OK) {       \
      KernelOops(#expr, eval_expr); \
      return __VA_ARGS__;           \
    }                               \
  } while (0)

#define RETURN_IF_NOT(predicate, ...) \
  do {                                \
    if (!(predicate)) {               \
      KernelOops(#predicate, false);  \
      return __VA_ARGS__;             \
    }                                 \
  } while (0)

namespace {

template <typename T>
void KernelOops(const char* expression, T actual,
                ktl::source_location where = ktl::source_location::current()) {
  KERNEL_OOPS("[%s:%u] Expectation failure (%s was %d).\n", where.file_name(), where.line(),
              expression, actual);
}

class VmoBuffer {
 public:
  explicit VmoBuffer(fbl::RefPtr<VmObjectPaged> vmo) : size_(vmo->size()), vmo_(ktl::move(vmo)) {}

  explicit VmoBuffer(size_t size = PAGE_SIZE, uint32_t options = VmObjectPaged::kResizable) {
    zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, options, size, &vmo_);
    ZX_ASSERT(status == ZX_OK);
    size_ = vmo_->size();
  }

  int Write(const ktl::string_view str) {
    // Enlarge the VMO as needed if it was created as resizable.
    if (str.size() > size_ - offset_ && vmo_->is_resizable()) {
      DEBUG_ASSERT(vmo_->size() < offset_ + str.size());
      size_t minimum_size = offset_ + str.size();
      size_t page_aligned_size = fbl::round_up(minimum_size, size_t{PAGE_SIZE});
      zx_status_t status = vmo_->Resize(page_aligned_size);
      if (status == ZX_OK) {
        // Update unlocked cache of the size.
        size_ = vmo_->size();
      } else if (offset_ == size_) {
        // None left to write without the resize.
        // Otherwise proceed to write what can be written.
        return static_cast<int>(status);
      }
    }

    const size_t todo = ktl::min(str.size(), size_ - offset_);

    if (zx_status_t res = vmo_->Write(str.data(), offset_, todo); res != ZX_OK) {
      DEBUG_ASSERT(static_cast<int>(res) < 0);
      return res;
    }

    offset_ += todo;
    DEBUG_ASSERT(todo <= ktl::numeric_limits<int>::max());
    return static_cast<int>(todo);
  }

  const fbl::RefPtr<VmObjectPaged>& vmo() const { return vmo_; }

  size_t content_size() const { return offset_; }

 private:
  size_t offset_{0};
  size_t size_;
  fbl::RefPtr<VmObjectPaged> vmo_;
};

constexpr const char kStackVmoName[] = "userboot-initial-stack";
constexpr const char kCrashlogVmoName[] = "crashlog";
constexpr const char kBootOptionsVmoname[] = "boot-options.txt";

KCOUNTER(timeline_userboot, "boot.timeline.userboot")
KCOUNTER(init_time, "init.userboot.time.msec")

class Userboot {
 public:
  Userboot(Userboot&&) = default;

  Userboot(HandoffEnd::Elf userboot, HandoffEnd::Elf vdso)
      : userboot_elf_{ktl::move(userboot)}, vdso_elf_{ktl::move(vdso)} {}

  [[nodiscard]] zx_status_t Start(ProcessDispatcher& process, VmAddressRegionDispatcher& root_vmar,
                                  fbl::RefPtr<ThreadDispatcher> thread, HandleOwner arg_handle,
                                  Handle*& out_vmar) {
    // Map in the userboot image along with the vDSO.
    zx::result mapped = Map(root_vmar);
    RETURN_IF_NOT_OK(mapped.status_value(), mapped.status_value());
    out_vmar = mapped->userboot_vmar.release();
    dprintf(SPEW, "userboot: %-31s @  %#" PRIxPTR "\n", "entry point", mapped->userboot_entry);

    // Set up the stack.
    zx::result<uintptr_t> sp = MapStack(root_vmar, mapped->stack_size);
    RETURN_IF_NOT_OK(sp.status_value(), sp.status_value());

    // Start the process running.
    return process.Start(ktl::move(thread), mapped->userboot_entry, sp.value(),
                         ktl::move(arg_handle), mapped->vdso_base);
  }

 private:
  struct Mapped {
    HandleOwner userboot_vmar;
    zx_vaddr_t userboot_entry;
    zx_vaddr_t vdso_base;
    size_t stack_size;
  };

  zx::result<Mapped> Map(VmAddressRegionDispatcher& root_vmar) {
    // Map userboot proper.
    zx::result userboot = MapHandoffElf(ktl::move(userboot_elf_), root_vmar);
    if (userboot.is_error()) {
      return userboot.take_error();
    }

    // Map the vDSO.
    zx::result vdso = MapHandoffElf(ktl::move(vdso_elf_), root_vmar);
    if (vdso.is_error()) {
      return vdso.take_error();
    }

    RETURN_IF_NOT(userboot->stack_size, zx::error(ZX_ERR_INVALID_ARGS));
    return zx::ok(Mapped{
        .userboot_vmar = ktl::move(userboot->vmar),
        .userboot_entry = userboot->entry,
        .vdso_base = vdso->vaddr_start,
        .stack_size = *userboot->stack_size,
    });
  }

  // Map the stack anywhere, in its own VMAR and a one-page guard region below.
  static zx::result<uintptr_t> MapStack(VmAddressRegionDispatcher& root_vmar, size_t stack_size) {
    fbl::RefPtr<VmObjectPaged> stack_vmo;
    zx_status_t status;

    RETURN_IF_NOT_OK(status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY | PMM_ALLOC_FLAG_CAN_WAIT,
                                                    0u, stack_size, &stack_vmo),
                     zx::error(status));
    stack_vmo->set_name(kStackVmoName, sizeof(kStackVmoName) - 1);

    const size_t vmar_size = stack_size + ZX_PAGE_SIZE;
    KernelHandle<VmAddressRegionDispatcher> vmar_handle;
    zx_rights_t vmar_rights;
    RETURN_IF_NOT_OK(
        status = root_vmar.Allocate(
            0, vmar_size, ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE | ZX_VM_CAN_MAP_SPECIFIC,
            &vmar_handle, &vmar_rights),
        zx::error(status));

    zx::result<VmAddressRegion::MapResult> map_result =
        vmar_handle.dispatcher()->Map(ZX_PAGE_SIZE, stack_vmo, 0, stack_size,
                                      ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_SPECIFIC);
    RETURN_IF_NOT_OK(map_result.status_value(), map_result.take_error());
    const uintptr_t stack_base = map_result->base;
    const uintptr_t sp = elfldltl::AbiTraits<>::InitialStackPointer(stack_base, stack_size);
    dprintf(SPEW, "userboot: %-31s @ [%#" PRIxPTR ", %#" PRIxPTR ")\n", "stack mapped", stack_base,
            stack_base + stack_size);
    constexpr auto hex_width = [](auto x) { return 2 + ((ktl::bit_width(x) + 3) / 4); };
    dprintf(SPEW, "userboot: %-31s @ %#*" PRIxPTR "\n", "sp",
            hex_width(stack_base) + 3 + hex_width(sp), sp);

    zx_rights_t vmo_rights;
    KernelHandle<VmObjectDispatcher> vmo_handle;
    RETURN_IF_NOT_OK(status = VmObjectDispatcher::Create(
                         ktl::move(stack_vmo), stack_size,
                         VmObjectDispatcher::InitialMutability::kMutable, &vmo_handle, &vmo_rights),
                     zx::error(status));

    return zx::ok(sp);
  }

  HandoffEnd::Elf userboot_elf_;
  HandoffEnd::Elf vdso_elf_;
};

// Keep a global reference to the kcounters vmo so that the kcounters
// memory always remains valid, even if userspace closes the last handle.
fbl::RefPtr<VmObject> kcounters_vmo_ref;

// Get a handle to a VM object, with full rights except perhaps for writing.
zx_status_t get_vmo_handle(fbl::RefPtr<VmObject> vmo, bool readonly, uint64_t content_size,
                           fbl::RefPtr<VmObjectDispatcher>* disp_ptr, Handle** ptr) {
  if (!vmo)
    return ZX_ERR_NO_MEMORY;

  zx_rights_t rights;
  KernelHandle<VmObjectDispatcher> vmo_kernel_handle;
  zx_status_t result = VmObjectDispatcher::Create(ktl::move(vmo), content_size,
                                                  VmObjectDispatcher::InitialMutability::kMutable,
                                                  &vmo_kernel_handle, &rights);
  if (result == ZX_OK) {
    if (disp_ptr)
      *disp_ptr = vmo_kernel_handle.dispatcher();
    if (readonly)
      rights &= ~ZX_RIGHT_WRITE;
    if (ptr)
      *ptr = Handle::Make(ktl::move(vmo_kernel_handle), rights).release();
  }
  return result;
}

HandleOwner get_job_handle() {
  return Handle::Dup(GetRootJobHandle(), JobDispatcher::default_rights());
}

// Converts platform crashlog into a VMO
zx_status_t crashlog_to_vmo(fbl::RefPtr<VmObject>* out, size_t* out_size) {
  PlatformCrashlog::Interface& crashlog = PlatformCrashlog::Get();

  size_t size = crashlog.Recover(nullptr);
  fbl::RefPtr<VmObjectPaged> crashlog_vmo;
  size_t aligned_size;
  zx_status_t status;
  RETURN_IF_NOT_OK(status = VmObject::RoundSize(size, &aligned_size), status);
  RETURN_IF_NOT_OK(
      status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0u, aligned_size, &crashlog_vmo), status);

  if (size) {
    VmoBuffer vmo_buffer{crashlog_vmo};
    FILE vmo_file{&vmo_buffer};
    crashlog.Recover(&vmo_file);
  }

  crashlog_vmo->set_name(kCrashlogVmoName, sizeof(kCrashlogVmoName) - 1);

  // Stash the recovered crashlog so that it may be propagated to the next
  // kernel instance in case we later mexec.
  crashlog_stash(crashlog_vmo);

  *out = ktl::move(crashlog_vmo);
  *out_size = size;

  // Now that we have recovered the old crashlog, enable crashlog uptime
  // updates.  This will cause systems with a RAM based crashlog to periodically
  // create a payload-less crashlog indicating a SW reboot reason of "unknown"
  // along with an uptime indicator.  If the system spontaneously reboots (due
  // to something like a WDT, or brownout) we will be able to recover this log
  // and know that we spontaneously rebooted, and have some idea of how long we
  // were running before we did.
  crashlog.EnableCrashlogUptimeUpdates(true);
  return ZX_OK;
}

void bootstrap_vmos(HandoffEnd handoff_end, ktl::span<Handle*, userboot::kHandleCount> handles,
                    ktl::optional<Userboot>& out_userboot) {
  // The instrumentation VMOs need to be created prior to the rootfs as the information for these
  // vmos is in the phys handoff region, which becomes inaccessible once the rootfs is created.
  RETURN_IF_NOT_OK(InstrumentationData::GetVmos(&handles[userboot::kFirstInstrumentationData]));

  for (size_t i = 0; i < PhysVmo::kMaxExtraHandoffPhysVmos; ++i) {
    handles[userboot::kFirstExtraPhysVmo + i] = handoff_end.extra_phys_vmos[i].release();
  }

  handles[userboot::kZbi] = handoff_end.zbi.release();

  KernelHandle<VmObjectDispatcher> vdso_kernel_handles[userboot::kNumVdsoVariants];
  KernelHandle<VmObjectDispatcher> time_values_handle;
  const VDso* vdso =
      VDso::Create(handoff_end.vdso, ktl::span{vdso_kernel_handles}, &time_values_handle);
  handles[userboot::kTimeValues] =
      Handle::Make(ktl::move(time_values_handle), (vdso->vmo_rights() & (~ZX_RIGHT_EXECUTE)))
          .release();
  RETURN_IF_NOT(handles[userboot::kTimeValues]);
  for (size_t i = 0; i < userboot::kNumVdsoVariants; ++i) {
    RETURN_IF_NOT(vdso_kernel_handles[i].dispatcher());
    handles[userboot::kFirstVdso + i] =
        Handle::Make(ktl::move(vdso_kernel_handles[i]), vdso->vmo_rights()).release();
    RETURN_IF_NOT(handles[userboot::kFirstVdso + i]);
  }
  DEBUG_ASSERT(handles[userboot::kFirstVdso + 1]->dispatcher() == vdso->dispatcher());
  if (gBootOptions->always_use_next_vdso) {
    ktl::swap(handles[userboot::kFirstVdso], handles[userboot::kFirstVdso + 1]);
  }

  // Crashlog.
  fbl::RefPtr<VmObject> crashlog_vmo;
  size_t crashlog_size = 0;
  RETURN_IF_NOT_OK(crashlog_to_vmo(&crashlog_vmo, &crashlog_size));
  RETURN_IF_NOT_OK(
      get_vmo_handle(crashlog_vmo, true, crashlog_size, nullptr, &handles[userboot::kCrashlog]));

  // Boot options.
  {
    VmoBuffer boot_options;
    FILE boot_options_file{&boot_options};
    gBootOptions->Show(/*defaults=*/false, &boot_options_file);
    boot_options.vmo()->set_name(kBootOptionsVmoname, sizeof(kBootOptionsVmoname) - 1);
    RETURN_IF_NOT_OK(get_vmo_handle(boot_options.vmo(), false, boot_options.content_size(), nullptr,
                                    &handles[userboot::kBootOptions]));
  }

#if ENABLE_ENTROPY_COLLECTOR_TEST
  RETURN_IF_NOT(!crypto::entropy::entropy_was_lost);
  RETURN_IF_NOT_OK(get_vmo_handle(crypto::entropy::entropy_vmo, true,
                                  crypto::entropy::entropy_vmo_content_size, nullptr,
                                  &handles[userboot::kEntropyTestData]));

#endif

  // kcounters names table.
  fbl::RefPtr<VmObjectPaged> kcountdesc_vmo;
  RETURN_IF_NOT_OK(VmObjectPaged::CreateFromWiredPages(
      CounterDesc().VmoData(), CounterDesc().VmoDataSize(), true, &kcountdesc_vmo));
  kcountdesc_vmo->set_name(counters::DescriptorVmo::kVmoName,
                           sizeof(counters::DescriptorVmo::kVmoName) - 1);
  RETURN_IF_NOT_OK(get_vmo_handle(ktl::move(kcountdesc_vmo), true, CounterDesc().VmoContentSize(),
                                  nullptr, &handles[userboot::kCounterNames]));

  // kcounters live data.
  fbl::RefPtr<VmObjectPaged> kcounters_vmo;
  RETURN_IF_NOT_OK(VmObjectPaged::CreateFromWiredPages(
      CounterArena().VmoData(), CounterArena().VmoDataSize(), false, &kcounters_vmo));
  kcounters_vmo_ref = kcounters_vmo;
  kcounters_vmo->set_name(counters::kArenaVmoName, sizeof(counters::kArenaVmoName) - 1);
  RETURN_IF_NOT_OK(get_vmo_handle(ktl::move(kcounters_vmo), true, CounterArena().VmoContentSize(),
                                  nullptr, &handles[userboot::kCounters]));

  out_userboot.emplace(ktl::move(handoff_end.userboot), ktl::move(handoff_end.vdso));
}

class BootstrapChannel {
 public:
  static zx::result<> Create(ProcessDispatcher& process,
                             ktl::optional<BootstrapChannel>& out_channel) {
    // Make the channel that will hold the message.
    KernelHandle<ChannelDispatcher> user_handle, kernel_handle;
    zx_rights_t channel_rights;
    zx_status_t res;
    RETURN_IF_NOT_OK(res = ChannelDispatcher::Create(&user_handle, &kernel_handle, &channel_rights),
                     zx::make_result(res));
    out_channel.emplace();
    out_channel->user_handle_ = Handle::Make(ktl::move(user_handle), channel_rights);
    out_channel->send_ = kernel_handle.release();
    return zx::ok();
  }

  zx::result<> Send(MessagePacketPtr msg) {
    RETURN_IF_NOT(send_, zx::make_result(ZX_ERR_BAD_STATE));
    return zx::make_result(ktl::exchange(send_, {})->Write(ZX_KOID_INVALID, ktl::move(msg)));
  }

  HandleOwner TakeUserHandle() { return ktl::move(user_handle_); }

 private:
  HandleOwner user_handle_;
  fbl::RefPtr<ChannelDispatcher> send_;
};

void MakeThread(fbl::RefPtr<ProcessDispatcher> process,
                fbl::RefPtr<ThreadDispatcher>& out_dispatcher, Handle*& out_handle) {
  ASSERT(out_dispatcher == nullptr);
  KernelHandle<ThreadDispatcher> thread_handle;
  zx_rights_t thread_rights;
  RETURN_IF_NOT_OK(
      ThreadDispatcher::Create(ktl::move(process), 0, "userboot", &thread_handle, &thread_rights));
  RETURN_IF_NOT_OK(thread_handle.dispatcher()->Initialize());
  out_dispatcher = thread_handle.dispatcher();
  out_handle = Handle::Make(ktl::move(thread_handle), thread_rights).release();
}

}  // namespace

void userboot_init(HandoffEnd handoff_end) {
  // Prepare the bootstrap message packet.  This allocates space for its
  // handles, which we'll fill in as we create things.
  MessagePacketPtr msg;
  zx_status_t status = MessagePacket::Create(nullptr, 0, userboot::kHandleCount, &msg);
  ASSERT(status == ZX_OK);
  msg->set_owns_handles(true);

  DEBUG_ASSERT(msg->num_handles() == userboot::kHandleCount);
  ktl::span<Handle*, userboot::kHandleCount> handles{msg->mutable_handles(), msg->num_handles()};

  // Create the process.
  KernelHandle<ProcessDispatcher> process_handle;
  KernelHandle<VmAddressRegionDispatcher> vmar_handle;
  zx_rights_t process_rights, vmar_rights;
  status = ProcessDispatcher::Create(GetRootJobDispatcher(), "userboot", 0, &process_handle,
                                     &process_rights, &vmar_handle, &vmar_rights);
  ASSERT(status == ZX_OK);

  // Create a root job observer, restarting the system if the root job becomes
  // childless.  From now, the life of the system is bound this first process
  // that runs the userboot static PIE.  If it dies without creating more
  // processes that live, the system restarts.
  StartRootJobObserver();
  fbl::RefPtr<ProcessDispatcher> process = process_handle.dispatcher();
  auto kill_userboot =
      fit::defer([process]() { process->Kill(ZX_TASK_RETCODE_CRITICAL_PROCESS_KILL); });

  // Create the user thread.
  fbl::RefPtr<ThreadDispatcher> thread;
  MakeThread(process_handle.dispatcher(), thread, handles[userboot::kThreadSelf]);
  RETURN_IF_NOT(thread);

  // Set up the bootstrap channel and install the other end in the process.
  ktl::optional<BootstrapChannel> bootstrap_channel;
  RETURN_IF_NOT_OK(
      BootstrapChannel::Create(*process_handle.dispatcher(), bootstrap_channel).status_value());
  RETURN_IF_NOT(bootstrap_channel.has_value());

  // Pack up the miscellaneous VMOs and take the userboot VMO and details.
  ktl::optional<Userboot> userboot;
  bootstrap_vmos(ktl::move(handoff_end), handles, userboot);
  RETURN_IF_NOT(userboot.has_value());

  // Start userboot running.  It may block waiting for the bootstrap message.
  RETURN_IF_NOT_OK(userboot->Start(*process_handle.dispatcher(), *vmar_handle.dispatcher(),
                                   ktl::move(thread), bootstrap_channel->TakeUserHandle(),
                                   handles[userboot::kVmarLoaded]));
  RETURN_IF_NOT(handles[userboot::kVmarLoaded]);

  // It needs its own process and root VMAR handles.
  HandleOwner proc_handle_owner = Handle::Make(ktl::move(process_handle), process_rights);
  HandleOwner vmar_handle_owner = Handle::Make(ktl::move(vmar_handle), vmar_rights);
  RETURN_IF_NOT(proc_handle_owner);
  RETURN_IF_NOT(vmar_handle_owner);
  handles[userboot::kProcSelf] = proc_handle_owner.release();
  handles[userboot::kVmarRootSelf] = vmar_handle_owner.release();
  handles[userboot::kMmioResource] = get_resource_handle(ZX_RSRC_KIND_MMIO).release();
  RETURN_IF_NOT(handles[userboot::kMmioResource]);
  handles[userboot::kIrqResource] = get_resource_handle(ZX_RSRC_KIND_IRQ).release();
  RETURN_IF_NOT(handles[userboot::kIrqResource]);
#if ARCH_X86
  handles[userboot::kIoportResource] = get_resource_handle(ZX_RSRC_KIND_IOPORT).release();
  RETURN_IF_NOT(handles[userboot::kIoportResource]);
#elif ARCH_ARM64
  handles[userboot::kSmcResource] = get_resource_handle(ZX_RSRC_KIND_SMC).release();
  RETURN_IF_NOT(handles[userboot::kSmcResource]);
#endif
  handles[userboot::kSystemResource] = get_resource_handle(ZX_RSRC_KIND_SYSTEM).release();
  RETURN_IF_NOT(handles[userboot::kSystemResource]);
  handles[userboot::kRootJob] = get_job_handle().release();
  RETURN_IF_NOT(handles[userboot::kRootJob]);

  // Send the bootstrap message.
  if (zx::result<> send_result = bootstrap_channel->Send(ktl::move(msg)); send_result.is_error()) {
    zx_info_process_t info = process->GetInfo();
    KERNEL_OOPS("write on userboot boostrap channel failed: %d; process retcode %" PRId64
                ", flags %#" PRIx32 "\n",
                send_result.error_value(), info.return_code, info.flags);
    return;
  }

  kill_userboot.cancel();

  timeline_userboot.Set(current_mono_ticks());
  init_time.Add(current_mono_time() / 1000000LL);
}
