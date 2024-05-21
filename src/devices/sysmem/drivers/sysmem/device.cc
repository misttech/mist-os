// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <bind/fuchsia/sysmem/heap/cpp/bind.h>
// TODO(b/42113093): Remove this include of AmLogic-specific heap names in sysmem code. The include
// is currently needed for secure heap names only, which is why an include for goldfish heap names
// isn't here.
#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <inttypes.h>
#include <lib/async/dispatcher.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/sync/cpp/completion.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zx/channel.h>
#include <lib/zx/event.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/threads.h>

#include <algorithm>
#include <memory>
#include <string>
#include <thread>

#include <bind/fuchsia/amlogic/platform/sysmem/heap/cpp/bind.h>
#include <fbl/string_printf.h>
#include <sdk/lib/sys/cpp/service_directory.h>

#include "allocator.h"
#include "buffer_collection_token.h"
#include "contiguous_pooled_memory_allocator.h"
#include "driver.h"
#include "external_memory_allocator.h"
#include "lib/ddk/debug.h"
#include "lib/ddk/driver.h"
#include "macros.h"
#include "src/devices/sysmem/drivers/sysmem/sysmem_config.h"
#include "src/devices/sysmem/metrics/metrics.cb.h"
#include "utils.h"

using sysmem_driver::MemoryAllocator;

namespace sysmem_driver {
namespace {

constexpr bool kLogAllCollectionsPeriodically = false;
constexpr zx::duration kLogAllCollectionsInterval = zx::sec(20);

// These defaults only take effect if there is no
// fuchsia.hardware.sysmem/SYSMEM_METADATA_TYPE, and also neither of these
// kernel cmdline parameters set: driver.sysmem.contiguous_memory_size
// driver.sysmem.protected_memory_size
//
// Typically these defaults are overriden.
//
// By default there is no protected memory pool.
constexpr int64_t kDefaultProtectedMemorySize = 0;
// By default we pre-reserve 5% of physical memory for contiguous memory
// allocation via sysmem.
//
// This is enough to allow tests in sysmem_tests.cc to pass, and avoids relying
// on zx::vmo::create_contiguous() after early boot (by default), since it can
// fail if physical memory has gotten too fragmented.
constexpr int64_t kDefaultContiguousMemorySize = -5;

// fbl::round_up() doesn't work on signed types.
template <typename T>
T AlignUp(T value, T divisor) {
  return (value + divisor - 1) / divisor * divisor;
}

// Helper function to build owned HeapProperties table with coherency domain support.
fuchsia_hardware_sysmem::HeapProperties BuildHeapPropertiesWithCoherencyDomainSupport(
    bool cpu_supported, bool ram_supported, bool inaccessible_supported, bool need_clear,
    bool need_flush) {
  using fuchsia_hardware_sysmem::CoherencyDomainSupport;
  using fuchsia_hardware_sysmem::HeapProperties;

  CoherencyDomainSupport coherency_domain_support;
  coherency_domain_support.cpu_supported().emplace(cpu_supported);
  coherency_domain_support.ram_supported().emplace(ram_supported);
  coherency_domain_support.inaccessible_supported().emplace(inaccessible_supported);

  HeapProperties heap_properties;
  heap_properties.coherency_domain_support().emplace(std::move(coherency_domain_support));
  heap_properties.need_clear().emplace(need_clear);
  heap_properties.need_flush().emplace(need_flush);
  return heap_properties;
}

class SystemRamMemoryAllocator : public MemoryAllocator {
 public:
  explicit SystemRamMemoryAllocator(Owner* parent_device)
      : MemoryAllocator(BuildHeapPropertiesWithCoherencyDomainSupport(
            true /*cpu*/, true /*ram*/, true /*inaccessible*/,
            // Zircon guarantees created VMO are filled with 0; sysmem doesn't
            // need to clear it once again.  There's little point in flushing a
            // demand-backed VMO that's only virtually filled with 0.
            /*need_clear=*/false, /*need_flush=*/false)) {
    node_ = parent_device->heap_node()->CreateChild("SysmemRamMemoryAllocator");
    node_.CreateUint("id", id(), &properties_);
  }

  zx_status_t Allocate(uint64_t size, const fuchsia_sysmem2::SingleBufferSettings& settings,
                       std::optional<std::string> name, uint64_t buffer_collection_id,
                       uint32_t buffer_index, zx::vmo* parent_vmo) override {
    ZX_DEBUG_ASSERT_MSG(size % zx_system_get_page_size() == 0, "size: 0x%" PRIx64, size);
    ZX_DEBUG_ASSERT_MSG(
        fbl::round_up(*settings.buffer_settings()->size_bytes(), zx_system_get_page_size()) == size,
        "size_bytes: %" PRIu64 " size: 0x%" PRIx64, *settings.buffer_settings()->size_bytes(),
        size);
    zx_status_t status = zx::vmo::create(size, 0, parent_vmo);
    if (status != ZX_OK) {
      return status;
    }
    constexpr const char vmo_name[] = "Sysmem-core";
    parent_vmo->set_property(ZX_PROP_NAME, vmo_name, sizeof(vmo_name));
    return status;
  }

  void Delete(zx::vmo parent_vmo) override {
    // ~parent_vmo
  }
  // Since this allocator only allocates independent VMOs, it's fine to orphan those VMOs from the
  // allocator since the VMOs independently track what pages they're using.  So this allocator can
  // always claim is_empty() true.
  bool is_empty() override { return true; }

 private:
  inspect::Node node_;
  inspect::ValueList properties_;
};

class ContiguousSystemRamMemoryAllocator : public MemoryAllocator {
 public:
  explicit ContiguousSystemRamMemoryAllocator(zx::unowned_resource info_resource,
                                              Owner* parent_device)
      : MemoryAllocator(BuildHeapPropertiesWithCoherencyDomainSupport(
            /*cpu_supported=*/true, /*ram_supported=*/true,
            /*inaccessible_supported=*/true,
            // Zircon guarantees contagious VMO created are filled with 0;
            // sysmem doesn't need to clear it once again.  Unlike non-contiguous
            // VMOs which haven't backed pages yet, contiguous VMOs have backed
            // pages, and it's effective to flush the zeroes to RAM.  Some current
            // sysmem clients rely on contiguous allocations having their initial
            // zero-fill already flushed to RAM (at least for the RAM coherency
            // domain, this should probably remain true).
            /*need_clear=*/false, /*need_flush=*/true)),
        parent_device_(parent_device),
        info_resource_(std::move(info_resource)) {
    node_ = parent_device_->heap_node()->CreateChild("ContiguousSystemRamMemoryAllocator");
    node_.CreateUint("id", id(), &properties_);
  }

  zx_status_t Allocate(uint64_t size, const fuchsia_sysmem2::SingleBufferSettings& settings,
                       std::optional<std::string> name, uint64_t buffer_collection_id,
                       uint32_t buffer_index, zx::vmo* parent_vmo) override {
    ZX_DEBUG_ASSERT_MSG(size % zx_system_get_page_size() == 0, "size: 0x%" PRIx64, size);
    ZX_DEBUG_ASSERT_MSG(
        fbl::round_up(*settings.buffer_settings()->size_bytes(), zx_system_get_page_size()) == size,
        "size_bytes: %" PRIu64 " size: 0x%" PRIx64, *settings.buffer_settings()->size_bytes(),
        size);
    zx::vmo result_parent_vmo;
    // This code is unlikely to work after running for a while and physical
    // memory is more fragmented than early during boot. The
    // ContiguousPooledMemoryAllocator handles that case by keeping
    // a separate pool of contiguous memory.
    zx_status_t status =
        zx::vmo::create_contiguous(parent_device_->bti(), size, 0, &result_parent_vmo);
    if (status != ZX_OK) {
      DRIVER_ERROR("zx::vmo::create_contiguous() failed - size_bytes: %" PRIu64 " status: %d", size,
                   status);
      zx_info_kmem_stats_t kmem_stats;
      status = zx_object_get_info(info_resource_->get(), ZX_INFO_KMEM_STATS, &kmem_stats,
                                  sizeof(kmem_stats), nullptr, nullptr);
      if (status == ZX_OK) {
        DRIVER_ERROR(
            "kmem stats: total_bytes: 0x%lx free_bytes 0x%lx: wired_bytes: 0x%lx vmo_bytes: 0x%lx\n"
            "mmu_overhead_bytes: 0x%lx other_bytes: 0x%lx",
            kmem_stats.total_bytes, kmem_stats.free_bytes, kmem_stats.wired_bytes,
            kmem_stats.vmo_bytes, kmem_stats.mmu_overhead_bytes, kmem_stats.other_bytes);
      }
      // sanitize to ZX_ERR_NO_MEMORY regardless of why.
      status = ZX_ERR_NO_MEMORY;
      return status;
    }
    constexpr const char vmo_name[] = "Sysmem-contig-core";
    result_parent_vmo.set_property(ZX_PROP_NAME, vmo_name, sizeof(vmo_name));
    *parent_vmo = std::move(result_parent_vmo);
    return ZX_OK;
  }
  void Delete(zx::vmo parent_vmo) override {
    // ~vmo
  }
  // Since this allocator only allocates independent VMOs, it's fine to orphan those VMOs from the
  // allocator since the VMOs independently track what pages they're using.  So this allocator can
  // always claim is_empty() true.
  bool is_empty() override { return true; }

 private:
  Owner* const parent_device_;
  zx::unowned_resource info_resource_;
  inspect::Node node_;
  inspect::ValueList properties_;
};

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> CloneDirectoryClient(
    fidl::UnownedClientEnd<fuchsia_io::Directory> dir_client) {
  auto clone_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (clone_endpoints.is_error()) {
    LOG(ERROR, "CreateEndpoints failed: %s", clone_endpoints.status_string());
    return zx::error(clone_endpoints.status_value());
  }
  fuchsia_io::Node1CloneRequest clone_request;
  clone_request.flags() = fuchsia_io::OpenFlags::kCloneSameRights;
  clone_request.object() = fidl::ServerEnd<fuchsia_io::Node>(clone_endpoints->server.TakeChannel());
  auto clone_result = fidl::Call(dir_client)->Clone(std::move(clone_request));
  if (clone_result.is_error()) {
    LOG(ERROR, "Clone failed: %s", clone_result.error_value().status_string());
    return zx::error(clone_result.error_value().status());
  }
  return zx::ok(std::move(clone_endpoints->client));
}

}  // namespace

Device::Device(zx_device_t* parent_device, Driver* parent_driver)
    : DdkDeviceType(parent_device),
      parent_driver_(parent_driver),
      loop_(&kAsyncLoopConfigNeverAttachToThread) {
  ZX_DEBUG_ASSERT(parent_);
  ZX_DEBUG_ASSERT(parent_driver_);
  zx_status_t status = loop_.StartThread("sysmem", &loop_thrd_);
  ZX_ASSERT(status == ZX_OK);

  const char* role_name = "fuchsia.devices.sysmem.drivers.sysmem.device";
  const zx_status_t role_status = device_set_profile_by_role(
      parent_device, thrd_get_zx_handle(loop_thrd_), role_name, strlen(role_name));
  if (role_status != ZX_OK) {
    LOG(WARNING,
        "Failed to set loop thread role to \"%s\": %s. Thread will run at default priority.",
        role_name, zx_status_get_string(role_status));
  }

  // Up until DdkAdd, all access to member variables must happen on this thread.
  loop_checker_.emplace(fit::thread_checker());
}

Device::~Device() {
  if (loop_.GetState() != ASYNC_LOOP_RUNNABLE) {
    // This is the normal case.
    return;
  }

  // In unit tests, Device::Bind() may not have been run, so ensure the loop_checker_ has been moved
  // to the loop_ thread before DdkUnbindInternal() below which expects to use the loop_ thread.
  libsync::Completion completion;
  postTask([this, &completion] {
    ZX_ASSERT(loop_checker_.has_value());
    // After this point, all operations must happen on the loop thread.
    loop_checker_.emplace(fit::thread_checker());
    completion.Signal();
  });
  completion.Wait();

  // This may happen from tests that may not call DdkUnbind() first.
  //
  // TODO(b/332631101): Consider changing the tests to call DdkUnbind, DdkRelease, instead of just
  // ~Device.
  //
  // Shouldn't be seen outside of tests:
  LOG(ERROR, "Device::~Device() called without DdkUnbind() first (MockDdk test?)");
  DdkUnbindInternal();
}

zx_status_t Device::OverrideSizeFromCommandLine(const char* name, int64_t* memory_size) {
  char pool_arg[32];
  auto status = device_get_variable(parent(), name, pool_arg, sizeof(pool_arg), nullptr);
  if (status != ZX_OK || strlen(pool_arg) == 0)
    return ZX_OK;
  char* end = nullptr;
  int64_t override_size = strtoll(pool_arg, &end, 10);
  // Check that entire string was used and there isn't garbage at the end.
  if (*end != '\0') {
    DRIVER_ERROR("Ignoring flag %s with invalid size \"%s\"", name, pool_arg);
    return ZX_ERR_INVALID_ARGS;
  }
  DRIVER_INFO("Flag %s overriding size to %ld", name, override_size);
  if (override_size < -99) {
    DRIVER_ERROR("Flag %s specified too-large percentage: %" PRId64, name, -override_size);
    return ZX_ERR_INVALID_ARGS;
  }
  *memory_size = override_size;
  return ZX_OK;
}

zx::result<std::string> Device::GetFromCommandLine(const char* name) {
  char arg[32];
  auto status = device_get_variable(parent(), name, arg, sizeof(arg), nullptr);
  if (status == ZX_ERR_NOT_FOUND) {
    return zx::error(status);
  }
  if (status != ZX_OK) {
    LOG(ERROR, "device_get_variable() failed - status: %d", status);
    return zx::error(status);
  }
  if (strlen(arg) == 0) {
    LOG(ERROR, "strlen(arg) == 0");
    return zx::error(ZX_ERR_INTERNAL);
  }
  return zx::ok(std::string(arg, strlen(arg)));
}

zx::result<bool> Device::GetBoolFromCommandLine(const char* name, bool default_value) {
  auto result = GetFromCommandLine(name);
  if (!result.is_ok()) {
    if (result.error_value() == ZX_ERR_NOT_FOUND) {
      return zx::ok(default_value);
    }
    return zx::error(result.error_value());
  }
  std::string str = *result;
  std::transform(str.begin(), str.end(), str.begin(), ::tolower);
  if (str == "true" || str == "1") {
    return zx::ok(true);
  }
  if (str == "false" || str == "0") {
    return zx::ok(false);
  }
  return zx::error(ZX_ERR_INVALID_ARGS);
}

zx_status_t Device::GetContiguousGuardParameters(uint64_t* guard_bytes_out,
                                                 bool* unused_pages_guarded,
                                                 zx::duration* unused_page_check_cycle_period,
                                                 bool* internal_guard_pages_out,
                                                 bool* crash_on_fail_out) {
  const uint64_t kDefaultGuardBytes = zx_system_get_page_size();
  *guard_bytes_out = kDefaultGuardBytes;
  *unused_pages_guarded = true;
  *unused_page_check_cycle_period =
      ContiguousPooledMemoryAllocator::kDefaultUnusedPageCheckCyclePeriod;
  *internal_guard_pages_out = false;
  *crash_on_fail_out = false;

  // If true, sysmem crashes on a guard page violation.
  char arg[32];
  if (ZX_OK == device_get_variable(parent(), "driver.sysmem.contiguous_guard_pages_fatal", arg,
                                   sizeof(arg), nullptr)) {
    DRIVER_INFO("Setting contiguous_guard_pages_fatal");
    *crash_on_fail_out = true;
  }

  // If true, sysmem will create guard regions around every allocation.
  if (ZX_OK == device_get_variable(parent(), "driver.sysmem.contiguous_guard_pages_internal", arg,
                                   sizeof(arg), nullptr)) {
    DRIVER_INFO("Setting contiguous_guard_pages_internal");
    *internal_guard_pages_out = true;
  }

  // If true, sysmem will _not_ treat currently-unused pages as guard pages.  We flip the sense on
  // this one because we want the default to be enabled so we discover any issues with
  // DMA-write-after-free by default, and because this mechanism doesn't cost any pages.
  if (ZX_OK == device_get_variable(parent(), "driver.sysmem.contiguous_guard_pages_unused_disabled",
                                   arg, sizeof(arg), nullptr)) {
    DRIVER_INFO("Clearing unused_pages_guarded");
    *unused_pages_guarded = false;
  }

  const char* kUnusedPageCheckCyclePeriodName =
      "driver.sysmem.contiguous_guard_pages_unused_cycle_seconds";
  char unused_page_check_cycle_period_seconds_string[32];
  auto status = device_get_variable(parent(), kUnusedPageCheckCyclePeriodName,
                                    unused_page_check_cycle_period_seconds_string,
                                    sizeof(unused_page_check_cycle_period_seconds_string), nullptr);
  if (status == ZX_OK && strlen(unused_page_check_cycle_period_seconds_string)) {
    char* end = nullptr;
    int64_t potential_cycle_period_seconds =
        strtoll(unused_page_check_cycle_period_seconds_string, &end, 10);
    if (*end != '\0') {
      DRIVER_ERROR("Flag %s has invalid value \"%s\"", kUnusedPageCheckCyclePeriodName,
                   unused_page_check_cycle_period_seconds_string);
      return ZX_ERR_INVALID_ARGS;
    }
    DRIVER_INFO("Flag %s setting unused page check period to %ld seconds",
                kUnusedPageCheckCyclePeriodName, potential_cycle_period_seconds);
    *unused_page_check_cycle_period = zx::sec(potential_cycle_period_seconds);
  }

  const char* kGuardBytesName = "driver.sysmem.contiguous_guard_page_count";
  char guard_count[32];
  status =
      device_get_variable(parent(), kGuardBytesName, guard_count, sizeof(guard_count), nullptr);
  if (status == ZX_OK && strlen(guard_count)) {
    char* end = nullptr;
    int64_t page_count = strtoll(guard_count, &end, 10);
    // Check that entire string was used and there isn't garbage at the end.
    if (*end != '\0') {
      DRIVER_ERROR("Flag %s has invalid value \"%s\"", kGuardBytesName, guard_count);
      return ZX_ERR_INVALID_ARGS;
    }
    DRIVER_INFO("Flag %s setting guard page count to %ld", kGuardBytesName, page_count);
    *guard_bytes_out = zx_system_get_page_size() * page_count;
  }

  return ZX_OK;
}

void Device::DdkUnbindInternal() {
  // Try to ensure there are no outstanding VMOS before shutting down the loop.
  postTask([this]() mutable {
    ZX_ASSERT(loop_checker_.has_value());
    std::lock_guard checker(*loop_checker_);
    outgoing_.reset();
    waiting_for_unbind_ = true;
    CheckForUnbind();
  });

  // JoinThreads waits for the Quit() in CheckForUnbind to execute and cause the thread to exit. We
  // could instead try to asynchronously do these operations on another thread, but the display unit
  // tests don't have a way to wait for the unbind to be complete before tearing down the device.
  loop_.JoinThreads();
  loop_.Shutdown();

  // After this point the FIDL servers should have been shutdown and all DDK and other protocol
  // methods will error out because posting tasks to the dispatcher fails.
  LOG(DEBUG, "Finished DdkUnbindInternal.");
}

void Device::DdkUnbind(ddk::UnbindTxn txn) {
  DdkUnbindInternal();
  txn.Reply();
  LOG(DEBUG, "Finished DdkUnbind.");
}

void Device::CheckForUnbind() {
  std::lock_guard checker(*loop_checker_);
  if (!waiting_for_unbind_) {
    return;
  }
  if (!logical_buffer_collections().empty()) {
    zxlogf(INFO, "Not unbinding because there are logical buffer collections count %ld",
           logical_buffer_collections().size());
    return;
  }
  if (!!contiguous_system_ram_allocator_ && !contiguous_system_ram_allocator_->is_empty()) {
    zxlogf(INFO, "Not unbinding because contiguous system ram allocator is not empty");
    return;
  }
  for (auto& [heap, allocator] : allocators_) {
    if (!allocator->is_empty()) {
      zxlogf(INFO, "Not unbinding because allocator %s is not empty",
             heap.heap_type().value().c_str());

      return;
    }
  }

  // This will cause the loop to exit and will allow DdkUnbind to continue.
  loop_.Quit();
}

SysmemMetrics& Device::metrics() { return metrics_; }

protected_ranges::ProtectedRangesCoreControl& Device::protected_ranges_core_control(
    const fuchsia_sysmem2::Heap& heap) {
  std::lock_guard checker(*loop_checker_);
  auto iter = secure_mem_controls_.find(heap);
  ZX_DEBUG_ASSERT(iter != secure_mem_controls_.end());
  return iter->second;
}

bool Device::SecureMemControl::IsDynamic() { return is_dynamic; }

uint64_t Device::SecureMemControl::GetRangeGranularity() { return range_granularity; }

uint64_t Device::SecureMemControl::MaxRangeCount() { return max_range_count; }

bool Device::SecureMemControl::HasModProtectedRange() { return has_mod_protected_range; }

void Device::SecureMemControl::AddProtectedRange(const protected_ranges::Range& range) {
  std::lock_guard checker(*parent->loop_checker_);
  ZX_DEBUG_ASSERT(parent->secure_mem_);
  fuchsia_sysmem::SecureHeapAndRange secure_heap_and_range;
  secure_heap_and_range.heap().emplace(v1_heap_type);
  fuchsia_sysmem::SecureHeapRange secure_heap_range;
  secure_heap_range.physical_address().emplace(range.begin());
  secure_heap_range.size_bytes().emplace(range.length());
  secure_heap_and_range.range().emplace(std::move(secure_heap_range));
  fidl::Arena arena;
  auto wire_secure_heap_and_range = fidl::ToWire(arena, std::move(secure_heap_and_range));
  auto result =
      parent->secure_mem_->channel()->AddSecureHeapPhysicalRange(wire_secure_heap_and_range);
  // If we lose the ability to control protected memory ranges ... reboot.
  ZX_ASSERT(result.ok());
  if (result->is_error()) {
    LOG(ERROR, "AddSecureHeapPhysicalRange() failed - status: %d", result->error_value());
  }
  ZX_ASSERT(!result->is_error());
}

void Device::SecureMemControl::DelProtectedRange(const protected_ranges::Range& range) {
  std::lock_guard checker(*parent->loop_checker_);
  ZX_DEBUG_ASSERT(parent->secure_mem_);
  fuchsia_sysmem::SecureHeapAndRange secure_heap_and_range;
  secure_heap_and_range.heap().emplace(v1_heap_type);
  fuchsia_sysmem::SecureHeapRange secure_heap_range;
  secure_heap_range.physical_address().emplace(range.begin());
  secure_heap_range.size_bytes().emplace(range.length());
  secure_heap_and_range.range().emplace(std::move(secure_heap_range));
  fidl::Arena arena;
  auto wire_secure_heap_and_range = fidl::ToWire(arena, std::move(secure_heap_and_range));
  auto result =
      parent->secure_mem_->channel()->DeleteSecureHeapPhysicalRange(wire_secure_heap_and_range);
  // If we lose the ability to control protected memory ranges ... reboot.
  ZX_ASSERT(result.ok());
  if (result->is_error()) {
    LOG(ERROR, "DeleteSecureHeapPhysicalRange() failed - status: %d", result->error_value());
  }
  ZX_ASSERT(!result->is_error());
}

void Device::SecureMemControl::ModProtectedRange(const protected_ranges::Range& old_range,
                                                 const protected_ranges::Range& new_range) {
  if (new_range.end() != old_range.end() && new_range.begin() != old_range.begin()) {
    LOG(INFO,
        "new_range.end(): %" PRIx64 " old_range.end(): %" PRIx64 " new_range.begin(): %" PRIx64
        " old_range.begin(): %" PRIx64,
        new_range.end(), old_range.end(), new_range.begin(), old_range.begin());
    ZX_PANIC("INVALID RANGE MODIFICATION");
  }

  std::lock_guard checker(*parent->loop_checker_);
  ZX_DEBUG_ASSERT(parent->secure_mem_);
  fuchsia_sysmem::SecureHeapAndRangeModification modification;
  modification.heap().emplace(v1_heap_type);
  fuchsia_sysmem::SecureHeapRange range_old;
  range_old.physical_address().emplace(old_range.begin());
  range_old.size_bytes().emplace(old_range.length());
  fuchsia_sysmem::SecureHeapRange range_new;
  range_new.physical_address().emplace(new_range.begin());
  range_new.size_bytes().emplace(new_range.length());
  modification.old_range().emplace(std::move(range_old));
  modification.new_range().emplace(std::move(range_new));
  fidl::Arena arena;
  auto wire_modification = fidl::ToWire(arena, std::move(modification));
  auto result = parent->secure_mem_->channel()->ModifySecureHeapPhysicalRange(wire_modification);
  // If we lose the ability to control protected memory ranges ... reboot.
  ZX_ASSERT(result.ok());
  if (result->is_error()) {
    LOG(ERROR, "ModifySecureHeapPhysicalRange() failed - status: %d", result->error_value());
  }
  ZX_ASSERT(!result->is_error());
}

void Device::SecureMemControl::ZeroProtectedSubRange(bool is_covering_range_explicit,
                                                     const protected_ranges::Range& range) {
  std::lock_guard checker(*parent->loop_checker_);
  ZX_DEBUG_ASSERT(parent->secure_mem_);
  fuchsia_sysmem::SecureHeapAndRange secure_heap_and_range;
  secure_heap_and_range.heap().emplace(v1_heap_type);
  fuchsia_sysmem::SecureHeapRange secure_heap_range;
  secure_heap_range.physical_address().emplace(range.begin());
  secure_heap_range.size_bytes().emplace(range.length());
  secure_heap_and_range.range().emplace(std::move(secure_heap_range));
  fidl::Arena arena;
  auto wire_secure_heap_and_range = fidl::ToWire(arena, std::move(secure_heap_and_range));
  auto result = parent->secure_mem_->channel()->ZeroSubRange(is_covering_range_explicit,
                                                             wire_secure_heap_and_range);
  // If we lose the ability to control protected memory ranges ... reboot.
  ZX_ASSERT(result.ok());
  if (result->is_error()) {
    LOG(ERROR, "ZeroSubRange() failed - status: %d", result->error_value());
  }
  ZX_ASSERT(!result->is_error());
}

zx_status_t Device::Bind(std::unique_ptr<Device> device) {
  zx_status_t status = device->Bind();

  if (status == ZX_OK) {
    // The device has bound successfully so it is owned by the DDK now. The delete later may occur
    // in DdkRelease.
    [[maybe_unused]] auto unused_device_ptr = device.release();
  } else {
    // ~device
  }

  return status;
}

zx_status_t Device::Bind() {
  std::lock_guard checker(*loop_checker_);

  // Put everything under a node called "sysmem" because there's currently there's not a simple way
  // to distinguish (using a selector) which driver inspect information is coming from.
  sysmem_root_ = inspector_.GetRoot().CreateChild("sysmem");
  heaps_ = sysmem_root_.CreateChild("heaps");
  collections_node_ = sysmem_root_.CreateChild("collections");

  auto pdev_client = DdkConnectFidlProtocol<fuchsia_hardware_platform_device::Service::Device>();
  if (pdev_client.is_error()) {
    DRIVER_ERROR(
        "Failed device_connect_fidl_protocol() for fuchsia.hardware.platform.device - status: %s",
        pdev_client.status_string());
    return pdev_client.status_value();
  }

  pdev_ = fidl::SyncClient(std::move(*pdev_client));

  int64_t protected_memory_size = kDefaultProtectedMemorySize;
  int64_t contiguous_memory_size = kDefaultContiguousMemorySize;

  size_t metadata_size = 0;
  zx_status_t status = DdkGetMetadataSize(fuchsia_hardware_sysmem::kMetadataType, &metadata_size);
  if (status == ZX_OK) {
    std::vector<uint8_t> raw_metadata(metadata_size);
    size_t metadata_actual = 0;
    status = DdkGetMetadata(fuchsia_hardware_sysmem::kMetadataType, raw_metadata.data(),
                            raw_metadata.size(), &metadata_actual);
    if (status == ZX_OK) {
      ZX_ASSERT(metadata_actual == metadata_size);
      auto unpersist_result =
          fidl::Unpersist<fuchsia_hardware_sysmem::Metadata>(cpp20::span(raw_metadata));
      if (unpersist_result.is_error()) {
        DRIVER_ERROR("Failed fidl::Unpersist - status: %s",
                     zx_status_get_string(unpersist_result.error_value().status()));
        return unpersist_result.error_value().status();
      }
      auto& metadata = unpersist_result.value();

      // Default is zero when field un-set.
      pdev_device_info_vid_ = metadata.vid().has_value() ? *metadata.vid() : 0;
      pdev_device_info_pid_ = metadata.pid().has_value() ? *metadata.pid() : 0;
      protected_memory_size =
          metadata.protected_memory_size().has_value() ? *metadata.protected_memory_size() : 0;
      contiguous_memory_size =
          metadata.contiguous_memory_size().has_value() ? *metadata.contiguous_memory_size() : 0;
    }
  }

  const char* kDisableDynamicRanges = "driver.sysmem.protected_ranges.disable_dynamic";
  zx::result<bool> protected_ranges_disable_dynamic =
      GetBoolFromCommandLine(kDisableDynamicRanges, false);
  if (protected_ranges_disable_dynamic.is_error()) {
    return protected_ranges_disable_dynamic.status_value();
  }
  cmdline_protected_ranges_disable_dynamic_ = protected_ranges_disable_dynamic.value();

  // TODO(b/333399746): Remove OverrideSizeFromCommandLine() calls once the kernel command line
  // arguments "driver.sysmem.protected_memory_size" and "driver.sysmem.contiguous_memory_size" are
  // no longer set in any Fuchsia build.
  status =
      OverrideSizeFromCommandLine("driver.sysmem.protected_memory_size", &protected_memory_size);
  if (status != ZX_OK) {
    // OverrideSizeFromCommandLine() already printed an error.
    return status;
  }
  status =
      OverrideSizeFromCommandLine("driver.sysmem.contiguous_memory_size", &contiguous_memory_size);
  if (status != ZX_OK) {
    // OverrideSizeFromCommandLine() already printed an error.
    return status;
  }

  zx_handle_t structured_config_vmo;
  status = device_get_config_vmo(parent_, &structured_config_vmo);
  if (status != ZX_OK) {
    DRIVER_ERROR("Failed to get config vmo: %s", zx_status_get_string(status));
    return status;
  }
  if (structured_config_vmo == ZX_HANDLE_INVALID) {
    DRIVER_DEBUG("Skipping config: config vmo handle does not exist");
  } else {
    auto config = sysmem_config::Config::CreateFromVmo(zx::vmo(structured_config_vmo));
    if (config.driver_sysmem_protected_memory_size_override() >= 0) {
      protected_memory_size = config.driver_sysmem_protected_memory_size_override();
    }
    if (config.driver_sysmem_contiguous_memory_size_override() >= 0) {
      contiguous_memory_size = config.driver_sysmem_contiguous_memory_size_override();
    }
  }

  // Negative values are interpreted as a percentage of physical RAM.
  if (protected_memory_size < 0) {
    protected_memory_size = -protected_memory_size;
    ZX_DEBUG_ASSERT(protected_memory_size >= 1 && protected_memory_size <= 99);
    protected_memory_size = zx_system_get_physmem() * protected_memory_size / 100;
  }
  if (contiguous_memory_size < 0) {
    contiguous_memory_size = -contiguous_memory_size;
    ZX_DEBUG_ASSERT(contiguous_memory_size >= 1 && contiguous_memory_size <= 99);
    contiguous_memory_size = zx_system_get_physmem() * contiguous_memory_size / 100;
  }

  constexpr int64_t kMinProtectedAlignment = 64 * 1024;
  assert(kMinProtectedAlignment % zx_system_get_page_size() == 0);
  protected_memory_size = AlignUp(protected_memory_size, kMinProtectedAlignment);
  contiguous_memory_size =
      AlignUp(contiguous_memory_size, safe_cast<int64_t>(zx_system_get_page_size()));

  auto heap = sysmem::MakeHeap(bind_fuchsia_sysmem_heap::HEAP_TYPE_SYSTEM_RAM, 0);
  allocators_[std::move(heap)] = std::make_unique<SystemRamMemoryAllocator>(this);

  auto result = pdev_.wire()->GetBtiById(0);
  if (!result.ok()) {
    DRIVER_ERROR("Transport error for PDev::GetBtiById() - status: %s", result.status_string());
    return result.status();
  }

  if (result->is_error()) {
    DRIVER_ERROR("Failed PDev::GetBtiById() - status: %s",
                 zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  bti_ = std::move(result->value()->bti);

  zx::bti bti_copy;
  status = bti_.duplicate(ZX_RIGHT_SAME_RIGHTS, &bti_copy);
  if (status != ZX_OK) {
    DRIVER_ERROR("BTI duplicate failed: %d", status);
    return status;
  }

  if (contiguous_memory_size) {
    constexpr bool kIsAlwaysCpuAccessible = true;
    constexpr bool kIsEverCpuAccessible = true;
    constexpr bool kIsReady = true;
    constexpr bool kCanBeTornDown = true;
    auto heap = sysmem::MakeHeap(bind_fuchsia_sysmem_heap::HEAP_TYPE_SYSTEM_RAM, 0);
    auto pooled_allocator = std::make_unique<ContiguousPooledMemoryAllocator>(
        this, "SysmemContiguousPool", &heaps_, std::move(heap), contiguous_memory_size,
        kIsAlwaysCpuAccessible, kIsEverCpuAccessible, kIsReady, kCanBeTornDown, loop_.dispatcher());
    if (pooled_allocator->Init() != ZX_OK) {
      DRIVER_ERROR("Contiguous system ram allocator initialization failed");
      return ZX_ERR_NO_MEMORY;
    }
    uint64_t guard_region_size;
    bool unused_pages_guarded;
    zx::duration unused_page_check_cycle_period;
    bool internal_guard_regions;
    bool crash_on_guard;
    if (GetContiguousGuardParameters(&guard_region_size, &unused_pages_guarded,
                                     &unused_page_check_cycle_period, &internal_guard_regions,
                                     &crash_on_guard) == ZX_OK) {
      pooled_allocator->InitGuardRegion(guard_region_size, unused_pages_guarded,
                                        unused_page_check_cycle_period, internal_guard_regions,
                                        crash_on_guard, loop_.dispatcher());
    }
    pooled_allocator->SetupUnusedPages();
    contiguous_system_ram_allocator_ = std::move(pooled_allocator);
  } else {
    contiguous_system_ram_allocator_ = std::make_unique<ContiguousSystemRamMemoryAllocator>(
        zx::unowned_resource(get_info_resource(parent())), this);
  }

  // TODO: Separate protected memory allocator into separate driver or library
  if (pdev_device_info_vid_ == PDEV_VID_AMLOGIC && protected_memory_size > 0) {
    constexpr bool kIsAlwaysCpuAccessible = false;
    constexpr bool kIsEverCpuAccessible = true;
    constexpr bool kIsReady = false;
    // We have no way to tear down secure memory.
    constexpr bool kCanBeTornDown = false;
    auto heap = sysmem::MakeHeap(bind_fuchsia_amlogic_platform_sysmem_heap::HEAP_TYPE_SECURE, 0);
    auto amlogic_allocator = std::make_unique<ContiguousPooledMemoryAllocator>(
        this, "SysmemAmlogicProtectedPool", &heaps_, heap, protected_memory_size,
        kIsAlwaysCpuAccessible, kIsEverCpuAccessible, kIsReady, kCanBeTornDown, loop_.dispatcher());
    // Request 64kB alignment because the hardware can only modify protections along 64kB
    // boundaries.
    status = amlogic_allocator->Init(16);
    if (status != ZX_OK) {
      DRIVER_ERROR("Failed to init allocator for amlogic protected memory: %d", status);
      return status;
    }
    // For !is_cpu_accessible_, we don't call amlogic_allocator->SetupUnusedPages() until the start
    // of set_ready().
    secure_allocators_[heap] = amlogic_allocator.get();
    allocators_[std::move(heap)] = std::move(amlogic_allocator);
  }

  // outgoing_ can only be created and used on loop_ thread, so go ahead and switch loop_checker_
  // to the loop_ thread
  libsync::Completion completion;
  postTask([this, &completion] {
    // After this point, all operations must happen on the loop thread.
    loop_checker_.emplace(fit::thread_checker());
    completion.Signal();
  });
  completion.Wait();

  auto service_dir_result = SetupOutgoingServiceDir();
  if (service_dir_result.is_error()) {
    DRIVER_ERROR("SetupOutgoingServiceDir failed: %s", service_dir_result.status_string());
    return service_dir_result.status_value();
  }
  auto outgoing_dir_client = std::move(service_dir_result.value());

  std::array offers = {
      fuchsia_hardware_sysmem::Service::Name,
  };
  status = DdkAdd(ddk::DeviceAddArgs("sysmem")
                      .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                      .set_inspect_vmo(inspector_.DuplicateVmo())
                      .set_fidl_service_offers(offers)
                      .set_outgoing_dir(outgoing_dir_client.TakeChannel()));
  if (status != ZX_OK) {
    DRIVER_ERROR("Failed to bind device");
    return status;
  }

  if constexpr (kLogAllCollectionsPeriodically) {
    ZX_ASSERT(ZX_OK ==
              log_all_collections_.PostDelayed(loop_.dispatcher(), kLogAllCollectionsInterval));
  }

  zxlogf(INFO, "sysmem finished initialization");

  return ZX_OK;
}

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> Device::SetupOutgoingServiceDir() {
  auto service_dir_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (service_dir_endpoints.is_error()) {
    DRIVER_ERROR("fidl::CreateEndpoints failed: %s", service_dir_endpoints.status_string());
    return zx::error(service_dir_endpoints.status_value());
  }

  // We currently wait on this part to plumb the status from potential failure of AddService or
  // Serve. If in future the comments on those two are explicitly clarified to promise that they can
  // only fail if inputs are bad (including promsing that into the future), then we could let this
  // part be async and not plumb status back from the loop_ thread, instead just ZX_ASSERT'ing
  // ZX_OK on the loop_ thread.
  std::optional<zx_status_t> service_dir_status;
  RunSyncOnLoop(
      [this, server = std::move(service_dir_endpoints->server), &service_dir_status]() mutable {
        std::lock_guard<fit::thread_checker> thread_checker(*loop_checker_);

        outgoing_.emplace(dispatcher());
        auto outgoing_result = outgoing_->AddService<fuchsia_hardware_sysmem::Service>(
            fuchsia_hardware_sysmem::Service::InstanceHandler({
                .sysmem = bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure),
                .allocator_v1 =
                    [this](fidl::ServerEnd<fuchsia_sysmem::Allocator> request) {
                      zx_status_t status = CommonSysmemConnectV1(request.TakeChannel());
                      if (status != ZX_OK) {
                        LOG(INFO, "Direct connect to fuchsia_sysmem::Allocator() failed");
                      }
                    },
                .allocator_v2 =
                    [this](fidl::ServerEnd<fuchsia_sysmem2::Allocator> request) {
                      zx_status_t status = CommonSysmemConnectV2(request.TakeChannel());
                      if (status != ZX_OK) {
                        LOG(INFO, "Direct connect to fuchsia_sysmem2::Allocator() failed");
                      }
                    },
            }));

        if (outgoing_result.is_error()) {
          zxlogf(ERROR, "failed to add FIDL protocol to the outgoing directory (sysmem): %s",
                 outgoing_result.status_string());
          service_dir_status = outgoing_result.status_value();
          return;
        }

        outgoing_result = outgoing_->Serve(std::move(server));
        if (outgoing_result.is_error()) {
          zxlogf(ERROR, "failed to serve outgoing directory (sysmem): %s",
                 outgoing_result.status_string());
          service_dir_status = outgoing_result.status_value();
          return;
        }
        service_dir_status = ZX_OK;
      });
  ZX_ASSERT(service_dir_status.has_value());
  if (service_dir_status.value() != ZX_OK) {
    return zx::error(service_dir_status.value());
  }

  // We stash a client_end to the outgoing directory (in outgoing_dir_client_for_tests_) for later
  // use by fake-display-stack, at least until MockDdk supports the outgoing dir (as of this
  // comment, doesn't appear to so far).
  auto outgoing_dir_client = std::move(service_dir_endpoints->client);
  auto clone_result = CloneDirectoryClient(outgoing_dir_client);
  if (clone_result.is_error()) {
    DRIVER_ERROR("CloneDirectoryClient failed: %s", clone_result.status_string());
    return zx::error(clone_result.status_value());
  }
  outgoing_dir_client_for_tests_ = std::move(clone_result.value());

  return zx::ok(std::move(outgoing_dir_client));
}

void Device::ConnectV1(ConnectV1RequestView request, ConnectV1Completer::Sync& completer) {
  postTask([this, allocator_request = std::move(request->allocator_request)]() mutable {
    // The Allocator is channel-owned / self-owned.
    Allocator::CreateChannelOwnedV1(allocator_request.TakeChannel(), this);
  });
}

void Device::ConnectV2(ConnectV2RequestView request, ConnectV2Completer::Sync& completer) {
  postTask([this, allocator_request = std::move(request->allocator_request)]() mutable {
    // The Allocator is channel-owned / self-owned.
    Allocator::CreateChannelOwnedV2(allocator_request.TakeChannel(), this);
  });
}

void Device::SetAuxServiceDirectory(SetAuxServiceDirectoryRequestView request,
                                    SetAuxServiceDirectoryCompleter::Sync& completer) {
  postTask([this, aux_service_directory = std::make_shared<sys::ServiceDirectory>(
                      request->service_directory.TakeChannel())] {
    // Should the need arise in future, it'd be fine to stash a shared_ptr<aux_service_directory>
    // here if we need it for anything else.  For now we only need it for metrics.
    metrics_.metrics_buffer().SetServiceDirectory(aux_service_directory);
    metrics_.LogUnusedPageCheck(sysmem_metrics::UnusedPageCheckMetricDimensionEvent_Connectivity);
  });
}

zx_status_t Device::CommonSysmemConnectV1(zx::channel allocator_request) {
  // The Allocator is channel-owned / self-owned.
  postTask([this, allocator_request = std::move(allocator_request)]() mutable {
    // The Allocator is channel-owned / self-owned.
    Allocator::CreateChannelOwnedV1(std::move(allocator_request), this);
  });
  return ZX_OK;
}

zx_status_t Device::CommonSysmemConnectV2(zx::channel allocator_request) {
  // The Allocator is channel-owned / self-owned.
  postTask([this, allocator_request = std::move(allocator_request)]() mutable {
    // The Allocator is channel-owned / self-owned.
    Allocator::CreateChannelOwnedV2(std::move(allocator_request), this);
  });
  return ZX_OK;
}

zx_status_t Device::CommonSysmemRegisterHeap(
    fuchsia_sysmem2::Heap heap, fidl::ClientEnd<fuchsia_hardware_sysmem::Heap> heap_connection) {
  class EventHandler : public fidl::WireAsyncEventHandler<fuchsia_hardware_sysmem::Heap> {
   public:
    void OnRegister(
        ::fidl::WireEvent<::fuchsia_hardware_sysmem::Heap::OnRegister>* event) override {
      auto properties = fidl::ToNatural(event->properties);
      std::lock_guard checker(*device_->loop_checker_);
      // A heap should not be registered twice.
      ZX_DEBUG_ASSERT(heap_client_.is_valid());
      // This replaces any previously registered allocator for heap. This
      // behavior is preferred as it avoids a potential race-condition during
      // heap restart.
      auto allocator = std::make_shared<ExternalMemoryAllocator>(device_, std::move(heap_client_),
                                                                 std::move(properties));
      weak_associated_allocator_ = allocator;
      device_->allocators_[heap_] = std::move(allocator);
    }

    void on_fidl_error(fidl::UnbindInfo info) override {
      if (!info.is_peer_closed()) {
        DRIVER_ERROR("Heap failed: %s\n", info.FormatDescription().c_str());
      }
    }

    // Clean up heap allocator after |heap_client_| tears down, but only if the
    // heap allocator for this |heap_| is still associated with this handler via
    // |weak_associated_allocator_|.
    ~EventHandler() override {
      std::lock_guard checker(*device_->loop_checker_);
      auto existing = device_->allocators_.find(heap_);
      if (existing != device_->allocators_.end() &&
          existing->second == weak_associated_allocator_.lock())
        device_->allocators_.erase(heap_);
    }

    static void Bind(Device* device, fidl::ClientEnd<fuchsia_hardware_sysmem::Heap> heap_client_end,
                     fuchsia_sysmem2::Heap heap) {
      auto event_handler = std::unique_ptr<EventHandler>(new EventHandler(device, heap));
      event_handler->heap_client_.Bind(std::move(heap_client_end), device->dispatcher(),
                                       std::move(event_handler));
    }

   private:
    EventHandler(Device* device, fuchsia_sysmem2::Heap heap)
        : device_(device), heap_(std::move(heap)) {}

    Device* const device_;
    fidl::WireSharedClient<fuchsia_hardware_sysmem::Heap> heap_client_;
    const fuchsia_sysmem2::Heap heap_;
    std::weak_ptr<ExternalMemoryAllocator> weak_associated_allocator_;
  };

  postTask([this, heap = std::move(heap), heap_connection = std::move(heap_connection)]() mutable {
    std::lock_guard checker(*loop_checker_);
    EventHandler::Bind(this, std::move(heap_connection), std::move(heap));
  });
  return ZX_OK;
}

zx_status_t Device::CommonSysmemRegisterSecureMem(
    fidl::ClientEnd<fuchsia_sysmem::SecureMem> secure_mem_connection) {
  LOG(DEBUG, "sysmem RegisterSecureMem begin");

  current_close_is_abort_ = std::make_shared<std::atomic_bool>(true);

  postTask([this, secure_mem_connection = std::move(secure_mem_connection),
            close_is_abort = current_close_is_abort_]() mutable {
    std::lock_guard checker(*loop_checker_);
    // This code must run asynchronously for two reasons:
    // 1) It does synchronous IPCs to the secure mem device, so SysmemRegisterSecureMem must
    // have return so the call from the secure mem device is unblocked.
    // 2) It modifies member variables like |secure_mem_| and |heaps_| that should only be
    // touched on |loop_|'s thread.
    auto wait_for_close = std::make_unique<async::Wait>(
        secure_mem_connection.channel().get(), ZX_CHANNEL_PEER_CLOSED, 0,
        async::Wait::Handler([this, close_is_abort](async_dispatcher_t* dispatcher,
                                                    async::Wait* wait, zx_status_t status,
                                                    const zx_packet_signal_t* signal) {
          std::lock_guard checker(*loop_checker_);
          if (*close_is_abort && secure_mem_) {
            // The server end of this channel (the aml-securemem driver) is the driver that
            // listens for suspend(mexec) so that soft reboot can succeed.  If that driver has
            // failed, intentionally force a hard reboot here to get back to a known-good state.
            //
            // TODO(https://fxbug.dev/42180331): When there's any more direct/immediate way to
            // intentionally trigger a hard reboot, switch to that (or just remove this TODO
            // when sysmem terminating directly leads to a hard reboot).
            ZX_PANIC(
                "secure_mem_ connection unexpectedly lost; secure mem in unknown state; hard "
                "reboot");
          }
        }));

    // It is safe to call Begin() here before setting up secure_mem_ because handler will either
    // run on current thread (loop_thrd_), or be run after the current task finishes while the
    // loop is shutting down.
    zx_status_t status = wait_for_close->Begin(dispatcher());
    if (status != ZX_OK) {
      DRIVER_ERROR("Device::RegisterSecureMem() failed wait_for_close->Begin()");
      return;
    }

    secure_mem_ = std::make_unique<SecureMemConnection>(std::move(secure_mem_connection),
                                                        std::move(wait_for_close));

    // Else we already ZX_PANIC()ed in wait_for_close.
    ZX_DEBUG_ASSERT(secure_mem_);

    // At this point secure_allocators_ has only the secure heaps that are configured via sysmem
    // (not those configured via the TEE), and the memory for these is not yet protected.  Get
    // the SecureMem properties for these.  At some point in the future it _may_ make sense to
    // have connections to more than one SecureMem driver, but for now we assume that a single
    // SecureMem connection is handling all the secure heaps.  We don't actually protect any
    // range(s) until later.
    for (const auto& [heap, allocator] : secure_allocators_) {
      uint64_t phys_base;
      uint64_t size_bytes;
      zx_status_t get_status = allocator->GetPhysicalMemoryInfo(&phys_base, &size_bytes);
      if (get_status != ZX_OK) {
        LOG(WARNING, "get_status != ZX_OK - get_status: %d", get_status);
        return;
      }
      fuchsia_sysmem::SecureHeapAndRange whole_heap;
      // TODO(b/316646315): Switch GetPhysicalSecureHeapProperties to use fuchsia_sysmem2::Heap.
      auto v1_heap_type_result = sysmem::V1CopyFromV2HeapType(heap.heap_type().value());
      if (!v1_heap_type_result.is_ok()) {
        LOG(WARNING, "V1CopyFromV2HeapType failed");
        return;
      }
      auto v1_heap_type = v1_heap_type_result.value();
      whole_heap.heap().emplace(v1_heap_type);
      fuchsia_sysmem::SecureHeapRange range;
      range.physical_address().emplace(phys_base);
      range.size_bytes().emplace(size_bytes);
      whole_heap.range().emplace(std::move(range));
      fidl::Arena arena;
      auto wire_whole_heap = fidl::ToWire(arena, std::move(whole_heap));
      auto get_properties_result =
          secure_mem_->channel()->GetPhysicalSecureHeapProperties(wire_whole_heap);
      if (!get_properties_result.ok()) {
        ZX_ASSERT(!*close_is_abort);
        return;
      }
      if (get_properties_result->is_error()) {
        LOG(WARNING, "GetPhysicalSecureHeapProperties() failed - status: %d",
            get_properties_result->error_value());
        // Don't call set_ready() on secure_allocators_.  Eg. this can happen if securemem TA is
        // not found.
        return;
      }
      ZX_ASSERT(get_properties_result->is_ok());
      const fuchsia_sysmem::SecureHeapProperties& properties =
          fidl::ToNatural(get_properties_result->value()->properties);
      ZX_ASSERT(properties.heap().has_value());
      ZX_ASSERT(properties.heap().value() == v1_heap_type);
      ZX_ASSERT(properties.dynamic_protection_ranges().has_value());
      ZX_ASSERT(properties.protected_range_granularity().has_value());
      ZX_ASSERT(properties.max_protected_range_count().has_value());
      ZX_ASSERT(properties.is_mod_protected_range_available().has_value());
      SecureMemControl control;
      // intentional copy/clone
      control.heap = heap;
      control.v1_heap_type = v1_heap_type;
      control.parent = this;
      control.is_dynamic = properties.dynamic_protection_ranges().value();
      control.max_range_count = properties.max_protected_range_count().value();
      control.range_granularity = properties.protected_range_granularity().value();
      control.has_mod_protected_range = properties.is_mod_protected_range_available().value();
      // copy/clone the key (heap)
      secure_mem_controls_.emplace(heap, std::move(control));
    }

    // Now we get the secure heaps that are configured via the TEE.
    auto get_result = secure_mem_->channel()->GetPhysicalSecureHeaps();
    if (!get_result.ok()) {
      // For now this is fatal unless explicitly unregistered, since this case is very
      // unexpected, and in this case rebooting is the most plausible way to get back to a
      // working state anyway.
      ZX_ASSERT(!*close_is_abort);
      return;
    }
    if (get_result->is_error()) {
      LOG(WARNING, "GetPhysicalSecureHeaps() failed - status: %d", get_result->error_value());
      // Don't call set_ready() on secure_allocators_.  Eg. this can happen if securemem TA is
      // not found.
      return;
    }
    ZX_ASSERT(get_result->is_ok());
    const fuchsia_sysmem::SecureHeapsAndRanges& tee_configured_heaps =
        fidl::ToNatural(get_result->value()->heaps);
    ZX_ASSERT(tee_configured_heaps.heaps().has_value());
    ZX_ASSERT(tee_configured_heaps.heaps()->size() != 0);
    for (const auto& heap : *tee_configured_heaps.heaps()) {
      ZX_ASSERT(heap.heap().has_value());
      ZX_ASSERT(heap.ranges().has_value());
      // A tee-configured heap with multiple ranges can be specified by the protocol but is not
      // currently supported by sysmem.
      ZX_ASSERT(heap.ranges()->size() == 1);
      // For now we assume that all TEE-configured heaps are protected full-time, and that they
      // start fully protected.
      constexpr bool kIsAlwaysCpuAccessible = false;
      constexpr bool kIsEverCpuAccessible = false;
      constexpr bool kIsReady = false;
      constexpr bool kCanBeTornDown = true;
      const fuchsia_sysmem::SecureHeapRange& heap_range = heap.ranges()->at(0);
      auto v2_heap_type_result = sysmem::V2CopyFromV1HeapType(heap.heap().value());
      ZX_ASSERT(v2_heap_type_result.is_ok());
      auto& v2_heap_type = v2_heap_type_result.value();
      fuchsia_sysmem2::Heap v2_heap;
      v2_heap.heap_type() = std::move(v2_heap_type);
      v2_heap.id() = 0;
      auto secure_allocator = std::make_unique<ContiguousPooledMemoryAllocator>(
          this, "tee_secure", &heaps_, v2_heap, *heap_range.size_bytes(), kIsAlwaysCpuAccessible,
          kIsEverCpuAccessible, kIsReady, kCanBeTornDown, loop_.dispatcher());
      status = secure_allocator->InitPhysical(heap_range.physical_address().value());
      // A failing status is fatal for now.
      ZX_ASSERT(status == ZX_OK);
      LOG(DEBUG,
          "created secure allocator: heap_type: %08lx base: %016" PRIx64 " size: %016" PRIx64,
          safe_cast<uint64_t>(heap.heap().value()), heap_range.physical_address().value(),
          heap_range.size_bytes().value());

      auto v1_heap_type = heap.heap().value();

      // The only usage of SecureMemControl for a TEE-configured heap is to do ZeroSubRange(),
      // so field values of this SecureMemControl are somewhat degenerate (eg. VDEC).
      SecureMemControl control;
      control.heap = v2_heap;
      control.v1_heap_type = v1_heap_type;
      control.parent = this;
      control.is_dynamic = false;
      control.max_range_count = 0;
      control.range_granularity = 0;
      control.has_mod_protected_range = false;
      secure_mem_controls_.emplace(v2_heap, std::move(control));

      ZX_ASSERT(secure_allocators_.find(v2_heap) == secure_allocators_.end());
      secure_allocators_[v2_heap] = secure_allocator.get();
      ZX_ASSERT(allocators_.find(v2_heap) == allocators_.end());
      allocators_[std::move(v2_heap)] = std::move(secure_allocator);
    }

    for (const auto& [heap_type, allocator] : secure_allocators_) {
      // The secure_mem_ connection is ready to protect ranges on demand to cover this heap's
      // used ranges.  There are no used ranges yet since the heap wasn't ready until now.
      allocator->set_ready();
    }

    is_secure_mem_ready_ = true;

    // At least for now, we just call all the LogicalBufferCollection(s), regardless of which
    // are waiting on secure mem (if any). The extra calls are required (by semantics of
    // OnDependencyReady) to not be harmful from a correctness point of view. If any are waiting
    // on secure mem, those can now proceed, and will do so using the current thread (the loop_
    // thread).
    ForEachLogicalBufferCollection([](LogicalBufferCollection* logical_buffer_collection) {
      logical_buffer_collection->OnDependencyReady();
    });

    LOG(DEBUG, "sysmem RegisterSecureMem() done (async)");
  });
  return ZX_OK;
}

// This call allows us to tell the difference between expected vs. unexpected close of the tee_
// channel.
zx_status_t Device::CommonSysmemUnregisterSecureMem() {
  // By this point, the aml-securemem driver's suspend(mexec) has already prepared for mexec.
  //
  // In this path, the server end of the channel hasn't closed yet, but will be closed shortly after
  // return from UnregisterSecureMem().
  //
  // We set a flag here so that a PEER_CLOSED of the channel won't cause the wait handler to crash.
  *current_close_is_abort_ = false;
  current_close_is_abort_.reset();
  postTask([this]() {
    std::lock_guard checker(*loop_checker_);
    LOG(DEBUG, "begin UnregisterSecureMem()");
    secure_mem_.reset();
    LOG(DEBUG, "end UnregisterSecureMem()");
  });
  return ZX_OK;
}

const zx::bti& Device::bti() { return bti_; }

// Only use this in cases where we really can't use zx::vmo::create_contiguous() because we must
// specify a specific physical range.
zx_status_t Device::CreatePhysicalVmo(uint64_t base, uint64_t size, zx::vmo* vmo_out) {
  zx::vmo result_vmo;
  zx::unowned_resource resource(get_mmio_resource(parent()));
  zx_status_t status = zx::vmo::create_physical(*resource, base, size, &result_vmo);
  if (status != ZX_OK) {
    return status;
  }
  *vmo_out = std::move(result_vmo);
  return ZX_OK;
}

uint32_t Device::pdev_device_info_vid() {
  ZX_DEBUG_ASSERT(pdev_device_info_vid_ != std::numeric_limits<uint32_t>::max());
  return pdev_device_info_vid_;
}

uint32_t Device::pdev_device_info_pid() {
  ZX_DEBUG_ASSERT(pdev_device_info_pid_ != std::numeric_limits<uint32_t>::max());
  return pdev_device_info_pid_;
}

void Device::TrackToken(BufferCollectionToken* token) {
  std::lock_guard checker(*loop_checker_);
  ZX_DEBUG_ASSERT(token->has_server_koid());
  zx_koid_t server_koid = token->server_koid();
  ZX_DEBUG_ASSERT(server_koid != ZX_KOID_INVALID);
  ZX_DEBUG_ASSERT(tokens_by_koid_.find(server_koid) == tokens_by_koid_.end());
  tokens_by_koid_.insert({server_koid, token});
}

void Device::UntrackToken(BufferCollectionToken* token) {
  std::lock_guard checker(*loop_checker_);
  if (!token->has_server_koid()) {
    // The caller is allowed to un-track a token that never saw
    // OnServerKoid().
    return;
  }
  // This is intentionally idempotent, to allow un-tracking from
  // BufferCollectionToken::CloseChannel() as well as from
  // ~BufferCollectionToken().
  tokens_by_koid_.erase(token->server_koid());
}

bool Device::TryRemoveKoidFromUnfoundTokenList(zx_koid_t token_server_koid) {
  std::lock_guard checker(*loop_checker_);
  // unfound_token_koids_ is limited to kMaxUnfoundTokenCount (and likely empty), so a loop over it
  // should be efficient enough.
  for (auto it = unfound_token_koids_.begin(); it != unfound_token_koids_.end(); ++it) {
    if (*it == token_server_koid) {
      unfound_token_koids_.erase(it);
      return true;
    }
  }
  return false;
}

BufferCollectionToken* Device::FindTokenByServerChannelKoid(zx_koid_t token_server_koid) {
  std::lock_guard checker(*loop_checker_);
  auto iter = tokens_by_koid_.find(token_server_koid);
  if (iter == tokens_by_koid_.end()) {
    unfound_token_koids_.push_back(token_server_koid);
    constexpr uint32_t kMaxUnfoundTokenCount = 8;
    while (unfound_token_koids_.size() > kMaxUnfoundTokenCount) {
      unfound_token_koids_.pop_front();
    }
    return nullptr;
  }
  return iter->second;
}

Device::FindLogicalBufferByVmoKoidResult Device::FindLogicalBufferByVmoKoid(zx_koid_t vmo_koid) {
  auto iter = vmo_koids_.find(vmo_koid);
  if (iter == vmo_koids_.end()) {
    return {nullptr, false};
  }
  return iter->second;
}

MemoryAllocator* Device::GetAllocator(const fuchsia_sysmem2::BufferMemorySettings& settings) {
  std::lock_guard checker(*loop_checker_);
  if (*settings.heap()->heap_type() == bind_fuchsia_sysmem_heap::HEAP_TYPE_SYSTEM_RAM &&
      *settings.is_physically_contiguous()) {
    return contiguous_system_ram_allocator_.get();
  }

  auto iter = allocators_.find(*settings.heap());
  if (iter == allocators_.end()) {
    return nullptr;
  }
  return iter->second.get();
}

const fuchsia_hardware_sysmem::HeapProperties* Device::GetHeapProperties(
    const fuchsia_sysmem2::Heap& heap) const {
  std::lock_guard checker(*loop_checker_);
  auto iter = allocators_.find(heap);
  if (iter == allocators_.end()) {
    return nullptr;
  }
  return &iter->second->heap_properties();
}

void Device::AddVmoKoid(zx_koid_t koid, bool is_weak, LogicalBuffer& logical_buffer) {
  vmo_koids_.insert({koid, {&logical_buffer, is_weak}});
}

void Device::RemoveVmoKoid(zx_koid_t koid) {
  // May not be present if ~TrackedParentVmo called in error path prior to being fully set up.
  vmo_koids_.erase(koid);
}

void Device::LogAllBufferCollections() {
  std::lock_guard checker(*loop_checker_);
  IndentTracker indent_tracker(0);
  auto indent = indent_tracker.Current();
  LOG(INFO, "%*scollections.size: %" PRId64, indent.num_spaces(), "",
      logical_buffer_collections_.size());
  // We sort by create time to make it easier to figure out which VMOs are likely leaks, especially
  // when running with kLogAllCollectionsPeriodically true.
  std::vector<LogicalBufferCollection*> sort_by_create_time;
  ForEachLogicalBufferCollection(
      [&sort_by_create_time](LogicalBufferCollection* logical_buffer_collection) {
        sort_by_create_time.push_back(logical_buffer_collection);
      });
  struct CompareByCreateTime {
    bool operator()(const LogicalBufferCollection* a, const LogicalBufferCollection* b) {
      return a->create_time_monotonic() < b->create_time_monotonic();
    }
  };
  std::sort(sort_by_create_time.begin(), sort_by_create_time.end(), CompareByCreateTime{});
  for (auto* logical_buffer_collection : sort_by_create_time) {
    logical_buffer_collection->LogSummary(indent_tracker);
    // let output catch up so we don't drop log lines, hopefully; if there were a way to flush/wait
    // the logger to avoid dropped lines, we could do that instead
    zx::nanosleep(zx::deadline_after(zx::msec(20)));
  };
}

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> Device::CloneServiceDirClientForTests() {
  return CloneDirectoryClient(outgoing_dir_client_for_tests_);
}

Device::SecureMemConnection::SecureMemConnection(fidl::ClientEnd<fuchsia_sysmem::SecureMem> channel,
                                                 std::unique_ptr<async::Wait> wait_for_close)
    : connection_(std::move(channel)), wait_for_close_(std::move(wait_for_close)) {
  // nothing else to do here
}

const fidl::WireSyncClient<fuchsia_sysmem::SecureMem>& Device::SecureMemConnection::channel()
    const {
  ZX_DEBUG_ASSERT(connection_);
  return connection_;
}

void Device::LogCollectionsTimer(async_dispatcher_t* dispatcher, async::TaskBase* task,
                                 zx_status_t status) {
  std::lock_guard checker(*loop_checker_);
  LogAllBufferCollections();
  ZX_ASSERT(kLogAllCollectionsPeriodically);
  ZX_ASSERT(ZX_OK ==
            log_all_collections_.PostDelayed(loop_.dispatcher(), kLogAllCollectionsInterval));
}

void Device::RegisterHeap(RegisterHeapRequest& request, RegisterHeapCompleter::Sync& completer) {
  // TODO(b/316646315): Change RegisterHeap to specify fuchsia_sysmem2::Heap, and remove the
  // conversion here.
  auto v2_heap_type_result =
      sysmem::V2CopyFromV1HeapType(static_cast<fuchsia_sysmem::HeapType>(request.heap()));
  if (!v2_heap_type_result.is_ok()) {
    LOG(WARNING, "V2CopyFromV1HeapType failed");
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  auto& v2_heap_type = v2_heap_type_result.value();
  auto heap = sysmem::MakeHeap(std::move(v2_heap_type), 0);
  zx_status_t status =
      CommonSysmemRegisterHeap(std::move(heap), std::move(request.heap_connection()));
  if (status != ZX_OK) {
    LOG(WARNING, "CommonSysmemRegisterHeap failed");
    completer.Close(status);
    return;
  }
}

void Device::RegisterSecureMem(RegisterSecureMemRequest& request,
                               RegisterSecureMemCompleter::Sync& completer) {
  zx_status_t status = CommonSysmemRegisterSecureMem(std::move(request.secure_mem_connection()));
  if (status != ZX_OK) {
    completer.Close(status);
    return;
  }
}

void Device::UnregisterSecureMem(UnregisterSecureMemCompleter::Sync& completer) {
  zx_status_t status = CommonSysmemUnregisterSecureMem();
  if (status == ZX_OK) {
    completer.Reply(fit::ok());
  } else {
    completer.Reply(fit::error(status));
  }
}

}  // namespace sysmem_driver
