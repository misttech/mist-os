// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_BLOCK_DEVICE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_BLOCK_DEVICE_H_

#include <fidl/fuchsia.hardware.sdmmc/cpp/wire.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/operation/block.h>
#include <lib/sdmmc/hw.h>
#include <lib/sync/cpp/completion.h>
#include <lib/trace/event.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <threads.h>
#include <zircon/types.h>

#include <array>
#include <atomic>
#include <cinttypes>
#include <deque>
#include <memory>
#include <semaphore>

#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>

#include "sdmmc-device.h"
#include "sdmmc-partition-device.h"
#include "sdmmc-rpmb-device.h"
#include "sdmmc-types.h"
#include "src/storage/lib/block_server/block_server.h"

namespace sdmmc {

class SdmmcRootDevice;

// This struct is used to maintain metadata for IO while the IO is in progress: When using Banjo,
// this is a hard requirement, as only object handles are passed through the call. When using FIDL,
// objects can be transferred through the call. However for performance, we avoid repeatedly
// creating and initializing objects.
struct ReadWriteMetadata {
 public:
  explicit ReadWriteMetadata(SdmmcBlockDevice* block_device) : block_device(block_device) {}

  // This initialization is only required if packed commands are used.
  zx_status_t InitForPackedCommands(uint32_t buffer_region_count, uint32_t block_size) {
    buffer_regions = std::make_unique<sdmmc_buffer_region_t[]>(buffer_region_count);
    memset(buffer_regions.get(), 0, sizeof(sdmmc_buffer_region_t) * buffer_region_count);

    zx_status_t status = zx::vmo::create(block_size, 0, &packed_command_header_vmo);
    if (status != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "Failed to create packed command header vmo: %s",
               zx_status_get_string(status));
      return status;
    }

    status = packed_command_header_mapper.Map(packed_command_header_vmo);
    if (status != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "Failed to map packed command header vmo: %s",
               zx_status_get_string(status));
      return status;
    }

    packed_command_header_data = static_cast<PackedCommand*>(packed_command_header_mapper.start());
    memset(packed_command_header_data, 0, block_size);
    packed_command_header_data->version = 1;
    return ZX_OK;
  }

  fdf::Logger& logger();

  // For non-packed commands, only this is needed, as initialized here.
  std::unique_ptr<sdmmc_buffer_region_t[]> buffer_regions =
      std::make_unique<sdmmc_buffer_region_t[]>(1);

  // For packed commands, the following are also needed.
  zx::vmo packed_command_header_vmo;
  fzl::VmoMapper packed_command_header_mapper;
  PackedCommand* packed_command_header_data;

 private:
  SdmmcBlockDevice* const block_device;
};

class SdmmcBlockDevice : public block_server::Interface {
 public:
  static constexpr char kHardwarePowerElementName[] = "sdmmc-hardware";

  // Power levels for the sdmmc-hardware element.
  static constexpr fuchsia_power_broker::PowerLevel kPowerLevelOff = 0;
  static constexpr fuchsia_power_broker::PowerLevel kPowerLevelOn = 1;
  // Note that this power level actually represents a LOWER power
  // state than kPowerLevelOn, based on the order the level is
  // supplied when the element is created.
  static constexpr fuchsia_power_broker::PowerLevel kPowerLevelBoot = 2;

  SdmmcBlockDevice(SdmmcRootDevice* parent, std::unique_ptr<SdmmcDevice> sdmmc)
      : parent_(parent), sdmmc_(std::move(sdmmc)) {
    block_info_.max_transfer_size = static_cast<uint32_t>(sdmmc_->host_info().max_transfer_size);
  }

  static zx_status_t Create(SdmmcRootDevice* parent, std::unique_ptr<SdmmcDevice> sdmmc,
                            std::unique_ptr<SdmmcBlockDevice>* out_dev);
  // Returns the SdmmcDevice. Used if this SdmmcBlockDevice fails to probe (i.e., no eligible device
  // present).
  std::unique_ptr<SdmmcDevice> TakeSdmmcDevice() { return std::move(sdmmc_); }

  // Probe for SD first, then MMC.
  zx_status_t Probe(const fuchsia_hardware_sdmmc::SdmmcMetadata& metadata) TA_EXCL(worker_lock_) {
    fbl::AutoLock lock(&worker_lock_);
    return ProbeSdLocked(metadata) == ZX_OK ? ZX_OK : ProbeMmcLocked(metadata);
  }
  zx_status_t ProbeSd(const fuchsia_hardware_sdmmc::SdmmcMetadata& metadata) TA_EXCL(worker_lock_) {
    fbl::AutoLock lock(&worker_lock_);
    return ProbeSdLocked(metadata);
  }
  zx_status_t ProbeMmc(const fuchsia_hardware_sdmmc::SdmmcMetadata& metadata)
      TA_EXCL(worker_lock_) {
    fbl::AutoLock lock(&worker_lock_);
    return ProbeMmcLocked(metadata);
  }

  zx_status_t AddDevice() TA_EXCL(queue_lock_);

  void StopWorkerDispatcher(std::optional<fdf::PrepareStopCompleter> completer = std::nullopt)
      TA_EXCL(queue_lock_);

  zx_status_t SuspendPower() TA_REQ(worker_lock_);
  zx_status_t ResumePower() TA_REQ(worker_lock_);

  // Called by children of this device.
  void Queue(BlockOperation txn) TA_EXCL(queue_lock_);
  void RpmbQueue(RpmbRequestInfo info) TA_EXCL(queue_lock_);
  fidl::WireSyncClient<fuchsia_driver_framework::Node>& block_node() { return block_node_; }
  std::string_view block_name() const { return block_name_; }
  SdmmcRootDevice* parent() { return parent_; }

  // Visible for testing.
  void SetBlockInfo(uint32_t block_size, uint64_t block_count);
  const inspect::Inspector& inspect() const;
  const std::vector<std::unique_ptr<PartitionDevice>>& child_partition_devices() const {
    return child_partition_devices_;
  }
  const std::unique_ptr<RpmbDevice>& child_rpmb_device() const { return child_rpmb_device_; }

  fdf::Logger& logger() const;

 private:
  // An arbitrary limit to prevent RPMB clients from flooding us with requests.
  static constexpr size_t kMaxOutstandingRpmbRequests = 16;

  // The worker thread will handle this many block ops then this many RPMB requests, and will repeat
  // until both queues are empty.
  static constexpr size_t kRoundRobinRequestCount = 16;

  zx_status_t ProbeSdLocked(const fuchsia_hardware_sdmmc::SdmmcMetadata& metadata)
      TA_REQ(worker_lock_);
  zx_status_t ProbeMmcLocked(const fuchsia_hardware_sdmmc::SdmmcMetadata& metadata)
      TA_REQ(worker_lock_);

  template <typename Request>
  zx_status_t ReadWriteWithRetries(std::vector<Request>& requests, EmmcPartition partition)
      TA_REQ(worker_lock_);
  template <typename Request>
  zx_status_t ReadWriteAttempt(std::vector<Request>& requests, bool suppress_error_messages)
      TA_REQ(worker_lock_);
  zx_status_t Flush() TA_REQ(worker_lock_);
  zx_status_t Trim(const block_trim_t& txn, const EmmcPartition partition) TA_REQ(worker_lock_);
  zx_status_t SetPartition(const EmmcPartition partition) TA_REQ(worker_lock_);
  zx_status_t RpmbRequest(const RpmbRequestInfo& request) TA_REQ(worker_lock_);

  void HandleBlockOps(block::BorrowedOperationQueue<PartitionInfo>& txn_list) TA_REQ(worker_lock_);
  void HandleRpmbRequests(std::deque<RpmbRequestInfo>& rpmb_list) TA_REQ(worker_lock_);

  void WorkerLoop();

  zx_status_t WaitForTran();

  zx_status_t MmcDoSwitch(uint8_t index, uint8_t value) TA_REQ(worker_lock_);
  zx_status_t MmcWaitForSwitch(uint8_t index, uint8_t value) TA_REQ(worker_lock_);
  zx_status_t MmcSetBusWidth(sdmmc_bus_width_t bus_width, uint8_t mmc_ext_csd_bus_width)
      TA_REQ(worker_lock_);
  sdmmc_bus_width_t MmcSelectBusWidth() TA_REQ(worker_lock_);
  // The host is expected to switch the timing from HS200 to HS as part of HS400 initialization.
  // Checking the status of the switch requires special handling to avoid a temporary mismatch
  // between the host and device timings.
  zx_status_t MmcSwitchTiming(sdmmc_timing_t new_timing) TA_REQ(worker_lock_);
  zx_status_t MmcSwitchTimingHs200ToHs() TA_REQ(worker_lock_);
  zx_status_t MmcSwitchFreq(uint32_t new_freq);
  zx_status_t MmcDecodeExtCsd() TA_REQ(worker_lock_);
  bool MmcSupportsHs() TA_REQ(worker_lock_);
  bool MmcSupportsHsDdr() TA_REQ(worker_lock_);
  bool MmcSupportsHs200() TA_REQ(worker_lock_);
  bool MmcSupportsHs400() TA_REQ(worker_lock_);
  void MmcSetInspectProperties() TA_REQ(worker_lock_) TA_REQ(queue_lock_);

  void BlockComplete(sdmmc::BlockOperation& txn, zx_status_t status);

  // TODO(b/309152899): Once fuchsia.power.SuspendEnabled config cap is available, have this method
  // return failure if power management could not be configured. Use fuchsia.power.SuspendEnabled to
  // ignore this failure when expected.
  // Register power configs with Power Broker, and begin the continuous power level adjustment of
  // hardware. For products that don't support the Power Framework, this method simply returns
  // success.
  zx::result<> ConfigurePowerManagement();

  // Acquires a lease on a power element via the supplied |lessor_client|, returning the resulting
  // lease control client end.
  // This method is planned for use in a future change.
  zx::result<fidl::ClientEnd<fuchsia_power_broker::LeaseControl>> AcquireInitLease(
      const fidl::WireSyncClient<fuchsia_power_broker::Lessor>& lessor_client);

  // Informs Power Broker of the updated |power_level| via the supplied |current_level_client|.
  void UpdatePowerLevel(
      const fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel>& current_level_client,
      fuchsia_power_broker::PowerLevel power_level);

  // Watches the required hardware power level and adjusts it accordingly. Also serves requests that
  // were delayed because they were received during suspended state. Communicates power level
  // transitions to the Power Broker.
  void WatchHardwareRequiredLevel();

  // block_server::Interface
  void StartThread(block_server::Thread) override;
  void OnNewSession(block_server::Session) override;
  void OnRequests(const block_server::Session&, cpp20::span<block_server::Request>) override;
  void Log(std::string_view msg) const override {
    FDF_LOGL(INFO, logger(), "%.*s", static_cast<int>(msg.size()), msg.data());
  }

  SdmmcRootDevice* const parent_;
  // Only accessed by ProbeSd, ProbeMmc, SuspendPower, ResumePower, and WorkerLoop.
  std::unique_ptr<SdmmcDevice> sdmmc_;

  sdmmc_bus_width_t bus_width_;
  sdmmc_timing_t timing_;

  uint32_t clock_rate_;  // Bus clock rate

  // mmc
  std::array<uint8_t, SDMMC_CID_SIZE> raw_cid_;
  std::array<uint8_t, MMC_EXT_CSD_SIZE> raw_ext_csd_ TA_GUARDED(worker_lock_);

  // `worker_lock_` must be held before interacting with the device.
  fbl::Mutex worker_lock_ TA_ACQ_BEFORE(queue_lock_);
  fbl::ConditionVariable worker_condition_;
  fbl::Mutex queue_lock_ TA_ACQ_AFTER(worker_lock_);

  // Signals the worker loop to process incoming commands (or driver shutdown).
  libsync::Completion worker_event_;

  // blockio requests
  block::BorrowedOperationQueue<PartitionInfo> txn_list_ TA_GUARDED(queue_lock_);
  std::deque<RpmbRequestInfo> rpmb_list_ TA_GUARDED(queue_lock_);

  // Dispatcher for processing queued block requests.
  fdf::Dispatcher worker_dispatcher_;
  // Signaled when worker_dispatcher_ is shut down.
  libsync::Completion worker_shutdown_completion_;

  bool power_suspended_ TA_GUARDED(worker_lock_) = false;
  bool shutdown_ TA_GUARDED(worker_lock_) = false;
  trace_async_id_t trace_async_id_;

  fidl::WireSyncClient<fuchsia_power_broker::ElementControl> hardware_power_element_control_client_;
  fidl::WireSyncClient<fuchsia_power_broker::Lessor> hardware_power_lessor_client_;
  fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel> hardware_power_current_level_client_;
  fidl::WireClient<fuchsia_power_broker::RequiredLevel> hardware_power_required_level_client_;
  zx::event hardware_power_element_assertive_token_;
  fidl::ClientEnd<fuchsia_power_broker::LeaseControl> hardware_power_lease_control_client_end_;

  block_info_t block_info_{};

  bool is_sd_ = false;
  bool cache_enabled_ = false;

  uint32_t max_packed_reads_effective_ = 0;   // Use command packing up to this many reads.
  uint32_t max_packed_writes_effective_ = 0;  // Use command packing up to this many writes.
  ReadWriteMetadata readwrite_metadata_ TA_GUARDED(worker_lock_){this};

  inspect::Node root_;
  struct InspectProperties {
    inspect::UintProperty io_errors_;                    // Only updated from the worker thread.
    inspect::UintProperty io_retries_;                   // Only updated from the worker thread.
    inspect::UintProperty type_a_lifetime_used_;         // Set once by the init thread.
    inspect::UintProperty type_b_lifetime_used_;         // Set once by the init thread.
    inspect::UintProperty max_lifetime_used_;            // Set once by the init thread.
    inspect::UintProperty cache_size_bits_;              // Set once by the init thread.
    inspect::BoolProperty cache_enabled_;                // Set once by the init thread.
    inspect::BoolProperty cache_flush_fifo_;             // Set once by the init thread.
    inspect::BoolProperty barrier_supported_;            // Set once by the init thread.
    inspect::BoolProperty trim_enabled_;                 // Set once by the init thread.
    inspect::UintProperty max_packed_reads_;             // Set once by the init thread.
    inspect::UintProperty max_packed_writes_;            // Set once by the init thread.
    inspect::UintProperty max_packed_reads_effective_;   // Set once by the init thread.
    inspect::UintProperty max_packed_writes_effective_;  // Set once by the init thread.
    inspect::BoolProperty using_fidl_;                   // Set once by the init thread.
    inspect::BoolProperty power_suspended_;              // Updated whenever power state changes.
  } properties_;

  std::string_view block_name_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> block_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  std::vector<std::unique_ptr<PartitionDevice>> child_partition_devices_;
  std::unique_ptr<RpmbDevice> child_rpmb_device_;
  std::optional<block_server::BlockServer> block_server_ TA_GUARDED(queue_lock_);
  EmmcPartition current_partition_ TA_GUARDED(worker_lock_) = EmmcPartition::USER_DATA_PARTITION;
};

}  // namespace sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_BLOCK_DEVICE_H_
