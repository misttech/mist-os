// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <inttypes.h>
#include <lib/async/dispatcher.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/fidl_driver/cpp/transport.h>
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

#include <bind/fuchsia/sysmem/heap/cpp/bind.h>

// TODO(b/42113093): Remove this include of AmLogic-specific heap names in sysmem code. The include
// is currently needed for secure heap names only, which is why an include for goldfish heap names
// isn't here.
#include <bind/fuchsia/amlogic/platform/sysmem/heap/cpp/bind.h>
#include <fbl/string_printf.h>
#include <sdk/lib/sys/cpp/service_directory.h>

#include "lib/async/cpp/task.h"
#include "src/devices/sysmem/drivers/sysmem/allocator.h"
#include "src/devices/sysmem/drivers/sysmem/buffer_collection_token.h"
#include "src/devices/sysmem/drivers/sysmem/contiguous_pooled_memory_allocator.h"
#include "src/devices/sysmem/drivers/sysmem/external_memory_allocator.h"
#include "src/devices/sysmem/drivers/sysmem/macros.h"
#include "src/devices/sysmem/drivers/sysmem/utils.h"
#include "src/devices/sysmem/metrics/metrics.cb.h"
#include "zircon/status.h"

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
  explicit ContiguousSystemRamMemoryAllocator(Owner* parent_device)
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
        parent_device_(parent_device) {
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
      LOG(ERROR, "zx::vmo::create_contiguous() failed - size_bytes: %" PRIu64 " status: %d", size,
          status);
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
  inspect::Node node_;
  inspect::ValueList properties_;
};

}  // namespace

Device::Device(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher incoming_driver_dispatcher)
    : fdf::DriverBase("sysmem-device", std::move(start_args),
                      std::move(incoming_driver_dispatcher)),
      driver_checker_(async::synchronization_checker(driver_dispatcher())) {
  LOG(DEBUG, "Device::Device");
  std::lock_guard checker(*driver_checker_);

  auto loop_result = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "sysmem-loop",
      [this](fdf_dispatcher_t*) { loop_shut_down_.Signal(); });
  ZX_ASSERT_MSG(loop_result.is_ok(), "%s", loop_result.status_string());
  loop_ = std::move(loop_result).value();

  RunSyncOnLoop(
      [this] { loop_checker_.emplace(async::synchronization_checker(loop_.async_dispatcher())); });
}

Device::~Device() {
  std::lock_guard checker(*driver_checker_);
  DdkUnbindInternal();
  LOG(DEBUG, "Finished DdkUnbindInternal");
}

zx_status_t Device::GetContiguousGuardParameters(const std::optional<sysmem_config::Config>& config,
                                                 uint64_t* guard_bytes_out,
                                                 bool* unused_pages_guarded,
                                                 int64_t* unused_guard_pattern_period_bytes,
                                                 zx::duration* unused_page_check_cycle_period,
                                                 bool* internal_guard_pages_out,
                                                 bool* crash_on_fail_out) {
  const uint64_t kDefaultGuardBytes = zx_system_get_page_size();
  *guard_bytes_out = kDefaultGuardBytes;
  *unused_page_check_cycle_period =
      ContiguousPooledMemoryAllocator::kDefaultUnusedPageCheckCyclePeriod;

  if (!config.has_value()) {
    *unused_pages_guarded = true;
    *internal_guard_pages_out = false;
    *crash_on_fail_out = false;
    *unused_guard_pattern_period_bytes = -1;
    return ZX_OK;
  }

  *crash_on_fail_out = config->contiguous_guard_pages_fatal();
  *internal_guard_pages_out = config->contiguous_guard_pages_internal();
  *unused_pages_guarded = config->contiguous_guard_pages_unused();
  // if this value is <= 0 it'll be ignored and the default of 1/128 will stay in effect
  *unused_guard_pattern_period_bytes =
      config->contiguous_guard_pages_unused_fraction_denominator() * zx_system_get_page_size();

  int64_t unused_page_check_cycle_seconds = config->contiguous_guard_pages_unused_cycle_seconds();
  if (unused_page_check_cycle_seconds > 0) {
    LOG(INFO, "Overriding unused page check period to %ld seconds",
        unused_page_check_cycle_seconds);
    *unused_page_check_cycle_period = zx::sec(unused_page_check_cycle_seconds);
  }

  int64_t guard_page_count = config->contiguous_guard_page_count();
  if (guard_page_count > 0) {
    LOG(INFO, "Overriding guard page count to %ld", guard_page_count);
    *guard_bytes_out = zx_system_get_page_size() * guard_page_count;
  }

  return ZX_OK;
}

void Device::DdkUnbindInternal() {
  ZX_DEBUG_ASSERT(fdf::Dispatcher::GetCurrent()->async_dispatcher() == driver_dispatcher());

  // stop other drivers from connecting
  zx::result<> remove_result = outgoing()->RemoveService<fuchsia_hardware_sysmem::Service>();
  if (!remove_result.is_ok()) {
    LOG(WARNING, "RemoveService failed: %s", remove_result.status_string());
    // keep going
  }

  // disconnect existing connections from drivers
  bindings_.RemoveAll();

  compat_server_.reset();

  // Try to ensure there are no outstanding VMOS before shutting down the loop.
  PostTask([this]() mutable {
    ZX_ASSERT(loop_checker_.has_value());
    std::lock_guard checker(*loop_checker_);

    // stop sysmem-connector from reconnecting (not that it'll try)
    devfs_connector_.RemoveAll();
    // disconnect sysmem-connector connection(s), preventing non-drivers from
    // connecting (drivers were prevented above)
    driver_connectors_.RemoveAll();

    // disconnect existing allocators
    v1_allocators_.RemoveAll();
    v2_allocators_.RemoveAll();

    snapshot_annotation_register_->UnsetServiceDirectory();
    snapshot_annotation_register_.reset();

    // Force-fail all LogicalBufferCollection(s) - this doesn't force them to all immediately
    // delete, but does get them headed toward deleting. The Fail() call may or may not delete a
    // LogicalBufferCollection synchronously, so copy the list before iterating.
    //
    // This disconnects all Node(s) (all BufferCollectionToken(s), BufferCollection(s),
    // BufferCollectionTokenGroup(s)).
    LogicalBufferCollections local_collections_list = logical_buffer_collections_;
    for (auto& collection : logical_buffer_collections_) {
      collection->Fail();
    }

    // Async notice when all still-existing LogicalBufferCollection(s) go away then shut down loop_.
    // This requires sysmem clients to close their remaining sysmem VMO handles to proceed async.
    //
    // If we decide in future that we want to be able to stop/delete the Device without requiring
    // client VMOs to close first, we'll want to ensure that the securemem protocol and securemem
    // drivers can avoid orphaning protected_memory_size, while also leaving buffers HW-protected
    // until clients are done with the buffers - currently not a real issue since both sysmem and
    // securemem are effectively start-only per boot (outside of unit tests).
    waiting_for_unbind_ = true;
    CheckForUnbind();
  });

  // If a test is stuck waiting here, ensure that the test is dropping its sysmem VMO handles before
  // ~Device.
  loop_shut_down_.Wait();

  LOG(DEBUG, "Finished DdkUnbindInternal.");
}

void Device::CheckForUnbind() {
  std::lock_guard checker(*loop_checker_);
  if (!waiting_for_unbind_) {
    return;
  }
  if (!logical_buffer_collections().empty()) {
    LOG(INFO, "Not unbinding because there are logical buffer collections count %ld",
        logical_buffer_collections().size());
    return;
  }
  if (!!contiguous_system_ram_allocator_ && !contiguous_system_ram_allocator_->is_empty()) {
    LOG(INFO, "Not unbinding because contiguous system ram allocator is not empty");
    return;
  }
  for (auto& [heap, allocator] : allocators_) {
    if (!allocator->is_empty()) {
      LOG(INFO, "Not unbinding because allocator %s is not empty",
          heap.heap_type().value().c_str());

      return;
    }
  }

  // This will cause the loop to exit and will allow DdkUnbind to continue.
  loop_.ShutdownAsync();
}

std::optional<SnapshotAnnotationRegister>& Device::snapshot_annotation_register() {
  return snapshot_annotation_register_;
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
  ZX_ASSERT_MSG(result.ok(), "AddSecureHeapPhysicalRange() failed: %s",
                result.FormatDescription().c_str());
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
  ZX_ASSERT_MSG(result.ok(), "DeleteSecureHeapPhysicalRange() failed: %s",
                result.FormatDescription().c_str());
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
  ZX_ASSERT_MSG(result.ok(), "ModifySecureHeapPhysicalRange() failed: %s",
                result.FormatDescription().c_str());
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
  ZX_ASSERT_MSG(result.ok(), "ZeroSubRange() failed: %s", result.FormatDescription().c_str());
}

zx::result<> Device::Start() {
  LOG(DEBUG, "SysmemDriver::Start()");
  std::lock_guard checker(*driver_checker_);

  zx_status_t status = Initialize();
  if (status != ZX_OK) {
    LOG(ERROR, "Initialize() failed: %s\n", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

zx_status_t Device::Initialize() {
  ZX_DEBUG_ASSERT(fdf::Dispatcher::GetCurrent()->async_dispatcher() == driver_dispatcher());

  // Put everything under a node called "sysmem" because there's currently there's not a simple way
  // to distinguish (using a selector) which driver inspect information is coming from.
  sysmem_root_ = inspector_.GetRoot().CreateChild("sysmem");
  heaps_ = sysmem_root_.CreateChild("heaps");
  collections_node_ = sysmem_root_.CreateChild("collections");

  zx::result<> compat_server_init_result =
      compat_server_.Initialize(incoming(), outgoing(), node_name(), name());
  if (compat_server_init_result.is_error()) {
    return compat_server_init_result.error_value();
  }

  zx::result<fidl::ClientEnd<fuchsia_hardware_platform_device::Device>> pdev_client_result =
      incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (pdev_client_result.is_error()) {
    LOG(ERROR,
        "Failed device_connect_fidl_protocol() for fuchsia.hardware.platform.device - status: %d",
        pdev_client_result.status_value());
    return pdev_client_result.status_value();
  }

  pdev_.Bind(std::move(*pdev_client_result));

  int64_t protected_memory_size = kDefaultProtectedMemorySize;
  int64_t contiguous_memory_size = kDefaultContiguousMemorySize;

  fidl::WireResult raw_metadata = pdev_->GetMetadata(fuchsia_hardware_sysmem::kMetadataType);
  if (!raw_metadata.ok()) {
    LOG(ERROR, "Failed to send GetMetadata request: %s", raw_metadata.status_string());
    return raw_metadata.status();
  }
  if (raw_metadata->is_error()) {
    if (raw_metadata->error_value() != ZX_ERR_NOT_FOUND) {
      return raw_metadata->error_value();
    }
    LOG(WARNING, "Metadata not found.");
  } else {
    auto unpersist_result =
        fidl::Unpersist<fuchsia_hardware_sysmem::Metadata>(raw_metadata->value()->metadata.get());
    if (unpersist_result.is_error()) {
      LOG(ERROR, "Failed fidl::Unpersist - status: %s",
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

  sysmem_config::Config config = take_config<sysmem_config::Config>();
  if (config.contiguous_memory_size() >= 0) {
    contiguous_memory_size = config.contiguous_memory_size();
  } else if (config.contiguous_memory_size_percent() >= 0 &&
             config.contiguous_memory_size_percent() <= 99) {
    // the negation is un-done below
    contiguous_memory_size = -config.contiguous_memory_size_percent();
  }
  if (config.protected_memory_size() >= 0) {
    protected_memory_size = config.protected_memory_size();
  } else if (config.protected_memory_size_percent() >= 0 &&
             config.protected_memory_size_percent() <= 99) {
    // the negation is un-done below
    protected_memory_size = -config.protected_memory_size_percent();
  }
  RunSyncOnLoop([this, &config] {
    std::lock_guard thread_checker(*loop_checker_);
    protected_ranges_disable_dynamic_ = config.protected_ranges_disable_dynamic();
  });

  // Negative values are interpreted as a percentage of physical RAM.
  if (contiguous_memory_size < 0) {
    contiguous_memory_size = -contiguous_memory_size;
    ZX_DEBUG_ASSERT(contiguous_memory_size >= 1 && contiguous_memory_size <= 99);
    contiguous_memory_size = zx_system_get_physmem() * contiguous_memory_size / 100;
  }
  if (protected_memory_size < 0) {
    protected_memory_size = -protected_memory_size;
    ZX_DEBUG_ASSERT(protected_memory_size >= 1 && protected_memory_size <= 99);
    protected_memory_size = zx_system_get_physmem() * protected_memory_size / 100;
  }

  constexpr int64_t kMinProtectedAlignment = 64 * 1024;
  assert(kMinProtectedAlignment % zx_system_get_page_size() == 0);
  contiguous_memory_size =
      AlignUp(contiguous_memory_size, safe_cast<int64_t>(zx_system_get_page_size()));
  protected_memory_size = AlignUp(protected_memory_size, kMinProtectedAlignment);

  auto heap = sysmem::MakeHeap(bind_fuchsia_sysmem_heap::HEAP_TYPE_SYSTEM_RAM, 0);
  RunSyncOnLoop([this, &heap] {
    std::lock_guard thread_checker(*loop_checker_);
    allocators_[std::move(heap)] = std::make_unique<SystemRamMemoryAllocator>(this);
    snapshot_annotation_register_.emplace(loop_dispatcher());
  });

  auto result = pdev_->GetBtiById(0);
  if (!result.ok()) {
    LOG(ERROR, "Transport error for PDev::GetBtiById() - status: %s", result.status_string());
    return result.status();
  }

  if (result->is_error()) {
    LOG(ERROR, "Failed PDev::GetBtiById() - status: %s",
        zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  bti_ = std::move(result->value()->bti);

  zx::bti bti_copy;
  zx_status_t status = bti_.duplicate(ZX_RIGHT_SAME_RIGHTS, &bti_copy);
  if (status != ZX_OK) {
    LOG(ERROR, "BTI duplicate failed: %d", status);
    return status;
  }

  if (contiguous_memory_size) {
    constexpr bool kIsAlwaysCpuAccessible = true;
    constexpr bool kIsEverCpuAccessible = true;
    constexpr bool kIsEverZirconAccessible = true;
    constexpr bool kIsReady = true;
    constexpr bool kCanBeTornDown = true;
    auto heap = sysmem::MakeHeap(bind_fuchsia_sysmem_heap::HEAP_TYPE_SYSTEM_RAM, 0);
    auto pooled_allocator = std::make_unique<ContiguousPooledMemoryAllocator>(
        this, "SysmemContiguousPool", &heaps_, std::move(heap), contiguous_memory_size,
        kIsAlwaysCpuAccessible, kIsEverCpuAccessible, kIsEverZirconAccessible, kIsReady,
        kCanBeTornDown, loop_dispatcher());
    if (pooled_allocator->Init() != ZX_OK) {
      LOG(ERROR, "Contiguous system ram allocator initialization failed");
      return ZX_ERR_NO_MEMORY;
    }
    uint64_t guard_region_size;
    bool unused_pages_guarded;
    int64_t unused_guard_pattern_period_bytes;
    zx::duration unused_page_check_cycle_period;
    bool internal_guard_regions;
    bool crash_on_guard;
    if (GetContiguousGuardParameters(
            config, &guard_region_size, &unused_pages_guarded, &unused_guard_pattern_period_bytes,
            &unused_page_check_cycle_period, &internal_guard_regions, &crash_on_guard) == ZX_OK) {
      pooled_allocator->InitGuardRegion(guard_region_size, unused_pages_guarded,
                                        unused_guard_pattern_period_bytes,
                                        unused_page_check_cycle_period, internal_guard_regions,
                                        crash_on_guard, loop_dispatcher());
    }
    pooled_allocator->SetupUnusedPages();
    RunSyncOnLoop([this, &pooled_allocator] {
      std::lock_guard thread_checker(*loop_checker_);
      contiguous_system_ram_allocator_ = std::move(pooled_allocator);
    });
  } else {
    RunSyncOnLoop([this] {
      std::lock_guard thread_checker(*loop_checker_);
      contiguous_system_ram_allocator_ = std::make_unique<ContiguousSystemRamMemoryAllocator>(this);
    });
  }

  // TODO: Separate protected memory allocator into separate driver or library
  if (pdev_device_info_vid_ == PDEV_VID_AMLOGIC && protected_memory_size > 0) {
    constexpr bool kIsAlwaysCpuAccessible = false;
    constexpr bool kIsEverCpuAccessible = true;
    constexpr bool kIsEverZirconAccessible = true;
    constexpr bool kIsReady = false;
    // We have no way to tear down secure memory.
    constexpr bool kCanBeTornDown = false;
    auto heap = sysmem::MakeHeap(bind_fuchsia_amlogic_platform_sysmem_heap::HEAP_TYPE_SECURE, 0);
    auto amlogic_allocator = std::make_unique<ContiguousPooledMemoryAllocator>(
        this, "SysmemAmlogicProtectedPool", &heaps_, heap, protected_memory_size,
        kIsAlwaysCpuAccessible, kIsEverCpuAccessible, kIsEverZirconAccessible, kIsReady,
        kCanBeTornDown, loop_dispatcher());
    // Request 64kB alignment because the hardware can only modify protections along 64kB
    // boundaries.
    status = amlogic_allocator->Init(16);
    if (status != ZX_OK) {
      LOG(ERROR, "Failed to init allocator for amlogic protected memory: %d", status);
      return status;
    }
    // For !is_cpu_accessible_, we don't call amlogic_allocator->SetupUnusedPages() until the start
    // of set_ready().
    RunSyncOnLoop([this, &heap, &amlogic_allocator] {
      std::lock_guard thread_checker(*loop_checker_);
      secure_allocators_[heap] = amlogic_allocator.get();
      allocators_[std::move(heap)] = std::move(amlogic_allocator);
    });
  }

  auto service_dir_result = SetupOutgoingServiceDir();
  if (service_dir_result.is_error()) {
    LOG(ERROR, "SetupOutgoingServiceDir failed: %s", service_dir_result.status_string());
    return service_dir_result.status_value();
  }

  auto [devfs_connector_client, devfs_connector_server] =
      fidl::Endpoints<fuchsia_device_fs::Connector>::Create();

  // devfs node
  fuchsia_driver_framework::DevfsAddArgs devfs_args;
  devfs_args.class_name() = "sysmem";
  devfs_args.connector() = std::move(devfs_connector_client);
  devfs_args.inspect() = inspector_.DuplicateVmo();
  auto add_owned_child_result = AddOwnedChild("sysmem-devfs", devfs_args);
  if (!add_owned_child_result.is_ok()) {
    LOG(ERROR, "AddOwnedChild failed: %s", add_owned_child_result.status_string());
    return add_owned_child_result.status_value();
  }
  devfs_owned_child_node_ = std::move(add_owned_child_result).value();

  // compat node
  const fuchsia_driver_framework::NodePropertyVector empty_node_propery_vector;
  std::vector<fuchsia_driver_framework::Offer> offers = compat_server_.CreateOffers2();
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_sysmem::Service>());
  zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> add_child_result =
      AddChild("sysmem", empty_node_propery_vector, offers);
  if (!add_child_result.is_ok()) {
    LOG(ERROR, "Failed to call FIDL AddChild: %s", add_child_result.status_string());
    return ZX_ERR_INTERNAL;
  }
  compat_node_controller_client_ = fidl::SyncClient(std::move(add_child_result).value());

  RunSyncOnLoop([this, devfs_connector_server = std::move(devfs_connector_server)]() mutable {
    devfs_connector_.AddBinding(
        loop_dispatcher(), std::move(devfs_connector_server), this,
        [](fidl::UnbindInfo unbind_info) { LOG(WARNING, "unexpected devfs connector unbind"); });
  });

  if constexpr (kLogAllCollectionsPeriodically) {
    ZX_ASSERT(ZX_OK ==
              log_all_collections_.PostDelayed(loop_dispatcher(), kLogAllCollectionsInterval));
  }

  LOG(INFO, "sysmem finished initialization");

  return ZX_OK;
}

zx::result<> Device::SetupOutgoingServiceDir() {
  ZX_DEBUG_ASSERT(fdf::Dispatcher::GetCurrent()->async_dispatcher() == driver_dispatcher());
  // outgoing() runs on the fdf dispatcher without being configurable; we post to loop_ on a
  // per-message basis
  auto add_result = outgoing()->AddService<fuchsia_hardware_sysmem::Service>(
      fuchsia_hardware_sysmem::Service::InstanceHandler({
          .sysmem = bindings_.CreateHandler(this, driver_dispatcher(), fidl::kIgnoreBindingClosure),
          .allocator_v1 =
              [this](fidl::ServerEnd<fuchsia_sysmem::Allocator> request) {
                PostTask([this, request = std::move(request)]() mutable {
                  std::lock_guard thread_checker(*loop_checker_);
                  Allocator::CreateOwnedV1(std::move(request), this, v1_allocators());
                });
              },
          .allocator_v2 =
              [this](fidl::ServerEnd<fuchsia_sysmem2::Allocator> request) {
                PostTask([this, request = std::move(request)]() mutable {
                  std::lock_guard thread_checker(*loop_checker_);
                  Allocator::CreateOwnedV2(std::move(request), this, v2_allocators());
                });
              },
      }));
  if (!add_result.is_ok()) {
    LOG(ERROR, "AddService failed: %s", add_result.status_string());
    return add_result.take_error();
  }
  return zx::ok();
}

void Device::Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) {
  if (!request.server().is_valid()) {
    LOG(ERROR, "!request.server().is_valid()");
    return;
  }
  fidl::ServerEnd<fuchsia_hardware_sysmem::DriverConnector> server_end(std::move(request.server()));
  PostTask([this, server_end = std::move(server_end)]() mutable {
    driver_connectors_.AddBinding(
        loop_dispatcher(), std::move(server_end), this,
        [](fidl::UnbindInfo info) { LOG(ERROR, "unexpected DriverConnector disconnect"); });
  });
}

void Device::ConnectV1(ConnectV1Request& request, ConnectV1Completer::Sync& completer) {
  PostTask([this, request = std::move(request.allocator_request())]() mutable {
    Allocator::CreateOwnedV1(std::move(request), this, v1_allocators());
  });
}

void Device::ConnectV2(ConnectV2Request& request, ConnectV2Completer::Sync& completer) {
  PostTask([this, request = std::move(request.allocator_request())]() mutable {
    Allocator::CreateOwnedV2(std::move(request), this, v2_allocators());
  });
}

void Device::ConnectSysmem(ConnectSysmemRequest& request, ConnectSysmemCompleter::Sync& completer) {
  zx_status_t status = async::PostTask(
      driver_dispatcher(), [request = std::move(request.sysmem_request()), this]() mutable {
        bindings_.AddBinding(driver_dispatcher(), std::move(request), this,
                             fidl::kIgnoreBindingClosure);
      });
  ZX_DEBUG_ASSERT_MSG(status == ZX_OK, "Failed to post task to connect to Sysmem protocol: %s",
                      zx_status_get_string(status));
}

void Device::SetAuxServiceDirectory(SetAuxServiceDirectoryRequest& request,
                                    SetAuxServiceDirectoryCompleter::Sync& completer) {
  PostTask([this, aux_service_directory = std::make_shared<sys::ServiceDirectory>(
                      request.service_directory().TakeChannel())] {
    // Should the need arise in future, it'd be fine to stash a shared_ptr<aux_service_directory>
    // here if we need it for anything else.
    snapshot_annotation_register_->SetServiceDirectory(aux_service_directory, loop_dispatcher());
    metrics_.metrics_buffer().SetServiceDirectory(aux_service_directory);
    metrics_.LogUnusedPageCheck(sysmem_metrics::UnusedPageCheckMetricDimensionEvent_Connectivity);
  });
}

zx_status_t Device::RegisterHeapInternal(
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
        LOG(ERROR, "Heap failed: %s\n", info.FormatDescription().c_str());
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
      event_handler->heap_client_.Bind(std::move(heap_client_end), device->loop_dispatcher(),
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

  PostTask([this, heap = std::move(heap), heap_connection = std::move(heap_connection)]() mutable {
    std::lock_guard checker(*loop_checker_);
    EventHandler::Bind(this, std::move(heap_connection), std::move(heap));
  });
  return ZX_OK;
}

zx_status_t Device::RegisterSecureMemInternal(
    fidl::ClientEnd<fuchsia_sysmem::SecureMem> secure_mem_connection) {
  LOG(DEBUG, "sysmem RegisterSecureMem begin");

  current_close_is_abort_ = std::make_shared<std::atomic_bool>(true);

  PostTask([this, secure_mem_connection = std::move(secure_mem_connection),
            close_is_abort = current_close_is_abort_]() mutable {
    std::lock_guard checker(*loop_checker_);
    // This code must run asynchronously for two reasons:
    // 1) It does synchronous IPCs to the secure mem device, so SysmemRegisterSecureMem must
    // have return so the call from the secure mem device is unblocked.
    // 2) It modifies member variables like |secure_mem_| and |heaps_| that should only be
    // touched on |loop_|'s thread.
    auto wait_for_close = std::make_unique<async::Wait>(
        secure_mem_connection.channel().get(), ZX_CHANNEL_PEER_CLOSED, 0,
        async::Wait::Handler([this, close_is_abort](async_dispatcher_t* loop_dispatcher,
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
    zx_status_t status = wait_for_close->Begin(loop_dispatcher());
    if (status != ZX_OK) {
      LOG(ERROR, "Device::RegisterSecureMem() failed wait_for_close->Begin()");
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
      ZX_ASSERT_MSG(get_properties_result->is_ok(), "%s",
                    get_properties_result.FormatDescription().c_str());
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
    ZX_ASSERT_MSG(get_result->is_ok(), "%s", get_result.FormatDescription().c_str());
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
      constexpr bool kIsEverZirconAccessible = false;
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
          kIsEverCpuAccessible, kIsEverZirconAccessible, kIsReady, kCanBeTornDown,
          loop_dispatcher());
      status = secure_allocator->InitPhysical(heap_range.physical_address().value());
      // A failing status is fatal for now.
      ZX_ASSERT_MSG(status == ZX_OK, "%s", zx_status_get_string(status));
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
zx_status_t Device::UnregisterSecureMemInternal() {
  // By this point, the aml-securemem driver's suspend(mexec) has already prepared for mexec.
  //
  // In this path, the server end of the channel hasn't closed yet, but will be closed shortly after
  // return from UnregisterSecureMem().
  //
  // We set a flag here so that a PEER_CLOSED of the channel won't cause the wait handler to crash.
  *current_close_is_abort_ = false;
  current_close_is_abort_.reset();
  PostTask([this]() {
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
zx::result<zx::vmo> Device::CreatePhysicalVmo(uint64_t base, uint64_t size) {
  // This isn't called much, so get the mmio resource each time rather than caching it.
  zx::result resource_result = incoming()->Connect<fuchsia_kernel::MmioResource>();
  if (resource_result.is_error()) {
    LOG(ERROR, "Connect<fuchsia_kernel::MmioResource>() failed: %s",
        resource_result.status_string());
    return resource_result.take_error();
  }
  auto resource_protocol = fidl::SyncClient(std::move(resource_result).value());
  auto get_result = resource_protocol->Get();
  if (!get_result.is_ok()) {
    LOG(ERROR, "resource->Get() failed: %s", get_result.error_value().FormatDescription().c_str());
    return zx::error(get_result.error_value().status());
  }
  auto resource = std::move(std::move(get_result).value().resource());

  zx::vmo result_vmo;
  zx_status_t status = zx::vmo::create_physical(resource, base, size, &result_vmo);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(result_vmo));
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
    if (token_server_koid != 0) {
      unfound_token_koids_.push_back(token_server_koid);
      constexpr uint32_t kMaxUnfoundTokenCount = 8;
      while (unfound_token_koids_.size() > kMaxUnfoundTokenCount) {
        unfound_token_koids_.pop_front();
      }
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

void Device::LogCollectionsTimer(async_dispatcher_t* loop_dispatcher, async::TaskBase* task,
                                 zx_status_t status) {
  std::lock_guard checker(*loop_checker_);
  LogAllBufferCollections();
  ZX_ASSERT(kLogAllCollectionsPeriodically);
  ZX_ASSERT(ZX_OK == log_all_collections_.PostDelayed(loop_dispatcher, kLogAllCollectionsInterval));
}

void Device::RegisterHeap(RegisterHeapRequest& request, RegisterHeapCompleter::Sync& completer) {
  // TODO(b/316646315): Change RegisterHeap to specify fuchsia_sysmem2::Heap, and remove the
  // conversion here.
  std::lock_guard checker(*driver_checker_);
  auto v2_heap_type_result =
      sysmem::V2CopyFromV1HeapType(static_cast<fuchsia_sysmem::HeapType>(request.heap()));
  if (!v2_heap_type_result.is_ok()) {
    LOG(WARNING, "V2CopyFromV1HeapType failed");
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  auto& v2_heap_type = v2_heap_type_result.value();
  auto heap = sysmem::MakeHeap(std::move(v2_heap_type), 0);
  zx_status_t status = RegisterHeapInternal(std::move(heap), std::move(request.heap_connection()));
  if (status != ZX_OK) {
    LOG(WARNING, "CommonSysmemRegisterHeap failed");
    completer.Close(status);
    return;
  }
}

void Device::RegisterSecureMem(RegisterSecureMemRequest& request,
                               RegisterSecureMemCompleter::Sync& completer) {
  std::lock_guard checker(*driver_checker_);
  zx_status_t status = RegisterSecureMemInternal(std::move(request.secure_mem_connection()));
  if (status != ZX_OK) {
    completer.Close(status);
    return;
  }
}

void Device::UnregisterSecureMem(UnregisterSecureMemCompleter::Sync& completer) {
  std::lock_guard checker(*driver_checker_);
  zx_status_t status = UnregisterSecureMemInternal();
  if (status == ZX_OK) {
    completer.Reply(fit::ok());
  } else {
    completer.Reply(fit::error(status));
  }
}

}  // namespace sysmem_driver

FUCHSIA_DRIVER_EXPORT(sysmem_driver::Device);
