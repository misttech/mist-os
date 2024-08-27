// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/mistos/userloader/userloader.h"

#include <lib/boot-options/boot-options.h>
#include <lib/counters.h>
#include <lib/crashlog.h>
#include <lib/elfldltl/machine.h>
#include <lib/mistos/userloader/userloader_internal.h>
#include <lib/mistos/zx_syscalls/util.h>
#include <lib/zircon-internal/default_stack_size.h>
#include <zircon/assert.h>

#include <fbl/ref_ptr.h>
#include <lk/init.h>
#include <object/handle.h>
#include <object/job_dispatcher.h>
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

#include <ktl/enforce.h>

namespace userloader {
ktl::array<Handle*, userloader::kHandleCount> gHandles;
}

namespace {

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
      zx_status_t status = vmo_->Resize(offset_ + str.size());
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

// constexpr const char kCrashlogVmoName[] = "crashlog";
// constexpr const char kBootOptionsVmoname[] = "boot-options.txt";
constexpr const char kZbiVmoName[] = "zbi";

// KCOUNTER(timeline_userboot, "boot.timeline.userboot")
// KCOUNTER(init_time, "init.userboot.time.msec")

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

#if 0
// Converts platform crashlog into a VMO
zx_status_t crashlog_to_vmo(fbl::RefPtr<VmObject>* out, size_t* out_size) {
  PlatformCrashlog::Interface& crashlog = PlatformCrashlog::Get();

  size_t size = crashlog.Recover(nullptr);
  fbl::RefPtr<VmObjectPaged> crashlog_vmo;
  zx_status_t status = VmObjectPaged::Create(
      PMM_ALLOC_FLAG_ANY, 0u, size, AttributionObject::GetKernelAttribution(), &crashlog_vmo);

  if (status != ZX_OK) {
    return status;
  }

  if (size) {
    VmoBuffer vmo_buffer{crashlog_vmo};
    FILE vmo_file = FILE{&vmo_buffer};
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
#endif

void bootstrap_vmos(ktl::array<Handle*, userloader::kHandleCount>& handles) {
  ktl::span<ktl::byte> zbi = ZbiInPhysmap(true);
  void* rbase = zbi.data();
  size_t rsize = ROUNDUP_PAGE_SIZE(zbi.size_bytes());
  dprintf(INFO, "userloader: ramdisk %#15zx @ %p\n", rsize, rbase);

#if 0
  // The instrumentation VMOs need to be created prior to the rootfs as the information for these
  // vmos is in the phys handoff region, which becomes inaccessible once the rootfs is created.
  zx_status_t status = InstrumentationData::GetVmos(&handles[userboot::kFirstInstrumentationData]);
  ASSERT(status == ZX_OK);
#endif

  // The ZBI.
  fbl::RefPtr<VmObjectPaged> rootfs_vmo;
  zx_status_t status = VmObjectPaged::CreateFromWiredPages(rbase, rsize, true, &rootfs_vmo);
  ASSERT(status == ZX_OK);
  rootfs_vmo->set_name(kZbiVmoName, sizeof(kZbiVmoName) - 1);
  status =
      get_vmo_handle(rootfs_vmo, false, rsize, nullptr, &userloader::gHandles[userloader::kZbi]);
  ASSERT(status == ZX_OK);
  // The rootfs vmo was created with exclusive=true, which means that the VMO is sole owner of those
  // pages. gPhysHandoff represents a pointer to the previous physmap location of the data, but the
  // VMO is free to move and use different pages to represent the data, as such attempting to use
  // this old reference is essentially a use-after-free.
  gPhysHandoff = nullptr;

#if 0
  // Crashlog.
  fbl::RefPtr<VmObject> crashlog_vmo;
  size_t crashlog_size = 0;
  status = crashlog_to_vmo(&crashlog_vmo, &crashlog_size);
  ASSERT(status == ZX_OK);
  status =
      get_vmo_handle(crashlog_vmo, true, crashlog_size, nullptr, &handles[userboot::kCrashlog]);
  ASSERT(status == ZX_OK);

  // Boot options.
  {
    VmoBuffer boot_options;
    FILE boot_options_file{&boot_options};
    gBootOptions->Show(/*defaults=*/false, &boot_options_file);
    boot_options.vmo()->set_name(kBootOptionsVmoname, sizeof(kBootOptionsVmoname) - 1);
    status = get_vmo_handle(boot_options.vmo(), false, boot_options.content_size(), nullptr,
                            &handles[userboot::kBootOptions]);
    ZX_ASSERT(status == ZX_OK);
  }

#if ENABLE_ENTROPY_COLLECTOR_TEST
  ASSERT(!crypto::entropy::entropy_was_lost);
  status =
      get_vmo_handle(crypto::entropy::entropy_vmo, true, crypto::entropy::entropy_vmo_content_size,
                     nullptr, &handles[kEntropyTestData]);
  ASSERT(status == ZX_OK);
#endif

  // kcounters names table.
  fbl::RefPtr<VmObjectPaged> kcountdesc_vmo;
  status = VmObjectPaged::CreateFromWiredPages(CounterDesc().VmoData(), CounterDesc().VmoDataSize(),
                                               true, AttributionObject::GetKernelAttribution(),
                                               &kcountdesc_vmo);
  ASSERT(status == ZX_OK);
  kcountdesc_vmo->set_name(counters::DescriptorVmo::kVmoName,
                           sizeof(counters::DescriptorVmo::kVmoName) - 1);
  status = get_vmo_handle(ktl::move(kcountdesc_vmo), true, CounterDesc().VmoContentSize(), nullptr,
                          &handles[userboot::kCounterNames]);
  ASSERT(status == ZX_OK);

  // kcounters live data.
  fbl::RefPtr<VmObjectPaged> kcounters_vmo;
  status = VmObjectPaged::CreateFromWiredPages(
      CounterArena().VmoData(), CounterArena().VmoDataSize(), false,
      AttributionObject::GetKernelAttribution(), &kcounters_vmo);
  ASSERT(status == ZX_OK);
  kcounters_vmo_ref = kcounters_vmo;
  kcounters_vmo->set_name(counters::kArenaVmoName, sizeof(counters::kArenaVmoName) - 1);
  status = get_vmo_handle(ktl::move(kcounters_vmo), true, CounterArena().VmoContentSize(), nullptr,
                          &handles[userboot::kCounters]);
  ASSERT(status == ZX_OK);
#endif
}

void userloader_init(uint) {  // zx_status_t status;
  bootstrap_vmos(userloader::gHandles);

  userloader::gHandles[userloader::kSystemResource] =
      get_resource_handle(ZX_RSRC_KIND_SYSTEM).release();
  ASSERT(userloader::gHandles[userloader::kSystemResource]);

  // Inject basic handles in the kernel handle_table.
  ProcessDispatcher* dispatcher = nullptr;
  for (auto handle : userloader::gHandles) {
    if (handle) {
      HandleOwner h(handle);
      handle_table(dispatcher).AddHandle(ktl::move(h));
    }
  }
}

}  // namespace

LK_INIT_HOOK(mist_os_userloader, userloader_init, LK_INIT_LEVEL_USER - 1)
