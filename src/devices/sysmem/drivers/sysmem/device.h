// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_DEVICE_H_
#define SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_DEVICE_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fuchsia/hardware/sysmem/c/banjo.h>
#include <fuchsia/hardware/sysmem/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/wait.h>
#include <lib/closure-queue/closure_queue.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fit/thread_checker.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/zx/bti.h>
#include <lib/zx/channel.h>

#include <limits>
#include <map>
#include <memory>
#include <unordered_set>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/vector.h>
#include <region-alloc/region-alloc.h>

#include "src/devices/sysmem/drivers/sysmem/memory_allocator.h"
#include "src/devices/sysmem/drivers/sysmem/sysmem_metrics.h"

namespace sys {
class ServiceDirectory;
}  // namespace sys

namespace sysmem_driver {

// For now, sysmem-connector (not a driver) uses DriverConnector, both to get Allocator channels,
// and to notice when/if sysmem crashes. Iff DFv2 allows for sysmem-connector to potentially connect
// via FidlDevice (see below) instead, we could potentially move this protocol to FidlDevice or
// combine with fuchsia_hardware_sysmem::Sysmem. But for now DriverConnector is still a separate
// protocol for use by sysmem-connector, not for use by other drivers.

class Device;
using DdkDeviceType =
    ddk::Device<Device, ddk::Messageable<fuchsia_hardware_sysmem::DriverConnector>::Mixin,
                ddk::Unbindable>;

class Driver;
class BufferCollectionToken;
class LogicalBuffer;
class LogicalBufferCollection;
class Node;

struct Settings {
  // Maximum size of a single allocation. Mainly useful for unit tests.
  uint64_t max_allocation_size = UINT64_MAX;
};

class Device final : public DdkDeviceType,
                     // This needs to remain until clients requesting "sysmem" for the banjo
                     // protocol at least change to request "sysmem-banjo", or preferably switch to
                     // fidl and request "sysmem-fidl".
                     public ddk::SysmemProtocol<Device, ddk::base_protocol>,
                     public MemoryAllocator::Owner {
 public:
  Device(zx_device_t* parent_device, Driver* parent_driver);

  [[nodiscard]] zx_status_t OverrideSizeFromCommandLine(const char* name, int64_t* memory_size);
  [[nodiscard]] zx::result<std::string> GetFromCommandLine(const char* name);
  [[nodiscard]] zx::result<bool> GetBoolFromCommandLine(const char* name, bool default_value);
  [[nodiscard]] zx_status_t GetContiguousGuardParameters(
      uint64_t* guard_bytes_out, bool* unused_pages_guarded,
      zx::duration* unused_page_check_cycle_period, bool* internal_guard_pages_out,
      bool* crash_on_fail_out);

  [[nodiscard]] zx_status_t Bind();

  //
  // The rest of the methods are only valid to call after Bind().
  //

  [[nodiscard]] zx_status_t CommonSysmemConnectV1(zx::channel allocator_request);
  [[nodiscard]] zx_status_t CommonSysmemConnectV2(zx::channel allocator_request);
  [[nodiscard]] zx_status_t CommonSysmemRegisterHeap(
      fuchsia_sysmem2::Heap heap, fidl::ClientEnd<fuchsia_hardware_sysmem::Heap> heap_connection);
  [[nodiscard]] zx_status_t CommonSysmemRegisterSecureMem(
      fidl::ClientEnd<fuchsia_sysmem::SecureMem> secure_mem_connection);
  [[nodiscard]] zx_status_t CommonSysmemUnregisterSecureMem();

  // SysmemProtocol "direct == true" Banjo implementation. This will be removed soon in favor of a
  // FIDL implementation. Currently these complain to the log on first use, and then delegate to the
  // "Common" methods above.
  //
  // This "direct == true" implementation is used when the banjo protocol is requested on "sysmem"
  // instead of "sysmem-banjo".
  //
  // Requesting banjo using "sysmem-banjo" is slightly preferred over "sysmem" (worth the CL), but
  // switching to sysmem FIDL is much better, since sysmem banjo is deprecated.
  [[nodiscard]] zx_status_t SysmemConnect(zx::channel allocator_request);
  [[nodiscard]] zx_status_t SysmemRegisterHeap(uint64_t heap, zx::channel heap_connection);
  [[nodiscard]] zx_status_t SysmemRegisterSecureMem(zx::channel tee_connection);
  [[nodiscard]] zx_status_t SysmemUnregisterSecureMem();

  // Ddk mixin implementations.
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease() { delete this; }

  // MemoryAllocator::Owner implementation.
  [[nodiscard]] const zx::bti& bti() override;
  [[nodiscard]] zx_status_t CreatePhysicalVmo(uint64_t base, uint64_t size,
                                              zx::vmo* vmo_out) override;
  void CheckForUnbind() override;
  SysmemMetrics& metrics() override;
  protected_ranges::ProtectedRangesCoreControl& protected_ranges_core_control(
      const fuchsia_sysmem2::Heap& heap) override;

  inspect::Node* heap_node() override { return &heaps_; }

  void ConnectV1(ConnectV1RequestView request, ConnectV1Completer::Sync& completer) override;
  void ConnectV2(ConnectV2RequestView request, ConnectV2Completer::Sync& completer) override;
  void SetAuxServiceDirectory(SetAuxServiceDirectoryRequestView request,
                              SetAuxServiceDirectoryCompleter::Sync& completer) override;

  [[nodiscard]] uint32_t pdev_device_info_vid();

  [[nodiscard]] uint32_t pdev_device_info_pid();

  // Track/untrack the token by the koid of the server end of its FIDL channel. TrackToken() is only
  // allowed after/during token->OnServerKoid(). UntrackToken() is allowed even if there was never a
  // token->OnServerKoid() (in which case it's a nop).
  //
  // While tracked, a token can be found with FindTokenByServerChannelKoid().
  void TrackToken(BufferCollectionToken* token);
  void UntrackToken(BufferCollectionToken* token);

  // Finds and removes token_server_koid from unfound_token_koids_.
  [[nodiscard]] bool TryRemoveKoidFromUnfoundTokenList(zx_koid_t token_server_koid);

  // Find the BufferCollectionToken (if any) by the koid of the server end of its FIDL channel.
  [[nodiscard]] BufferCollectionToken* FindTokenByServerChannelKoid(zx_koid_t token_server_koid);

  struct FindLogicalBufferByVmoKoidResult {
    LogicalBuffer* logical_buffer;
    bool is_koid_of_weak_vmo;
  };
  [[nodiscard]] FindLogicalBufferByVmoKoidResult FindLogicalBufferByVmoKoid(zx_koid_t vmo_koid);

  // Get allocator for |settings|. Returns NULL if allocator is not registered for settings.
  [[nodiscard]] MemoryAllocator* GetAllocator(
      const fuchsia_sysmem2::BufferMemorySettings& settings);

  // Get heap properties of a specific memory heap allocator.
  //
  // If the heap is not valid or not registered to sysmem driver, nullptr is returned.
  [[nodiscard]] const fuchsia_hardware_sysmem::HeapProperties* GetHeapProperties(
      const fuchsia_sysmem2::Heap& heap) const;

  [[nodiscard]] const sysmem_protocol_t* proto() const { return &in_proc_sysmem_protocol_; }
  [[nodiscard]] const zx_device_t* device() const { return zxdev_; }
  [[nodiscard]] async_dispatcher_t* dispatcher() { return loop_.dispatcher(); }

  [[nodiscard]] std::unordered_set<LogicalBufferCollection*>& logical_buffer_collections() {
    std::lock_guard checker(*loop_checker_);
    return logical_buffer_collections_;
  }

  void AddLogicalBufferCollection(LogicalBufferCollection* collection) {
    std::lock_guard checker(*loop_checker_);
    logical_buffer_collections_.insert(collection);
  }

  void RemoveLogicalBufferCollection(LogicalBufferCollection* collection) {
    std::lock_guard checker(*loop_checker_);
    logical_buffer_collections_.erase(collection);
    CheckForUnbind();
  }

  void AddVmoKoid(zx_koid_t koid, bool is_weak, LogicalBuffer& logical_buffer);
  void RemoveVmoKoid(zx_koid_t koid);

  [[nodiscard]] inspect::Node& collections_node() { return collections_node_; }

  void set_settings(const Settings& settings) { settings_ = settings; }

  [[nodiscard]] const Settings& settings() const { return settings_; }

  void ResetThreadCheckerForTesting() { loop_checker_.emplace(fit::thread_checker()); }

  bool protected_ranges_disable_dynamic() const override {
    std::lock_guard checker(*loop_checker_);
    return cmdline_protected_ranges_disable_dynamic_;
  }

  // false - no secure heaps are expected to exist
  // true - secure heaps are expected to exist (regardless of whether any of them currently exist)
  bool is_secure_mem_expected() const {
    std::lock_guard checker(*loop_checker_);
    // Currently, we can base this on secure_allocators_ non-empty() since in all current cases
    // there will be at least one secure allocator added before any clients can connect iff there
    // will be any secure heaps available. Non-empty here does not imply that all secure heaps are
    // already present and ready. For that, use is_secure_mem_ready().
    return !secure_allocators_.empty();
  }

  // false - secure mem is expected, but is not yet ready
  //
  // true - secure mem is not expected (and is therefore as ready as it will ever be / ready in the
  // "secure mem system is ready for allocation requests" sense), or secure mem is expected and
  // ready.
  bool is_secure_mem_ready() const {
    std::lock_guard checker(*loop_checker_);
    if (!is_secure_mem_expected()) {
      // attempts to use secure mem can go ahead and try to allocate and fail to allocate, so this
      // means "as ready
      return true;
    }
    return is_secure_mem_ready_;
  }

  template <typename F>
  void postTask(F to_run) {
    zx_status_t post_status = async::PostTask(loop_.dispatcher(), std::move(to_run));
    ZX_ASSERT_MSG(post_status == ZX_OK || (post_status == ZX_ERR_BAD_STATE && waiting_for_unbind_),
                  "async::PostTask failed: %d", post_status);
  }

  template <typename F>
  void runSyncOnLoop(F to_run) {
    // Must not call runSyncOnLoop() from the loop_ thread, since that would get stuck.
    ZX_DEBUG_ASSERT(!loop_checker_->is_thread_valid());
    sync_completion_t done;
    postTask([&done, to_run = std::move(to_run)]() mutable {
      std::move(to_run)();
      sync_completion_signal(&done);
    });
    ZX_ASSERT(ZX_OK == sync_completion_wait_deadline(&done, ZX_TIME_INFINITE));
  }

  virtual void OnAllocationFailure() override { LogAllBufferCollections(); }
  void LogAllBufferCollections();

 private:
  class SecureMemConnection {
   public:
    SecureMemConnection(fidl::ClientEnd<fuchsia_sysmem::SecureMem> channel,
                        std::unique_ptr<async::Wait> wait_for_close);
    const fidl::WireSyncClient<fuchsia_sysmem::SecureMem>& channel() const;

   private:
    fidl::WireSyncClient<fuchsia_sysmem::SecureMem> connection_;
    std::unique_ptr<async::Wait> wait_for_close_;
  };

  // to_run must not cause creation or deletion of any LogicalBufferCollection(s), with the one
  // exception of causing deletion of the passed-in LogicalBufferCollection, which is allowed
  void ForEachLogicalBufferCollection(fit::function<void(LogicalBufferCollection*)> to_run) {
    std::lock_guard checker(*loop_checker_);
    // to_run can erase the current item, but std::unordered_set only invalidates iterators pointing
    // at the erased item, so we can just save the pointer and advance iter before calling to_run
    //
    // to_run must not cause any other iterator invalidation
    LogicalBufferCollections::iterator next;
    for (auto iter = logical_buffer_collections_.begin(); iter != logical_buffer_collections_.end();
         /* iter already advanced in the loop */) {
      auto* item = *iter;
      ++iter;
      to_run(item);
    }
  }

  void LogCollectionsTimer(async_dispatcher_t* dispatcher, async::TaskBase* task,
                           zx_status_t status);

  Driver* parent_driver_ = nullptr;
  inspect::Inspector inspector_;
  async::Loop loop_;
  thrd_t loop_thrd_;
  // During initialization this checks that operations are performed on a DDK thread. After
  // initialization, it checks that operations are on the loop thread.
  mutable std::optional<fit::thread_checker> loop_checker_;

  // Currently located at bootstrap/driver_manager:root/sysmem.
  inspect::Node sysmem_root_;
  inspect::Node heaps_;

  inspect::Node collections_node_;

  fidl::SyncClient<fuchsia_hardware_platform_device::Device> pdev_;
  zx::bti bti_;

  // Initialize these to a value that won't be mistaken for a real vid or pid.
  uint32_t pdev_device_info_vid_ = std::numeric_limits<uint32_t>::max();
  uint32_t pdev_device_info_pid_ = std::numeric_limits<uint32_t>::max();

  // In-proc sysmem interface.  Essentially an in-proc version of
  // fuchsia.sysmem.DriverConnector.
  sysmem_protocol_t in_proc_sysmem_protocol_;

  // This map allows us to look up the BufferCollectionToken by the koid of
  // the server end of a BufferCollectionToken channel.
  std::map<zx_koid_t, BufferCollectionToken*> tokens_by_koid_ __TA_GUARDED(*loop_checker_);

  std::deque<zx_koid_t> unfound_token_koids_ __TA_GUARDED(*loop_checker_);

  struct HashHeap {
    size_t operator()(const fuchsia_sysmem2::Heap& heap) const {
      const static auto hash_heap_type = std::hash<std::string>{};
      const static auto hash_id = std::hash<uint64_t>{};
      size_t hash = 0;
      if (heap.heap_type().has_value()) {
        hash = hash ^ hash_heap_type(heap.heap_type().value());
      }
      if (heap.id().has_value()) {
        hash = hash ^ hash_id(heap.id().value());
      }
      return hash;
    }
  };
  // This map contains all registered memory allocators.
  std::unordered_map<fuchsia_sysmem2::Heap, std::shared_ptr<MemoryAllocator>, HashHeap> allocators_
      __TA_GUARDED(*loop_checker_);

  // This map contains only the secure allocators, if any.  The pointers are owned by allocators_.
  //
  // TODO(dustingreen): Consider unordered_map for this and some of above.
  std::unordered_map<fuchsia_sysmem2::Heap, MemoryAllocator*, HashHeap> secure_allocators_
      __TA_GUARDED(*loop_checker_);

  struct SecureMemControl : public protected_ranges::ProtectedRangesCoreControl {
    // ProtectedRangesCoreControl implementation.  These are essentially backed by
    // parent->secure_mem_.
    //
    // cached
    bool IsDynamic() override;
    // cached
    uint64_t MaxRangeCount() override;
    // cached
    uint64_t GetRangeGranularity() override;
    // cached
    bool HasModProtectedRange() override;
    // calls SecureMem driver
    void AddProtectedRange(const protected_ranges::Range& range) override;
    // calls SecureMem driver
    void DelProtectedRange(const protected_ranges::Range& range) override;
    // calls SecureMem driver
    void ModProtectedRange(const protected_ranges::Range& old_range,
                           const protected_ranges::Range& new_range) override;
    // calls SecureMem driver
    void ZeroProtectedSubRange(bool is_covering_range_explicit,
                               const protected_ranges::Range& range) override;

    fuchsia_sysmem2::Heap heap{};
    fuchsia_sysmem::HeapType v1_heap_type{};
    Device* parent{};
    bool is_dynamic{};
    uint64_t range_granularity{};
    uint64_t max_range_count{};
    bool has_mod_protected_range{};
  };
  // This map has the secure_mem_ properties for each fuchsia_sysmem2::Heap in secure_allocators_.
  std::unordered_map<fuchsia_sysmem2::Heap, SecureMemControl, HashHeap> secure_mem_controls_
      __TA_GUARDED(*loop_checker_);

  // This flag is used to determine if the closing of the current secure mem
  // connection is an error (true), or expected (false).
  std::shared_ptr<std::atomic_bool> current_close_is_abort_;

  // This has the connection to the securemem driver, if any.  Once allocated this is supposed to
  // stay allocated unless mexec is about to happen.  The server end takes care of handling
  // DdkSuspend() to allow mexec to work.  For example, by calling secmem TA.  This channel will
  // close from the server end when DdkSuspend(mexec) happens, but only after
  // UnregisterSecureMem().
  std::unique_ptr<SecureMemConnection> secure_mem_ __TA_GUARDED(*loop_checker_);

  std::unique_ptr<MemoryAllocator> contiguous_system_ram_allocator_ __TA_GUARDED(*loop_checker_);

  using LogicalBufferCollections = std::unordered_set<LogicalBufferCollection*>;
  LogicalBufferCollections logical_buffer_collections_ __TA_GUARDED(*loop_checker_);

  // A single LogicalBuffer can be in this map multiple times, once per VMO koid that has been
  // handed out by sysmem. Entries are removed when the TrackedParentVmo parent of the handed-out
  // VMO sees ZX_VMO_ZERO_CHILDREN, which occurs before LogicalBuffer is deleted.
  using VmoKoids = std::unordered_map<zx_koid_t, FindLogicalBufferByVmoKoidResult>;
  VmoKoids vmo_koids_;

  Settings settings_;

  std::atomic<bool> waiting_for_unbind_ = false;

  SysmemMetrics metrics_;

  bool cmdline_protected_ranges_disable_dynamic_ __TA_GUARDED(*loop_checker_) = false;

  bool is_secure_mem_ready_ __TA_GUARDED(*loop_checker_) = false;

  async::TaskMethod<Device, &Device::LogCollectionsTimer> log_all_collections_{this};
};

class FidlDevice;
using DdkFidlDeviceBase = ddk::Device<FidlDevice>;

// This device offers a FIDL interface of the fuchsia.hardware.sysmem protocol,
// using Device's Banjo implementation under the hood. It will only exist as
// long as we are incrementally migrating sysmem clients from Banjo to FIDL.
// Once all clients are on FIDL, the Device class will be changed into a FIDL
// server and this class will be removed.
class FidlDevice : public DdkFidlDeviceBase, public fidl::Server<fuchsia_hardware_sysmem::Sysmem> {
 public:
  FidlDevice(zx_device_t* parent, sysmem_driver::Device* sysmem_device,
             async_dispatcher_t* dispatcher);

  zx_status_t Bind();
  void DdkRelease() { delete this; }

  void RegisterHeap(RegisterHeapRequest& request, RegisterHeapCompleter::Sync& completer) override;
  void RegisterSecureMem(RegisterSecureMemRequest& request,
                         RegisterSecureMemCompleter::Sync& completer) override;
  void UnregisterSecureMem(UnregisterSecureMemCompleter::Sync& completer) override;

 private:
  sysmem_driver::Device* sysmem_device_;
  fidl::ServerBindingGroup<fuchsia_hardware_sysmem::Sysmem> bindings_;
  async_dispatcher_t* dispatcher_;
  std::optional<fit::thread_checker> dispatcher_checker_;
  // std::optional<> so we can init on the dispatcher_ thread
  std::optional<component::OutgoingDirectory> outgoing_ __TA_GUARDED(*dispatcher_checker_);
};

class BanjoDevice;

using DdkBanjoDeviceType = ddk::Device<BanjoDevice>;

// SysmemProtocol "direct == false" Banjo implementation. This will be removed soon in favor of
// a FIDL implementation.  Currently these complain to the log on first use, and then delegate to
// the "Common" methods above via the sysmem_device pointer.
//
// This "direct == false" implementation is used when the banjo protocol is requested on
// "sysmem-banjo" instead of "sysmem".
class BanjoDevice : public DdkBanjoDeviceType,
                    public ddk::SysmemProtocol<BanjoDevice, ddk::base_protocol> {
 public:
  BanjoDevice(zx_device_t* parent, sysmem_driver::Device* sysmem_device);

  zx_status_t Bind();
  void DdkRelease() { delete this; }

  [[nodiscard]] zx_status_t SysmemConnect(zx::channel allocator_request);
  [[nodiscard]] zx_status_t SysmemRegisterHeap(uint64_t heap, zx::channel heap_connection);
  [[nodiscard]] zx_status_t SysmemRegisterSecureMem(zx::channel tee_connection);
  [[nodiscard]] zx_status_t SysmemUnregisterSecureMem();

 private:
  sysmem_driver::Device* sysmem_device_;
};

}  // namespace sysmem_driver

#endif  // SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_DEVICE_H_
