// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <align.h>
#include <debug.h>
#include <lib/boot-options/boot-options.h>
#include <lib/debuglog.h>
#include <lib/fit/defer.h>
#include <lib/instrumentation/asan.h>
#include <lib/power-management/energy-model.h>
#include <lib/power-management/kernel-registry.h>
#include <lib/power-management/port-power-level-controller.h>
#include <lib/relaxed_atomic.h>
#include <lib/syscalls/forward.h>
#include <lib/zbi-format/kernel.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/checking.h>
#include <lib/zbitl/view.h>
#include <lib/zircon-internal/macros.h>
#include <mexec.h>
#include <platform.h>
#include <string.h>
#include <sys/types.h>
#include <trace.h>
#include <zircon/boot/crash-reason.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/status.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls/resource.h>
#include <zircon/syscalls/system.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>
#include <cstdio>

#include <arch/arch_ops.h>
#include <arch/mp.h>
#include <arch/ops.h>
#include <dev/hw_watchdog.h>
#include <dev/interrupt.h>
#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <kernel/cpu.h>
#include <kernel/idle_power_thread.h>
#include <kernel/mp.h>
#include <kernel/mutex.h>
#include <kernel/percpu.h>
#include <kernel/range_check.h>
#include <kernel/scheduler.h>
#include <kernel/thread.h>
#include <ktl/byte.h>
#include <ktl/span.h>
#include <ktl/unique_ptr.h>
#include <object/event_dispatcher.h>
#include <object/job_dispatcher.h>
#include <object/port_dispatcher.h>
#include <object/process_dispatcher.h>
#include <object/resource.h>
#include <object/user_handles.h>
#include <object/vm_object_dispatcher.h>
#include <platform/halt_helper.h>
#include <platform/halt_token.h>
#include <platform/timer.h>
#include <vm/physmap.h>
#include <vm/pmm.h>
#include <vm/vm.h>
#include <vm/vm_aspace.h>

#include "system_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

// Allocate this many extra bytes at the end of the bootdata for the platform
// to fill in with platform specific boot structures.
const size_t kBootdataPlatformExtraBytes = PAGE_SIZE * 4;

__BEGIN_CDECLS
extern void mexec_asm(void);
extern void mexec_asm_end(void);
__END_CDECLS

class IdentityPageAllocator {
 public:
  explicit IdentityPageAllocator(uintptr_t alloc_start) : alloc_start_(alloc_start) {
    allocated_ = LIST_INITIAL_VALUE(allocated_);
  }
  ~IdentityPageAllocator() { pmm_free(&allocated_); }

  /* Allocates a page of memory that has the same physical and virtual
  addresses. */
  zx_status_t Allocate(void** result);

  // Activate the 1:1 address space. P
  void Activate();

 private:
  zx_status_t InitializeAspace();
  fbl::RefPtr<VmAspace> aspace_ = nullptr;
  size_t mapping_id_ = 0;
  // Minimum physical/virtual address for all allocations.
  uintptr_t alloc_start_;
  list_node allocated_;
};

zx_status_t IdentityPageAllocator::InitializeAspace() {
  // The Aspace has already been initialized, nothing to do.
  if (aspace_) {
    return ZX_OK;
  }

  aspace_ = VmAspace::Create(VmAspace::Type::LowKernel, "identity");
  if (!aspace_) {
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

zx_status_t alloc_pages_greater_than(paddr_t lower_bound, size_t count, size_t limit,
                                     paddr_t* paddrs) {
  struct list_node list = LIST_INITIAL_VALUE(list);

  // We don't support partially completed requests. This function will either
  // allocate |count| pages or 0 pages. If we complete a partial allocation
  // but are unable to fulfil the complete request, we'll clean up any pages
  // that we may have allocated in the process.
  auto pmm_cleanup = fit::defer([&list]() { pmm_free(&list); });

  while (count) {
    // TODO: replace with pmm routine that can allocate while excluding a range.
    size_t actual = 0;
    list_node alloc_list = LIST_INITIAL_VALUE(alloc_list);
    zx_status_t status = pmm_alloc_range(lower_bound, count, &alloc_list);
    if (status == ZX_OK) {
      actual = count;
      if (list_is_empty(&list)) {
        list_move(&alloc_list, &list);
      } else {
        list_splice_after(&alloc_list, list_peek_tail(&list));
      }
    }

    for (size_t i = 0; i < actual; i++) {
      paddrs[count - (i + 1)] = lower_bound + PAGE_SIZE * i;
    }

    count -= actual;
    lower_bound += PAGE_SIZE * (actual + 1);

    // If we're past the limit and still trying to allocate, just give up.
    if (lower_bound >= limit) {
      return ZX_ERR_NO_RESOURCES;
    }
  }

  // mark all of the pages we allocated as WIRED.
  vm_page_t* p;
  list_for_every_entry (&list, p, vm_page_t, queue_node) {
    p->set_state(vm_page_state::WIRED);
  }

  // Make sure we don't free the pages we just allocated.
  pmm_cleanup.cancel();

  return ZX_OK;
}

zx_status_t IdentityPageAllocator::Allocate(void** result) {
  zx_status_t st;

  // Start by obtaining an unused physical page. This address will eventually
  // be the physical/virtual address of our identity mapped page.
  // TODO: when https://fxbug.dev/42105842 is completed, we should allocate low memory directly
  //       from the pmm rather than using "alloc_pages_greater_than" which is
  //       somewhat of a hack.
  paddr_t pa;
  DEBUG_ASSERT(alloc_start_ < 4 * GB);
  st = alloc_pages_greater_than(alloc_start_, 1, 4 * GB - alloc_start_, &pa);
  if (st != ZX_OK) {
    LTRACEF("mexec: failed to allocate page in low memory\n");
    return st;
  }

  // Add this page to the list of allocated pages such that it gets freed when
  // the object is destroyed.
  vm_page_t* page = paddr_to_vm_page(pa);
  DEBUG_ASSERT(page);
  list_add_tail(&allocated_, &page->queue_node);

  // The kernel address space may be in high memory which cannot be identity
  // mapped since all Kernel Virtual Addresses might be out of range of the
  // physical address space. For this reason, we need to make a new address
  // space.
  st = InitializeAspace();
  if (st != ZX_OK) {
    return st;
  }

  // Create a new allocation in the new address space that identity maps the
  // target page.
  constexpr uint kPermissionFlagsRWX =
      (ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE | ARCH_MMU_FLAG_PERM_EXECUTE);

  void* addr = reinterpret_cast<void*>(pa);

  // 2 ** 64 = 18446744073709551616
  // len("identity 18446744073709551616\n") == 30, round to sizeof(word) = 32
  char mapping_name[32];
  snprintf(mapping_name, sizeof(mapping_name), "identity %lu", mapping_id_++);

  st = aspace_->AllocPhysical(mapping_name, PAGE_SIZE, &addr, 0, pa,
                              VmAspace::VMM_FLAG_VALLOC_SPECIFIC, kPermissionFlagsRWX);
  if (st != ZX_OK) {
    return st;
  }

  *result = addr;
  return st;
}

void IdentityPageAllocator::Activate() {
  if (!aspace_) {
    panic("Cannot Activate 1:1 Aspace with no 1:1 mappings!");
  }
  vmm_set_active_aspace(aspace_.get());
}

/* Takes all the pages in a VMO and creates a copy of them where all the pages
 * occupy a physically contiguous region of physical memory.
 * TODO(gkalsi): Don't coalesce pages into a physically contiguous region and
 *               just pass a vectored I/O list to the mexec assembly.
 */
static zx_status_t vmo_coalesce_pages(zx_handle_t vmo_hdl, const size_t extra_bytes, paddr_t* addr,
                                      uint8_t** vaddr, size_t* size) {
  DEBUG_ASSERT(addr);
  if (!addr) {
    return ZX_ERR_INVALID_ARGS;
  }

  DEBUG_ASSERT(size);
  if (!size) {
    return ZX_ERR_INVALID_ARGS;
  }

  ProcessDispatcher* up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<VmObjectDispatcher> vmo_dispatcher;
  zx_status_t st =
      up->handle_table().GetDispatcherWithRights(*up, vmo_hdl, ZX_RIGHT_READ, &vmo_dispatcher);
  if (st != ZX_OK)
    return st;

  fbl::RefPtr<VmObject> vmo = vmo_dispatcher->vmo();

  const size_t vmo_size = vmo->size();

  const size_t num_pages = ROUNDUP(vmo_size + extra_bytes, PAGE_SIZE) / PAGE_SIZE;

  paddr_t base_addr;
  list_node list = LIST_INITIAL_VALUE(list);
  st = pmm_alloc_contiguous(num_pages, PMM_ALLOC_FLAG_ANY, 0, &base_addr, &list);
  if (st != ZX_OK) {
    // TODO(gkalsi): Free pages allocated by pmm_alloc_contiguous pages
    //               and return an error.
    panic("Failed to allocate contiguous memory");
  }

  uint8_t* dst_addr = (uint8_t*)paddr_to_physmap(base_addr);

  st = vmo->Read(dst_addr, 0, vmo_size);
  if (st != ZX_OK) {
    // TODO(gkalsi): Free pages allocated by pmm_alloc_contiguous pages
    //               and return an error.
    panic("Failed to read to contiguous vmo");
  }

  arch_clean_invalidate_cache_range((vaddr_t)dst_addr, vmo_size);

  *size = num_pages * PAGE_SIZE;
  *addr = base_addr;
  if (vaddr)
    *vaddr = dst_addr;

  return ZX_OK;
}

// zx_status_t zx_system_mexec_payload_get
zx_status_t sys_system_mexec_payload_get(zx_handle_t resource, user_out_ptr<void> user_buffer,
                                         size_t buffer_size) {
  if (!gBootOptions->enable_debugging_syscalls) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Highly privileged, only mexec resource should have access.
  if (zx_status_t result =
          validate_ranged_resource(resource, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_MEXEC_BASE, 1);
      result != ZX_OK) {
    return result;
  }

  // Limit the size of the result that we can return to userspace.
  if (buffer_size > kBootdataPlatformExtraBytes) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AllocChecker ac;
  auto buffer = new (&ac) ktl::byte[buffer_size];
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  if (auto result = WriteMexecData({buffer, buffer_size}); result.is_error()) {
    return result.error_value();
  } else {
    size_t zbi_size = ktl::move(result).value();
    ZX_DEBUG_ASSERT(zbi_size <= buffer_size);
    return user_buffer.reinterpret<ktl::byte>().copy_array_to_user(buffer, zbi_size);
  }
}

// zx_status_t zx_system_mexec
NO_ASAN zx_status_t sys_system_mexec(zx_handle_t resource, zx_handle_t kernel_vmo,
                                     zx_handle_t bootimage_vmo) {
  if (!gBootOptions->enable_debugging_syscalls) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t result =
      validate_ranged_resource(resource, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_MEXEC_BASE, 1);
  if (result != ZX_OK)
    return result;

  paddr_t new_kernel_addr;
  size_t new_kernel_len;
  result = vmo_coalesce_pages(kernel_vmo, 0, &new_kernel_addr, NULL, &new_kernel_len);
  if (result != ZX_OK) {
    return result;
  }

  paddr_t new_kernel_entry;
  {
    const zbi_header_t* header =
        reinterpret_cast<const zbi_header_t*>(paddr_to_physmap(new_kernel_addr));
    if (zbitl::CheckContainerHeader(*header).is_error()) {
      return ZX_ERR_IO_DATA_INTEGRITY;
    }
    zbitl::View zbi{zbitl::StorageFromRawHeader(header)};
    if (zbitl::CheckBootable(zbi).is_error()) {
      return ZX_ERR_IO_DATA_INTEGRITY;
    }
    const zbi_kernel_t* kernel = reinterpret_cast<const zbi_kernel_t*>(zbi.begin()->payload.data());
    new_kernel_entry = get_kernel_base_phys() + kernel->entry;
    ZX_ASSERT(zbi.take_error().is_ok());
  }

  paddr_t new_bootimage_addr;
  uint8_t* bootimage_buffer;
  size_t bootimage_len;
  result = vmo_coalesce_pages(bootimage_vmo, kBootdataPlatformExtraBytes, &new_bootimage_addr,
                              &bootimage_buffer, &bootimage_len);
  if (result != ZX_OK) {
    return result;
  }

  uintptr_t kernel_image_end = get_kernel_base_phys() + new_kernel_len;

  paddr_t final_bootimage_addr = new_bootimage_addr;
  // For testing purposes, we may want the bootdata at a high address. Alternatively if our
  // coalesced VMO should overlap into the target kernel range then we also need to move it, and
  // placing it high is as good as anywhere else.
  if (gBootOptions->mexec_force_high_ramdisk ||
      Intersects(final_bootimage_addr, bootimage_len, get_kernel_base_phys(), kernel_image_end)) {
    const size_t page_count = bootimage_len / PAGE_SIZE + 1;
    fbl::AllocChecker ac;
    ktl::unique_ptr<paddr_t[]> paddrs(new (&ac) paddr_t[page_count]);
    ASSERT(ac.check());

    // Allocate pages greater than 4GiB to test that we're tolerant of booting
    // with a ramdisk in high memory. This operation can be very expensive and
    // should be replaced with a PMM API that supports allocating from a
    // specific range of memory.
    result = alloc_pages_greater_than(4 * GB, page_count, 8 * GB, paddrs.get());
    ASSERT(result == ZX_OK);

    final_bootimage_addr = paddrs.get()[0];
  }

  IdentityPageAllocator id_alloc(kernel_image_end);
  void* id_page_addr = 0x0;
  result = id_alloc.Allocate(&id_page_addr);
  if (result != ZX_OK) {
    return result;
  }

  LTRACEF("zx_system_mexec allocated identity mapped page at %p\n", id_page_addr);

  Thread::Current::MigrateToCpu(BOOT_CPU_ID);

  // We assume that when the system starts, only one CPU is running. We denote
  // this as the boot CPU.
  // We want to make sure that this is the CPU that eventually branches into
  // the new kernel so we attempt to migrate this thread to that cpu.
  result = platform_halt_secondary_cpus(ZX_TIME_INFINITE);
  DEBUG_ASSERT(result == ZX_OK);

  platform_mexec_prep(final_bootimage_addr, bootimage_len);

  const zx_instant_mono_t dlog_deadline = current_time() + ZX_SEC(5);
  dlog_shutdown(dlog_deadline);

  // Give the watchdog one last pet to hold it off until the new image has booted far enough to pet
  // the dog itself (or disable it).
  hw_watchdog_pet();

  arch_disable_ints();

  // WARNING
  // It is unsafe to return from this function beyond this point.
  // This is because we have swapped out the user address space and halted the
  // secondary cores and there is no trivial way to bring both of these back.
  id_alloc.Activate();

  // We're going to copy this into our identity page, make sure it's not
  // longer than a single page.
  size_t mexec_asm_length = (uintptr_t)mexec_asm_end - (uintptr_t)mexec_asm;
  DEBUG_ASSERT(mexec_asm_length <= PAGE_SIZE);

  __unsanitized_memcpy(id_page_addr, (const void*)mexec_asm, mexec_asm_length);
  arch_sync_cache_range((vaddr_t)id_page_addr, mexec_asm_length);

  // We must pass in an arg that represents a list of memory regions to
  // shuffle around. We put this args list immediately after the mexec
  // assembly.
  // Put the args list in a separate page.
  void* ops_ptr;
  result = id_alloc.Allocate(&ops_ptr);
  DEBUG_ASSERT(result == ZX_OK);
  memmov_ops_t* ops = (memmov_ops_t*)(ops_ptr);

  uint32_t ops_idx = 0;

  // Op to move the new kernel into place.
  ops[ops_idx].src = (void*)new_kernel_addr;
  ops[ops_idx].dst = (void*)get_kernel_base_phys();
  ops[ops_idx].len = new_kernel_len;
  ops_idx++;

  // We can leave the bootimage in place unless we've been asked to move it to
  // high memory.
  if (new_bootimage_addr != final_bootimage_addr) {
    ops[ops_idx].src = (void*)new_bootimage_addr;
    ops[ops_idx].dst = (void*)final_bootimage_addr;
    ops[ops_idx].len = bootimage_len;
    ops_idx++;
  }

  // Null terminated list.
  ops[ops_idx++] = {0, 0, 0};

  // Make sure that the kernel, when copied, will not overwrite the bootdata, our mexec code or
  // copy ops.
  DEBUG_ASSERT(!Intersects(reinterpret_cast<uintptr_t>(ops[0].dst), ops[0].len,
                           reinterpret_cast<uintptr_t>(final_bootimage_addr), bootimage_len));
  DEBUG_ASSERT(!Intersects(reinterpret_cast<uintptr_t>(ops[0].dst), ops[0].len,
                           reinterpret_cast<uintptr_t>(id_page_addr),
                           static_cast<size_t>(PAGE_SIZE)));
  DEBUG_ASSERT(!Intersects(reinterpret_cast<uintptr_t>(ops[0].dst), ops[0].len,
                           reinterpret_cast<uintptr_t>(ops_ptr), static_cast<size_t>(PAGE_SIZE)));

  // Sync because there is code in here that we intend to run.
  arch_sync_cache_range((vaddr_t)id_page_addr, PAGE_SIZE);

  // Clean because we're going to turn the MMU/caches off and we want to make
  // sure that things are still available afterwards.
  arch_clean_cache_range((vaddr_t)id_page_addr, PAGE_SIZE);
  arch_clean_cache_range((vaddr_t)ops_ptr, PAGE_SIZE);

  // Shutdown the timer and interrupts.  Performing shutdown of these components
  // is critical as we might be using a PV clock or PV EOI signaling so we must
  // tell our hypervisor to stop updating them to avoid corrupting aribtrary
  // memory post-mexec.
  platform_stop_timer();
  platform_shutdown_timer();
  shutdown_interrupts_curr_cpu();
  shutdown_interrupts();

  // Ask the platform to mexec into the next kernel.
  mexec_asm_func mexec_assembly = (mexec_asm_func)id_page_addr;
  platform_mexec(mexec_assembly, ops, final_bootimage_addr, bootimage_len, new_kernel_entry);

  panic("Execution should never reach here\n");
  return ZX_OK;
}

// zx_status_t zx_system_powerctl
zx_status_t sys_system_powerctl(zx_handle_t power_rsrc, uint32_t cmd,
                                user_in_ptr<const zx_system_powerctl_arg_t> raw_arg) {
  zx_status_t status;
  if ((status = validate_ranged_resource(power_rsrc, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_POWER_BASE,
                                         1)) != ZX_OK) {
    return status;
  }

  switch (cmd) {
    case ZX_SYSTEM_POWERCTL_ENABLE_ALL_CPUS: {
      cpu_mask_t all_cpus = ((cpu_mask_t)1u << arch_max_num_cpus()) - 1;
      return mp_hotplug_cpu_mask(~mp_get_online_mask() & all_cpus);
    }
    case ZX_SYSTEM_POWERCTL_DISABLE_ALL_CPUS_BUT_PRIMARY: {
      cpu_mask_t primary = cpu_num_to_mask(0);
      return mp_unplug_cpu_mask(mp_get_online_mask() & ~primary, ZX_TIME_INFINITE);
    }
#if defined __x86_64__
    case ZX_SYSTEM_POWERCTL_ACPI_TRANSITION_S_STATE:
      return ZX_ERR_NOT_SUPPORTED;
    case ZX_SYSTEM_POWERCTL_X86_SET_PKG_PL1: {
      zx_system_powerctl_arg_t arg;
      MsrAccess msr;
      status = raw_arg.copy_from_user(&arg);
      if (status != ZX_OK) {
        return status;
      }

      return arch_system_powerctl(cmd, &arg, &msr);
    }
#endif  //__x86_64
    case ZX_SYSTEM_POWERCTL_REBOOT:
      platform_graceful_halt_helper(HALT_ACTION_REBOOT, ZirconCrashReason::NoCrash,
                                    ZX_TIME_INFINITE);
      break;
    case ZX_SYSTEM_POWERCTL_ACK_KERNEL_INITIATED_REBOOT:
      return HaltToken::Get().AckPendingHalt();
    case ZX_SYSTEM_POWERCTL_REBOOT_BOOTLOADER:
      platform_graceful_halt_helper(HALT_ACTION_REBOOT_BOOTLOADER, ZirconCrashReason::NoCrash,
                                    ZX_TIME_INFINITE);
      break;
    case ZX_SYSTEM_POWERCTL_REBOOT_RECOVERY:
      platform_graceful_halt_helper(HALT_ACTION_REBOOT_RECOVERY, ZirconCrashReason::NoCrash,
                                    ZX_TIME_INFINITE);
      break;
    case ZX_SYSTEM_POWERCTL_SHUTDOWN:
      platform_graceful_halt_helper(HALT_ACTION_SHUTDOWN, ZirconCrashReason::NoCrash,
                                    ZX_TIME_INFINITE);
      break;
    default:
      return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

// zx_status_t zx_system_get_event
zx_status_t sys_system_get_event(zx_handle_t root_job, uint32_t kind, zx_handle_t* out) {
  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<JobDispatcher> job;
  zx_status_t status;
  if (kind == ZX_SYSTEM_EVENT_OUT_OF_MEMORY) {
    status =
        up->handle_table().GetDispatcherWithRights(*up, root_job, ZX_RIGHT_MANAGE_PROCESS, &job);
  } else {
    // We check for the root job below. We should not need to enforce rights beyond that.
    status = up->handle_table().GetDispatcherWithRights(*up, root_job, ZX_RIGHT_NONE, &job);
  }
  if (status != ZX_OK) {
    return status;
  }

  // Validate that the job is in fact the first usermode job (aka root job).
  if (job != GetRootJobDispatcher()) {
    return ZX_ERR_ACCESS_DENIED;
  }

  switch (kind) {
    case ZX_SYSTEM_EVENT_OUT_OF_MEMORY:
    case ZX_SYSTEM_EVENT_IMMINENT_OUT_OF_MEMORY:
    case ZX_SYSTEM_EVENT_MEMORY_PRESSURE_CRITICAL:
    case ZX_SYSTEM_EVENT_MEMORY_PRESSURE_WARNING:
    case ZX_SYSTEM_EVENT_MEMORY_PRESSURE_NORMAL:
      // Do not grant default event rights, as we don't want userspace to, for
      // example, be able to signal this event.
      return up->MakeAndAddHandle(GetMemPressureEvent(kind),
                                  ZX_DEFAULT_SYSTEM_EVENT_LOW_MEMORY_RIGHTS, out);

    default:
      return ZX_ERR_INVALID_ARGS;
  }
}

zx_status_t sys_system_set_performance_info(zx_handle_t resource, uint32_t topic,
                                            user_in_ptr<const void> info_void, size_t count) {
  const zx_status_t validate_status =
      validate_ranged_resource(resource, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_CPU_BASE, 1);
  if (validate_status != ZX_OK) {
    return validate_status;
  }

  if (topic != ZX_CPU_PERF_SCALE) {
    return ZX_ERR_INVALID_ARGS;
  }

  const size_t num_cpus = percpu::processor_count();
  if (count == 0 || count > num_cpus) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  fbl::AllocChecker checker;
  auto performance_info = ktl::make_unique<zx_cpu_performance_info_t[]>(&checker, count);
  if (!checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto new_info = info_void.reinterpret<const zx_cpu_performance_info_t>();
  if (new_info.copy_array_from_user(performance_info.get(), count) != ZX_OK) {
    return ZX_ERR_INVALID_ARGS;
  }

  cpu_num_t last_cpu = INVALID_CPU;
  for (auto& info : ktl::span{performance_info.get(), count}) {
    const cpu_num_t cpu = info.logical_cpu_number;
    if (last_cpu != INVALID_CPU && cpu <= last_cpu) {
      return ZX_ERR_INVALID_ARGS;
    }
    last_cpu = cpu;

    const auto [integral, fractional] = info.performance_scale;
    if (cpu >= num_cpus || (integral == 0 && fractional == 0)) {
      return ZX_ERR_OUT_OF_RANGE;
    }
  }

  Scheduler::UpdatePerformanceScales(performance_info.get(), count);
  return ZX_OK;
}

zx_status_t sys_system_get_performance_info(zx_handle_t resource, uint32_t topic, size_t info_count,
                                            user_out_ptr<void> info_void,
                                            user_out_ptr<size_t> output_count) {
  const zx_status_t validate_status =
      validate_ranged_resource(resource, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_CPU_BASE, 1);
  if (validate_status != ZX_OK) {
    return validate_status;
  }

  const size_t num_cpus = percpu::processor_count();
  if (info_count != num_cpus) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  fbl::AllocChecker checker;
  auto performance_info = ktl::make_unique<zx_cpu_performance_info_t[]>(&checker, info_count);
  if (!checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  switch (topic) {
    case ZX_CPU_PERF_SCALE:
      Scheduler::GetPerformanceScales(performance_info.get(), info_count);
      break;

    case ZX_CPU_DEFAULT_PERF_SCALE:
      Scheduler::GetDefaultPerformanceScales(performance_info.get(), info_count);
      break;

    default:
      return ZX_ERR_INVALID_ARGS;
  }

  auto info = info_void.reinterpret<zx_cpu_performance_info_t>();
  if (info.copy_array_to_user(performance_info.get(), info_count) != ZX_OK) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (output_count.copy_to_user(info_count) != ZX_OK) {
    return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

// TODO(https://fxbug.dev/42182544): Reconcile with HaltToken, zx_system_powerctl, and
// kernel-initiated-oom-reboot.
zx_status_t sys_system_suspend_enter(zx_handle_t resource, zx_instant_boot_t resume_deadline) {
  const zx_status_t validate_status =
      validate_ranged_resource(resource, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_CPU_BASE, 1);
  if (validate_status != ZX_OK) {
    return validate_status;
  }

  return IdlePowerThread::TransitionAllActiveToSuspend(resume_deadline);
}

zx_status_t sys_system_set_processor_power_domain(
    zx_handle_t resource, uint64_t options, user_in_ptr<const zx_processor_power_domain_t> domain,
    zx_handle_t port, user_in_ptr<const zx_processor_power_level_t> power_levels,
    size_t num_power_levels, user_in_ptr<const zx_processor_power_level_transition_t> transitions,
    size_t num_transitions) {
  zx_status_t status =
      validate_ranged_resource(resource, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_CPU_BASE, 1);
  if (status != ZX_OK) {
    return status;
  }

  if (num_power_levels > ZX_MAX_POWER_LEVELS ||
      num_transitions > ZX_MAX_POWER_LEVEL_TRANSFORMATIONS) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  zx_processor_power_domain_t domain_info;
  if (domain.copy_from_user(&domain_info) != ZX_OK) {
    return ZX_ERR_INVALID_ARGS;
  }

  bool all_zero = true;
  for (auto& c : domain_info.cpus.mask) {
    all_zero = all_zero && (c == 0);
  }

  // No need to validate any of the other parameters, when we are unregistering a power domain.
  if (all_zero) {
    return power_management::KernelPowerDomainRegistry::Unregister(domain_info.domain_id)
        .status_value();
  }

  if (num_power_levels == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  size_t max_cpus = arch_max_num_cpus();
  size_t bucket = max_cpus / ZX_CPU_SET_BITS_PER_WORD;
  size_t bits = max_cpus % ZX_CPU_SET_BITS_PER_WORD;
  size_t mask = ~((1ull << bits) - 1);

  // We are not allowed to set cpus beyond our max cpus.
  if ((domain_info.cpus.mask[bucket] & mask) != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  for (size_t i = bucket + 1; i < ZX_CPU_SET_MAX_CPUS / ZX_CPU_SET_BITS_PER_WORD; ++i) {
    if (domain_info.cpus.mask[i] != 0) {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  // Check the port has required rights.
  ProcessDispatcher* up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<PortDispatcher> port_dispatcher;
  if (zx_status_t res = up->handle_table().GetDispatcherWithRights(
          *up, port, ZX_RIGHT_WRITE | ZX_RIGHT_READ, &port_dispatcher);
      res != ZX_OK) {
    return res;
  }

  // Set up the power domain and model.
  fbl::AllocChecker ac;
  auto levels = ktl::make_unique<zx_processor_power_level_t[]>(&ac, num_power_levels);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  ktl::unique_ptr<zx_processor_power_level_transition_t[]> sparse_transitions = nullptr;

  if (num_transitions > 0) {
    sparse_transitions =
        ktl::make_unique<zx_processor_power_level_transition_t[]>(&ac, num_transitions);
    if (!ac.check()) {
      return ZX_ERR_NO_MEMORY;
    }

    if (zx_status_t res =
            transitions.copy_array_from_user(sparse_transitions.get(), num_transitions);
        res != ZX_OK) {
      return res;
    }
  }

  if (zx_status_t res = power_levels.copy_array_from_user(levels.get(), num_power_levels);
      res != ZX_OK) {
    return res;
  }

  auto model =
      power_management::EnergyModel::Create(ktl::span(levels.get(), num_power_levels),
                                            ktl::span(sparse_transitions.get(), num_transitions));
  if (model.is_error()) {
    return model.error_value();
  }

  auto controller = fbl::MakeRefCountedChecked<power_management::PortPowerLevelController>(
      &ac, ktl::move(port_dispatcher));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto power_domain = fbl::MakeRefCountedChecked<power_management::PowerDomain>(
      &ac, domain_info.domain_id, domain_info.cpus, ktl::move(model).value(),
      ktl::move(controller));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  // Register power domain with the registry and update schedulers.
  return power_management::KernelPowerDomainRegistry::Register(ktl::move(power_domain))
      .status_value();
}

zx_status_t sys_system_set_processor_power_state(
    zx_handle_t port, user_in_ptr<const zx_processor_power_state_t> power_state) {
  if (port == ZX_HANDLE_INVALID) {
    return ZX_ERR_BAD_HANDLE;
  }
  zx_processor_power_state_t ps = {};
  if (auto res = power_state.copy_from_user(&ps); res != ZX_OK) {
    return res;
  }

  ProcessDispatcher* up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<PortDispatcher> port_dispatcher;
  if (zx_status_t res =
          up->handle_table().GetDispatcherWithRights(*up, port, ZX_RIGHT_READ, &port_dispatcher);
      res != ZX_OK) {
    return res;
  }

  return power_management::KernelPowerDomainRegistry::UpdateDomainPowerLevel(
             ps.domain_id, port_dispatcher->get_koid(),
             static_cast<power_management::ControlInterface>(ps.control_interface),
             ps.control_argument)
      .status_value();
}
