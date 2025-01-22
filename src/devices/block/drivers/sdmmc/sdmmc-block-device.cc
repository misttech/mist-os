// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdmmc-block-device.h"

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/driver/power/cpp/element-description-builder.h>
#include <lib/fit/defer.h>
#include <lib/fzl/vmo-mapper.h>
#include <threads.h>
#include <zircon/hw/gpt.h>
#include <zircon/process.h>
#include <zircon/status.h>
#include <zircon/threads.h>

#include <fbl/alloc_checker.h>
#include <safemath/safe_conversions.h>

#include "sdmmc-partition-device.h"
#include "sdmmc-root-device.h"
#include "sdmmc-rpmb-device.h"
#include "src/devices/block/lib/common/include/common.h"
#include "tools/power_config/lib/cpp/power_config.h"

namespace sdmmc {
namespace {

constexpr size_t kTranMaxAttempts = 10;

// Boot and RPMB partition sizes are in units of 128 KiB/KB.
constexpr uint32_t kBootSizeMultiplier = 128 * 1024;

// Populates and returns a fuchsia_hardware_sdmmc::wire::SdmmcBufferRegion using the supplied
// arguments.
zx::result<fuchsia_hardware_sdmmc::wire::SdmmcBufferRegion> GetBufferRegion(zx_handle_t vmo,
                                                                            uint64_t offset,
                                                                            uint64_t size,
                                                                            fdf::Logger& logger) {
  zx::vmo dup;
  zx_status_t status = zx_handle_duplicate(vmo, ZX_RIGHT_SAME_RIGHTS, dup.reset_and_get_address());
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger, "Failed to duplicate vmo: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  fuchsia_hardware_sdmmc::wire::SdmmcBufferRegion buffer_region;
  buffer_region.buffer = fuchsia_hardware_sdmmc::wire::SdmmcBuffer::WithVmo(std::move(dup));
  buffer_region.offset = offset;
  buffer_region.size = size;
  return zx::ok(std::move(buffer_region));
}

zx::result<fuchsia_hardware_power::ComponentPowerConfiguration> GetAllPowerConfigs(
    const fdf::Namespace& ns) {
  zx::result open_result =
      ns.Open<fuchsia_io::File>("/pkg/data/power_config.fidl", fuchsia_io::Flags::kPermRead);
  if (!open_result.is_ok() || !open_result->is_valid()) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  return power_config::Load(std::move(open_result.value()));
}

}  // namespace

zx::result<fidl::ClientEnd<fuchsia_power_broker::LeaseControl>> SdmmcBlockDevice::AcquireInitLease(
    const fidl::WireSyncClient<fuchsia_power_broker::Lessor>& lessor_client) {
  const fidl::WireResult result = lessor_client->Lease(SdmmcBlockDevice::kPowerLevelBoot);
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Call to Lease failed: %s", result.status_string());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    switch (result->error_value()) {
      case fuchsia_power_broker::LeaseError::kInternal:
        FDF_LOGL(ERROR, logger(), "Lease returned internal error.");
        break;
      case fuchsia_power_broker::LeaseError::kNotAuthorized:
        FDF_LOGL(ERROR, logger(), "Lease returned not authorized error.");
        break;
      default:
        FDF_LOGL(ERROR, logger(), "Lease returned unknown error.");
        break;
    }
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (!result->value()->lease_control.is_valid()) {
    FDF_LOGL(ERROR, logger(), "Lease returned invalid lease control client end.");
    return zx::error(ZX_ERR_BAD_STATE);
  }
  return zx::ok(std::move(result->value()->lease_control));
}

void SdmmcBlockDevice::UpdatePowerLevel(
    const fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel>& current_level_client,
    fuchsia_power_broker::PowerLevel power_level) {
  const fidl::WireResult result = current_level_client->Update(power_level);
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Call to Update failed: %s", result.status_string());
  } else if (result->is_error()) {
    FDF_LOGL(ERROR, logger(), "Update returned failure.");
  }
}

fdf::Logger& ReadWriteMetadata::logger() { return block_device->logger(); }

void SdmmcBlockDevice::BlockComplete(sdmmc::BlockOperation& txn, zx_status_t status) {
  if (txn.node()->complete_cb()) {
    txn.Complete(status);
  } else {
    FDF_LOGL(DEBUG, logger(), "block op %p completion_cb unset!", txn.operation());
  }
}

zx_status_t SdmmcBlockDevice::Create(SdmmcRootDevice* parent, std::unique_ptr<SdmmcDevice> sdmmc,
                                     std::unique_ptr<SdmmcBlockDevice>* out_dev) {
  fbl::AllocChecker ac;
  out_dev->reset(new (&ac) SdmmcBlockDevice(parent, std::move(sdmmc)));
  if (!ac.check()) {
    FDF_LOGL(ERROR, parent->logger(), "failed to allocate device memory");
    return ZX_ERR_NO_MEMORY;
  }

  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::AddDevice() {
  // Device must be in TRAN state at this point
  zx_status_t st = WaitForTran();
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "waiting for TRAN state failed, retcode = %d", st);
    return ZX_ERR_TIMED_OUT;
  }

  root_ = parent_->driver_inspector().root().CreateChild("sdmmc_core");
  properties_.io_errors_ = root_.CreateUint("io_errors", 0);
  properties_.io_retries_ = root_.CreateUint("io_retries", 0);

  fbl::AutoLock worker_lock(&worker_lock_);
  fbl::AutoLock lock(&queue_lock_);

  if (!is_sd_) {
    MmcSetInspectProperties();
  }

  auto dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "sdmmc-block-worker",
      [&](fdf_dispatcher_t*) { worker_shutdown_completion_.Signal(); },
      "fuchsia.devices.block.drivers.sdmmc.worker");
  if (dispatcher.is_error()) {
    FDF_LOGL(ERROR, logger(), "Failed to create dispatcher: %s",
             zx_status_get_string(dispatcher.status_value()));
    return dispatcher.status_value();
  }
  worker_dispatcher_ = *std::move(dispatcher);

  st = async::PostTask(worker_dispatcher_.async_dispatcher(), [this] { WorkerLoop(); });
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to start worker thread: %s", zx_status_get_string(st));
    return st;
  }

  if (!is_sd_ && parent_->config().enable_suspend()) {
    zx::result result = ConfigurePowerManagement();
    if (!result.is_ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to configure power management: %s", result.status_string());
      return result.status_value();
    }
  }

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
  auto [node_client_end, node_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::Node>::Create();

  controller_.Bind(std::move(controller_client_end));
  block_node_.Bind(std::move(node_client_end));

  fidl::Arena arena;

  block_name_ = is_sd_ ? "sdmmc-sd" : "sdmmc-mmc";
  const auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, block_name_).Build();

  auto result = parent_->root_node()->AddChild(args, std::move(controller_server_end),
                                               std::move(node_server_end));
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to add child block device: %s", result.status_string());
    return result.status();
  }

  block_server_.emplace(
      block_server::PartitionInfo{
          .block_count = block_info_.block_count,
          .block_size = block_info_.block_size,
      },
      this);

  if (auto result = parent_->driver_outgoing()->AddService<fuchsia_hardware_block_volume::Service>(
          fuchsia_hardware_block_volume::Service::InstanceHandler({
              .volume =
                  [this](fidl::ServerEnd<fuchsia_hardware_block_volume::Volume> server_end) {
                    fbl::AutoLock lock(&queue_lock_);
                    if (block_server_)
                      block_server_->Serve(std::move(server_end));
                  },
          }));
      result.is_error()) {
    FDF_LOGL(ERROR, logger(), "Failed to add service: %s", result.status_string());
    return result.status_value();
  }

  auto remove_device_on_error =
      fit::defer([&]() { [[maybe_unused]] auto result = controller_->Remove(); });

  fbl::AllocChecker ac;
  std::unique_ptr<PartitionDevice> user_partition(
      new (&ac) PartitionDevice(this, block_info_, USER_DATA_PARTITION));
  if (!ac.check()) {
    FDF_LOGL(ERROR, logger(), "failed to allocate device memory");
    return ZX_ERR_NO_MEMORY;
  }

  if ((st = user_partition->AddDevice()) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to add user partition device: %d", st);
    return st;
  }

  child_partition_devices_.push_back(std::move(user_partition));

  if (!is_sd_) {
    const uint32_t boot_size = raw_ext_csd_[MMC_EXT_CSD_BOOT_SIZE_MULT] * kBootSizeMultiplier;
    const bool boot_enabled =
        raw_ext_csd_[MMC_EXT_CSD_PARTITION_CONFIG] & MMC_EXT_CSD_BOOT_PARTITION_ENABLE_MASK;
    if (boot_size > 0 && boot_enabled) {
      const uint64_t boot_partition_block_count = boot_size / block_info_.block_size;
      const block_info_t boot_info = {
          .block_count = boot_partition_block_count,
          .block_size = block_info_.block_size,
          .max_transfer_size = block_info_.max_transfer_size,
          .flags = block_info_.flags,
      };

      std::unique_ptr<PartitionDevice> boot_partition_1(
          new (&ac) PartitionDevice(this, boot_info, BOOT_PARTITION_1));
      if (!ac.check()) {
        FDF_LOGL(ERROR, logger(), "failed to allocate device memory");
        return ZX_ERR_NO_MEMORY;
      }

      std::unique_ptr<PartitionDevice> boot_partition_2(
          new (&ac) PartitionDevice(this, boot_info, BOOT_PARTITION_2));
      if (!ac.check()) {
        FDF_LOGL(ERROR, logger(), "failed to allocate device memory");
        return ZX_ERR_NO_MEMORY;
      }

      if ((st = boot_partition_1->AddDevice()) != ZX_OK) {
        FDF_LOGL(ERROR, logger(), "failed to add boot partition device: %d", st);
        return st;
      }

      child_partition_devices_.push_back(std::move(boot_partition_1));

      if ((st = boot_partition_2->AddDevice()) != ZX_OK) {
        FDF_LOGL(ERROR, logger(), "failed to add boot partition device: %d", st);
        return st;
      }

      child_partition_devices_.push_back(std::move(boot_partition_2));
    }
  }

  if (!is_sd_ && raw_ext_csd_[MMC_EXT_CSD_RPMB_SIZE_MULT] > 0) {
    std::unique_ptr<RpmbDevice> rpmb_device(new (&ac) RpmbDevice(this, raw_cid_, raw_ext_csd_));
    if (!ac.check()) {
      FDF_LOGL(ERROR, logger(), "failed to allocate device memory");
      return ZX_ERR_NO_MEMORY;
    }

    if ((st = rpmb_device->AddDevice()) != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "failed to add rpmb device: %d", st);
      return st;
    }

    child_rpmb_device_ = std::move(rpmb_device);
  }

  remove_device_on_error.cancel();
  return ZX_OK;
}

zx::result<> SdmmcBlockDevice::ConfigurePowerManagement() {
  // Load our config from the package.
  zx::result power_configs = GetAllPowerConfigs(*parent_->driver_incoming());
  if (power_configs.is_error()) {
    FDF_LOGL(INFO, logger(), "Error getting power configs: %s", power_configs.status_string());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  std::vector<fdf_power::PowerElementConfiguration> element_configs;
  for (const fuchsia_hardware_power::PowerElementConfiguration& element_config :
       power_configs.value().power_elements()) {
    auto converted = fdf_power::PowerElementConfiguration::FromFidl(element_config);
    if (converted.is_error()) {
      FDF_LOGL(INFO, logger(), "Failed to convert power element config: %s",
               converted.status_string());
      return converted.take_error();
    }
    element_configs.push_back(std::move(converted.value()));
  }

  fit::result<fdf_power::Error, std::vector<fdf_power::ElementDesc>> config_result =
      fdf_power::ApplyPowerConfiguration(*parent_->driver_incoming(), element_configs);

  if (config_result.is_error()) {
    zx::result<> error = fdf_power::ErrorToZxError(config_result.error_value());
    FDF_LOGL(INFO, logger(), "Failed to apply power config: %s", error.status_string());
    return error;
  }

  std::vector<fdf_power::ElementDesc> elements = std::move(config_result.value());

  // Register power configs with the Power Broker.
  for (size_t i = 0; i < elements.size(); ++i) {
    fdf_power::ElementDesc& description = elements.at(i);

    if (description.element_config.element.name == kHardwarePowerElementName) {
      hardware_power_element_control_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::ElementControl>(
              std::move(description.element_control_client.value()));
      hardware_power_lessor_client_ = fidl::WireSyncClient<fuchsia_power_broker::Lessor>(
          std::move(description.lessor_client.value()));
      hardware_power_current_level_client_ =
          fidl::WireSyncClient<fuchsia_power_broker::CurrentLevel>(
              std::move(description.current_level_client.value()));
      hardware_power_required_level_client_ = fidl::WireClient<fuchsia_power_broker::RequiredLevel>(
          std::move(description.required_level_client.value()), parent_->driver_async_dispatcher());
      hardware_power_element_assertive_token_ = std::move(description.assertive_token);
    } else {
      FDF_SLOG(ERROR, "Unexpected power element.", KV("index", i),
               KV("element-name", description.element_config.element.name));
      return zx::error(ZX_ERR_BAD_STATE);
    }
  }

  zx::result connect_to_cpu_element_manager =
      parent_->driver_incoming()->Connect<fuchsia_power_system::CpuElementManager>();
  if (connect_to_cpu_element_manager.is_error()) {
    // TODO (https://fxbug.dev/372507953) Return an error instead of just logging
    FDF_LOGL(INFO, logger(), "Registration skipped, CpuElementManager unavailable: %s",
             zx_status_get_string(connect_to_cpu_element_manager.error_value()));
  } else {
    fidl::SyncClient<fuchsia_power_system::CpuElementManager> cpu_element_manager(
        std::move(connect_to_cpu_element_manager.value()));
    zx::event clone;
    ZX_ASSERT(hardware_power_element_assertive_token_.duplicate(ZX_RIGHT_SAME_RIGHTS, &clone) ==
              ZX_OK);
    fidl::Result<fuchsia_power_system::CpuElementManager::AddExecutionStateDependency> result =
        cpu_element_manager->AddExecutionStateDependency(
            {{.dependency_token = std::move(clone), .power_level = 1}});
    if (result.is_error()) {
      // TODO (https://fxbug.dev/372507953) Return an error instead of just logging
      FDF_LOGL(ERROR, logger(), "CpuElementManager token registration failed: %s",
               result.error_value().FormatDescription().c_str());
    }
  }

  // The lease request on the hardware power element remains until we register
  // our power element token with the CpuElementManager protocol
  zx::result lease_control_client_end = AcquireInitLease(hardware_power_lessor_client_);
  if (!lease_control_client_end.is_ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to acquire lease on hardware power: %s",
             zx_status_get_string(lease_control_client_end.status_value()));
    return lease_control_client_end.take_error();
  }
  hardware_power_lease_control_client_end_ = std::move(lease_control_client_end.value());

  // Start continuous monitoring of the required level and adjusting of the hardware's power level.
  WatchHardwareRequiredLevel();

  return zx::success();
}

void SdmmcBlockDevice::WatchHardwareRequiredLevel() {
  fidl::Arena<> arena;
  hardware_power_required_level_client_.buffer(arena)->Watch().Then(
      [this](fidl::WireUnownedResult<fuchsia_power_broker::RequiredLevel::Watch>& result) {
        bool delay_before_next_watch = true;

        auto defer = fit::defer([&]() {
          if ((result.status() == ZX_ERR_CANCELED) || (result.status() == ZX_ERR_PEER_CLOSED)) {
            FDF_LOGL(WARNING, logger(), "Watch returned %s. Stop monitoring required power level.",
                     zx_status_get_string(result.status()));
          } else {
            if (delay_before_next_watch) {
              // TODO(b/339826112): Determine how to handle errors when communicating with the Power
              // Broker. For now, avoid overwhelming the Power Broker with calls.
              zx::nanosleep(zx::deadline_after(zx::msec(1)));
            }
            // Recursively call self to watch the required hardware power level again. The Watch()
            // call blocks until the required power level has changed.
            WatchHardwareRequiredLevel();
          }
        });

        if (!result.ok()) {
          FDF_LOGL(ERROR, logger(), "Call to Watch failed: %s", result.status_string());
          return;
        }
        if (result->is_error()) {
          switch (result->error_value()) {
            case fuchsia_power_broker::RequiredLevelError::kInternal:
              FDF_LOGL(ERROR, logger(), "Watch returned internal error.");
              break;
            case fuchsia_power_broker::RequiredLevelError::kNotAuthorized:
              FDF_LOGL(ERROR, logger(), "Watch returned not authorized error.");
              break;
            default:
              FDF_LOGL(ERROR, logger(), "Watch returned unknown error.");
              break;
          }
          return;
        }

        const fuchsia_power_broker::PowerLevel required_level = result->value()->required_level;
        switch (required_level) {
          case kPowerLevelOn:
          case kPowerLevelBoot: {
            const zx::time start = zx::clock::get_monotonic();

            fbl::AutoLock lock(&worker_lock_);
            // Actually raise the hardware's power level.
            zx_status_t status = ResumePower();
            if (status != ZX_OK) {
              const zx::duration duration = zx::clock::get_monotonic() - start;
              FDF_LOGL(ERROR, logger(), "Failed to resume power after %ld us: %s",
                       duration.to_usecs(), zx_status_get_string(status));
              return;
            }

            // If we're rising above the boot power level, it must because an
            // external lease raised our power level. This means we can drop
            // our self-lease and allow the external entity to drive our power
            // state.
            if (kPowerLevelOn && hardware_power_lease_control_client_end_.is_valid()) {
              hardware_power_lease_control_client_end_.reset();
            }

            // Communicate to Power Broker that the hardware power level has been raised.
            UpdatePowerLevel(hardware_power_current_level_client_, required_level);

            worker_condition_.Broadcast();
            break;
          }
          case kPowerLevelOff: {
            fbl::AutoLock lock(&worker_lock_);
            // Actually lower the hardware's power level.
            zx_status_t status = SuspendPower();
            if (status != ZX_OK) {
              FDF_LOGL(ERROR, logger(), "Failed to suspend power: %s",
                       zx_status_get_string(status));
              return;
            }
            // Communicate to Power Broker that the hardware power level has been lowered.
            UpdatePowerLevel(hardware_power_current_level_client_, kPowerLevelOff);
            break;
          }
          default:
            FDF_LOGL(ERROR, logger(), "Unexpected power level for hardware power element: %u",
                     required_level);
            return;
        }

        delay_before_next_watch = false;
      });
}

void SdmmcBlockDevice::StopWorkerDispatcher(std::optional<fdf::PrepareStopCompleter> completer) {
  if (worker_dispatcher_.get()) {
    {
      fbl::AutoLock worker_lock(&worker_lock_);
      shutdown_ = true;
    }
    worker_condition_.Broadcast();
    worker_event_.Signal();

    worker_dispatcher_.ShutdownAsync();
    worker_shutdown_completion_.Wait();
  }

  // error out all pending requests
  fbl::AutoLock lock(&queue_lock_);
  txn_list_.CompleteAll(ZX_ERR_CANCELED);

  for (auto& request : rpmb_list_) {
    request.completer.ReplyError(ZX_ERR_CANCELED);
  }
  rpmb_list_.clear();

  if (block_server_) {
    std::move(block_server_).value().DestroyAsync([completer = std::move(completer)]() mutable {
      if (completer.has_value())
        completer.value()(zx::ok());
    });
  } else if (completer.has_value()) {
    completer.value()(zx::ok());
  }
}

template <typename Request>
zx_status_t SdmmcBlockDevice::ReadWriteWithRetries(std::vector<Request>& requests,
                                                   const EmmcPartition partition) {
  zx_status_t st = SetPartition(partition);
  if (st != ZX_OK) {
    return st;
  }

  uint32_t attempts = 0;
  while (true) {
    attempts++;
    const bool last_attempt = attempts >= sdmmc_->kTryAttempts;

    st = ReadWriteAttempt(requests, !last_attempt);

    if (st == ZX_OK || last_attempt) {
      break;
    }
  }

  properties_.io_retries_.Add(attempts - 1);
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "do_txn error: %s", zx_status_get_string(st));
    properties_.io_errors_.Add(1);
  }

  FDF_LOGL(DEBUG, logger(), "do_txn complete");
  return st;
}

template <typename Request>
struct RequestAccessor {};

// This must not outlive the operation this references.
template <>
struct RequestAccessor<BlockOperation> {
  RequestAccessor(const BlockOperation* op) : rw(op->operation()->rw) {}

  bool is_read() const { return rw.command.opcode == BLOCK_OPCODE_READ; }
  uint64_t vmo_offset(uint32_t block_size) const { return rw.offset_vmo * block_size; }
  uint64_t device_block_offset() const { return rw.offset_dev; }
  uint32_t block_count() const { return rw.length; }
  zx_handle_t vmo() const { return rw.vmo; }

  const block_read_write_t& rw;
};

// This must not outlive the request this references.
template <>
struct RequestAccessor<block_server::Request> {
  RequestAccessor(const block_server::Request* request) : request(*request) {}

  bool is_read() const { return request.operation.tag == block_server::Operation::Tag::Read; }

  // These work for writes as well as reads; the fields are guaranteed to be in the same place.
  uint64_t vmo_offset(uint32_t block_size) const { return request.operation.read.vmo_offset; }
  uint64_t device_block_offset() const { return request.operation.read.device_block_offset; }
  uint32_t block_count() const { return request.operation.read.block_count; }

  zx_handle_t vmo() const { return request.vmo->get(); }

  const block_server::Request& request;
};

template <typename Request>
zx_status_t SdmmcBlockDevice::ReadWriteAttempt(std::vector<Request>& requests,
                                               bool suppress_error_messages) {
  // For single-block transfers, we could get higher performance by using SDMMC_READ_BLOCK/
  // SDMMC_WRITE_BLOCK without the need to SDMMC_SET_BLOCK_COUNT or SDMMC_STOP_TRANSMISSION.
  // However, we always do multiple-block transfers for simplicity.
  ZX_DEBUG_ASSERT(requests.size() >= 1);
  const RequestAccessor<Request> first_request(&requests[0]);
  const bool is_read = first_request.is_read();
  const bool command_packing = requests.size() > 1;
  const uint32_t cmd_idx = is_read ? SDMMC_READ_MULTIPLE_BLOCK : SDMMC_WRITE_MULTIPLE_BLOCK;
  const uint32_t cmd_flags =
      is_read ? SDMMC_READ_MULTIPLE_BLOCK_FLAGS : SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS;
  uint32_t total_data_transfer_blocks = 0;
  for (const auto& request : requests) {
    total_data_transfer_blocks += RequestAccessor<Request>(&request).block_count();
  }

  FDF_LOGL(DEBUG, logger(),
           "sdmmc: do_txn blockop %c offset_vmo 0x%" PRIx64
           " length 0x%x packing_count %zu blocksize 0x%x"
           " max_transfer_size 0x%x",
           first_request.is_read() ? 'R' : 'W', first_request.vmo_offset(block_info_.block_size),
           total_data_transfer_blocks, requests.size(), block_info_.block_size,
           block_info_.max_transfer_size);

  fdf::Arena arena('SDMC');
  fidl::VectorView<fuchsia_hardware_sdmmc::wire::SdmmcReq> reqs;
  if (!command_packing) {
    // TODO(https://fxbug.dev/42076962): Consider using SDMMC_CMD_AUTO23, which is likely to enhance
    // performance.
    reqs.Allocate(arena, 2);

    auto& set_block_count = reqs[0];
    set_block_count.cmd_idx = SDMMC_SET_BLOCK_COUNT;
    set_block_count.cmd_flags = SDMMC_SET_BLOCK_COUNT_FLAGS;
    set_block_count.arg = total_data_transfer_blocks;

    auto& rw_multiple_block = reqs[1];
    rw_multiple_block.cmd_idx = cmd_idx;
    rw_multiple_block.cmd_flags = cmd_flags;
    rw_multiple_block.arg = static_cast<uint32_t>(first_request.device_block_offset());
    rw_multiple_block.blocksize = block_info_.block_size;
    rw_multiple_block.buffers.Allocate(arena, 1);
    auto buffer_region =
        GetBufferRegion(first_request.vmo(), first_request.vmo_offset(block_info_.block_size),
                        first_request.block_count() * block_info_.block_size, logger());
    if (buffer_region.is_error()) {
      return buffer_region.status_value();
    }
    rw_multiple_block.buffers[0] = *std::move(buffer_region);
  } else {
    // Form packed command header (section 6.6.29.1, eMMC standard 5.1)
    readwrite_metadata_.packed_command_header_data->rw = is_read ? 1 : 2;
    // Safe because requests.size() <= kMaxPackedCommandsFor512ByteBlockSize.
    readwrite_metadata_.packed_command_header_data->num_entries =
        safemath::checked_cast<uint8_t>(requests.size());

    // TODO(https://fxbug.dev/42083080): Consider pre-registering the packed command header VMO with
    // the SDMMC driver to avoid pinning and unpinning for each transfer. Also handle the cache ops
    // here.
    // Packed write: SET_BLOCK_COUNT (header+data) -> WRITE_MULTIPLE_BLOCK (header+data)
    // Packed read: SET_BLOCK_COUNT (header) -> WRITE_MULTIPLE_BLOCK (header) ->
    //              SET_BLOCK_COUNT (data) -> READ_MULTIPLE_BLOCK (data)
    int request_index_offset;
    if (!is_read) {
      request_index_offset = 0;
      reqs.Allocate(arena, 2);
    } else {
      request_index_offset = 2;
      reqs.Allocate(arena, 4);

      auto& set_block_count = reqs[0];
      set_block_count.cmd_idx = SDMMC_SET_BLOCK_COUNT;
      set_block_count.cmd_flags = SDMMC_SET_BLOCK_COUNT_FLAGS;
      set_block_count.arg = MMC_SET_BLOCK_COUNT_PACKED | 1;  // 1 header block.

      auto& write_multiple_block = reqs[1];
      write_multiple_block.cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK;
      write_multiple_block.cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS;
      write_multiple_block.arg = static_cast<uint32_t>(first_request.device_block_offset());
      write_multiple_block.blocksize = block_info_.block_size;
      write_multiple_block.buffers.Allocate(arena, 1);  // 1 header block.
      // The first buffer region points to the header (packed read case).
      auto buffer_region = GetBufferRegion(readwrite_metadata_.packed_command_header_vmo.get(), 0,
                                           block_info_.block_size, logger());
      if (buffer_region.is_error()) {
        return buffer_region.status_value();
      }
      write_multiple_block.buffers[0] = *std::move(buffer_region);
    }

    auto& set_block_count = reqs[request_index_offset];
    set_block_count.cmd_idx = SDMMC_SET_BLOCK_COUNT;
    set_block_count.cmd_flags = SDMMC_SET_BLOCK_COUNT_FLAGS;
    set_block_count.arg = MMC_SET_BLOCK_COUNT_PACKED |
                          (is_read ? total_data_transfer_blocks
                                   : (total_data_transfer_blocks + 1));  // +1 for header block.

    auto& rw_multiple_block = reqs[request_index_offset + 1];
    rw_multiple_block.cmd_idx = cmd_idx;
    rw_multiple_block.cmd_flags = cmd_flags;
    rw_multiple_block.arg = static_cast<uint32_t>(first_request.device_block_offset());
    rw_multiple_block.blocksize = block_info_.block_size;

    int buffer_index_offset;
    if (is_read) {
      buffer_index_offset = 0;
      rw_multiple_block.buffers.Allocate(arena, requests.size());
    } else {
      buffer_index_offset = 1;
      rw_multiple_block.buffers.Allocate(arena, requests.size() + 1);  // +1 for header block.
      // The first buffer region points to the header (packed write case).
      auto buffer_region = GetBufferRegion(readwrite_metadata_.packed_command_header_vmo.get(), 0,
                                           block_info_.block_size, logger());
      if (buffer_region.is_error()) {
        return buffer_region.status_value();
      }
      rw_multiple_block.buffers[0] = *std::move(buffer_region);
    }

    // The following buffer regions point to the data.
    for (size_t i = 0; i < requests.size(); i++) {
      const RequestAccessor<Request> request(&requests[i]);
      readwrite_metadata_.packed_command_header_data->arg[i].cmd23_arg = request.block_count();
      readwrite_metadata_.packed_command_header_data->arg[i].cmdXX_arg =
          static_cast<uint32_t>(request.device_block_offset());

      auto buffer_region =
          GetBufferRegion(request.vmo(), request.vmo_offset(block_info_.block_size),
                          request.block_count() * block_info_.block_size, logger());
      if (buffer_region.is_error()) {
        return buffer_region.status_value();
      }
      rw_multiple_block.buffers[buffer_index_offset + i] = *std::move(buffer_region);
    }
  }

  for (auto& req : reqs) {
    req.suppress_error_messages = suppress_error_messages;
  }
  return sdmmc_->SdmmcIoRequest(std::move(arena), reqs, readwrite_metadata_.buffer_regions.get());
}

zx_status_t SdmmcBlockDevice::Flush() {
  if (!cache_enabled_) {
    return ZX_OK;
  }

  // TODO(https://fxbug.dev/42075502): Enable the cache and add flush support for SD.
  ZX_ASSERT(!is_sd_);

  zx_status_t st = MmcDoSwitch(MMC_EXT_CSD_FLUSH_CACHE, MMC_EXT_CSD_FLUSH_MASK);
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to flush the cache: %s", zx_status_get_string(st));
  }
  return st;
}

zx_status_t SdmmcBlockDevice::Trim(const block_trim_t& txn, const EmmcPartition partition) {
  // TODO(b/312236221): Add trim support for SD.
  if (is_sd_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (!(block_info_.flags & FLAG_TRIM_SUPPORT)) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t status = SetPartition(partition);
  if (status != ZX_OK) {
    return status;
  }

  constexpr uint32_t kEraseErrorFlags =
      MMC_STATUS_ADDR_OUT_OF_RANGE | MMC_STATUS_ERASE_SEQ_ERR | MMC_STATUS_ERASE_PARAM;

  const sdmmc_req_t trim_start = {
      .cmd_idx = MMC_ERASE_GROUP_START,
      .cmd_flags = MMC_ERASE_GROUP_START_FLAGS,
      .arg = static_cast<uint32_t>(txn.offset_dev),
  };
  uint32_t response[4] = {};
  if ((status = sdmmc_->Request(&trim_start, response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to set trim group start: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }
  if (response[0] & kEraseErrorFlags) {
    FDF_LOGL(ERROR, logger(), "card reported trim group start error: 0x%08x", response[0]);
    properties_.io_errors_.Add(1);
    return ZX_ERR_IO;
  }

  const sdmmc_req_t trim_end = {
      .cmd_idx = MMC_ERASE_GROUP_END,
      .cmd_flags = MMC_ERASE_GROUP_END_FLAGS,
      .arg = static_cast<uint32_t>(txn.offset_dev + txn.length - 1),
  };
  if ((status = sdmmc_->Request(&trim_end, response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to set trim group end: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }
  if (response[0] & kEraseErrorFlags) {
    FDF_LOGL(ERROR, logger(), "card reported trim group end error: 0x%08x", response[0]);
    properties_.io_errors_.Add(1);
    return ZX_ERR_IO;
  }

  const sdmmc_req_t trim = {
      .cmd_idx = SDMMC_ERASE,
      .cmd_flags = SDMMC_ERASE_FLAGS,
      .arg = MMC_ERASE_TRIM_ARG,
  };
  if ((status = sdmmc_->Request(&trim, response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "trim failed: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }
  if (response[0] & kEraseErrorFlags) {
    FDF_LOGL(ERROR, logger(), "card reported trim error: 0x%08x", response[0]);
    properties_.io_errors_.Add(1);
    return ZX_ERR_IO;
  }

  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::RpmbRequest(const RpmbRequestInfo& request) {
  // TODO(https://fxbug.dev/42166356): Find out if RPMB requests can be retried.
  using fuchsia_hardware_rpmb::wire::kFrameSize;

  const uint64_t tx_frame_count = request.tx_frames.size / kFrameSize;
  const uint64_t rx_frame_count =
      request.rx_frames.vmo.is_valid() ? (request.rx_frames.size / kFrameSize) : 0;
  const bool read_needed = rx_frame_count > 0;

  zx_status_t status = SetPartition(RPMB_PARTITION);
  if (status != ZX_OK) {
    return status;
  }

  const sdmmc_req_t set_tx_block_count = {
      .cmd_idx = SDMMC_SET_BLOCK_COUNT,
      .cmd_flags = SDMMC_SET_BLOCK_COUNT_FLAGS,
      .arg = MMC_SET_BLOCK_COUNT_RELIABLE_WRITE | static_cast<uint32_t>(tx_frame_count),
  };
  uint32_t unused_response[4];
  if ((status = sdmmc_->Request(&set_tx_block_count, unused_response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to set block count for RPMB request: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }

  const sdmmc_buffer_region_t write_region = {
      .buffer = {.vmo = request.tx_frames.vmo.get()},
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = request.tx_frames.offset,
      .size = tx_frame_count * kFrameSize,
  };
  const sdmmc_req_t write_tx_frames = {
      .cmd_idx = SDMMC_WRITE_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_WRITE_MULTIPLE_BLOCK_FLAGS,
      .arg = 0,  // Ignored by the card.
      .blocksize = kFrameSize,
      .buffers_list = &write_region,
      .buffers_count = 1,
  };
  if ((status = sdmmc_->Request(&write_tx_frames, unused_response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to write RPMB frames: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }

  if (!read_needed) {
    return ZX_OK;
  }

  const sdmmc_req_t set_rx_block_count = {
      .cmd_idx = SDMMC_SET_BLOCK_COUNT,
      .cmd_flags = SDMMC_SET_BLOCK_COUNT_FLAGS,
      .arg = static_cast<uint32_t>(rx_frame_count),
  };
  if ((status = sdmmc_->Request(&set_rx_block_count, unused_response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to set block count for RPMB request: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }

  const sdmmc_buffer_region_t read_region = {
      .buffer = {.vmo = request.rx_frames.vmo.get()},
      .type = SDMMC_BUFFER_TYPE_VMO_HANDLE,
      .offset = request.rx_frames.offset,
      .size = rx_frame_count * kFrameSize,
  };
  const sdmmc_req_t read_rx_frames = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0,
      .blocksize = kFrameSize,
      .buffers_list = &read_region,
      .buffers_count = 1,
  };
  if ((status = sdmmc_->Request(&read_rx_frames, unused_response)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to read RPMB frames: %d", status);
    properties_.io_errors_.Add(1);
    return status;
  }

  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::SetPartition(const EmmcPartition partition) {
  if (is_sd_ || partition == current_partition_) {
    return ZX_OK;
  }

  const uint8_t partition_config_value =
      (raw_ext_csd_[MMC_EXT_CSD_PARTITION_CONFIG] & MMC_EXT_CSD_PARTITION_ACCESS_MASK) | partition;

  zx_status_t status = MmcDoSwitch(MMC_EXT_CSD_PARTITION_CONFIG, partition_config_value);
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to switch to partition %u", partition);
    properties_.io_errors_.Add(1);
    return status;
  }

  current_partition_ = partition;
  return ZX_OK;
}

void SdmmcBlockDevice::Queue(BlockOperation txn) {
  block_op_t* btxn = txn.operation();

  const uint64_t max = txn.private_storage()->block_count;
  switch (btxn->command.opcode) {
    case BLOCK_OPCODE_READ:
    case BLOCK_OPCODE_WRITE:
      if (zx_status_t status = block::CheckIoRange(btxn->rw, max, logger()); status != ZX_OK) {
        BlockComplete(txn, status);
        return;
      }
      // MMC supports FUA writes, but not FUA reads. SD does not support FUA.
      if (btxn->command.flags & BLOCK_IO_FLAG_FORCE_ACCESS) {
        BlockComplete(txn, ZX_ERR_NOT_SUPPORTED);
        return;
      }
      break;
    case BLOCK_OPCODE_TRIM:
      if (zx_status_t status = block::CheckIoRange(btxn->trim, max, logger()); status != ZX_OK) {
        BlockComplete(txn, status);
        return;
      }
      break;
    case BLOCK_OPCODE_FLUSH:
      // queue the flush op. because there is no out of order execution in this
      // driver, when this op gets processed all previous ops are complete.
      break;
    default:
      BlockComplete(txn, ZX_ERR_NOT_SUPPORTED);
      return;
  }

  {
    fbl::AutoLock lock(&queue_lock_);
    txn_list_.push(std::move(txn));
  }

  // Wake up the worker thread.
  worker_event_.Signal();
}

void SdmmcBlockDevice::RpmbQueue(RpmbRequestInfo info) {
  using fuchsia_hardware_rpmb::wire::kFrameSize;

  if (info.tx_frames.size % kFrameSize != 0) {
    FDF_LOGL(ERROR, logger(), "tx frame buffer size not a multiple of %u", kFrameSize);
    info.completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  // Checking against SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS is sufficient for casting to uint16_t.
  static_assert(SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS <= UINT16_MAX);

  const uint64_t tx_frame_count = info.tx_frames.size / kFrameSize;
  if (tx_frame_count == 0) {
    info.completer.ReplyError(ZX_OK);
    return;
  }

  if (tx_frame_count > SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS) {
    FDF_LOGL(ERROR, logger(), "received %lu tx frames, maximum is %u", tx_frame_count,
             SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS);
    info.completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
    return;
  }

  if (info.rx_frames.vmo.is_valid()) {
    if (info.rx_frames.size % kFrameSize != 0) {
      FDF_LOGL(ERROR, logger(), "rx frame buffer size is not a multiple of %u", kFrameSize);
      info.completer.ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }

    const uint64_t rx_frame_count = info.rx_frames.size / kFrameSize;
    if (rx_frame_count > SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS) {
      FDF_LOGL(ERROR, logger(), "received %lu rx frames, maximum is %u", rx_frame_count,
               SDMMC_SET_BLOCK_COUNT_MAX_BLOCKS);
      info.completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
      return;
    }
  }

  fbl::AutoLock lock(&queue_lock_);
  if (rpmb_list_.size() >= kMaxOutstandingRpmbRequests) {
    info.completer.ReplyError(ZX_ERR_SHOULD_WAIT);
  } else {
    rpmb_list_.push_back(std::move(info));
    lock.release();
    worker_event_.Signal();
  }
}

void SdmmcBlockDevice::HandleBlockOps(block::BorrowedOperationQueue<PartitionInfo>& txn_list) {
  for (size_t i = 0; i < kRoundRobinRequestCount; i++) {
    std::optional<BlockOperation> txn = txn_list.pop();
    if (!txn) {
      break;
    }

    std::vector<BlockOperation> btxns;
    btxns.push_back(*std::move(txn));

    const block_op_t& bop = *btxns[0].operation();
    const uint8_t op = bop.command.opcode;
    const EmmcPartition partition = btxns[0].private_storage()->partition;

    zx_status_t status = ZX_ERR_INVALID_ARGS;
    if (op == BLOCK_OPCODE_READ || op == BLOCK_OPCODE_WRITE) {
      const char* const trace_name = op == BLOCK_OPCODE_READ ? "read" : "write";
      TRACE_DURATION_BEGIN("sdmmc", trace_name);

      // Consider trailing txns for eMMC Command Packing (batching)
      if (partition == USER_DATA_PARTITION) {
        const uint32_t max_command_packing =
            (op == BLOCK_OPCODE_READ) ? max_packed_reads_effective_ : max_packed_writes_effective_;
        // The system page size is used below, because the header block requires its own
        // scatter-gather transfer descriptor in the lower-level SDMMC driver.
        uint64_t cum_transfer_bytes = (bop.rw.length * block_info_.block_size) +
                                      zx_system_get_page_size();  // +1 page for header block.
        while (btxns.size() < max_command_packing) {
          // TODO(https://fxbug.dev/42083080): It's inefficient to pop() here only to push() later
          // in the case of packing ineligibility. Later on, we'll likely move away from using
          // block::BorrowedOperationQueue once we start using the FIDL driver transport arena (at
          // which point, use something like peek() instead).
          std::optional<BlockOperation> pack_candidate_txn = txn_list.pop();
          if (!pack_candidate_txn) {
            // No more candidate txns to consider for packing.
            break;
          }

          cum_transfer_bytes += pack_candidate_txn->operation()->rw.length * block_info_.block_size;
          // TODO(https://fxbug.dev/42083080): Explore reordering commands for more command packing.
          if (pack_candidate_txn->operation()->command.opcode != bop.command.opcode ||
              pack_candidate_txn->private_storage()->partition != partition ||
              cum_transfer_bytes > block_info_.max_transfer_size) {
            // Candidate txn is ineligible for packing.
            txn_list.push(std::move(*pack_candidate_txn));
            break;
          }

          btxns.push_back(std::move(*pack_candidate_txn));
        }
      }

      status = ReadWriteWithRetries(btxns, partition);

      TRACE_DURATION_END("sdmmc", trace_name, "opcode", TA_INT32(bop.rw.command.opcode), "extra",
                         TA_INT32(bop.rw.extra), "length", TA_INT32(bop.rw.length), "offset_vmo",
                         TA_INT64(bop.rw.offset_vmo), "offset_dev", TA_INT64(bop.rw.offset_dev),
                         "txn_status", TA_INT32(status));
    } else if (op == BLOCK_OPCODE_TRIM) {
      TRACE_DURATION_BEGIN("sdmmc", "trim");

      status = Trim(bop.trim, partition);

      TRACE_DURATION_END("sdmmc", "trim", "opcode", TA_INT32(bop.trim.command.opcode), "length",
                         TA_INT32(bop.trim.length), "offset_dev", TA_INT64(bop.trim.offset_dev),
                         "txn_status", TA_INT32(status));
    } else if (op == BLOCK_OPCODE_FLUSH) {
      TRACE_DURATION_BEGIN("sdmmc", "flush");

      status = Flush();

      TRACE_DURATION_END("sdmmc", "flush", "opcode", TA_INT32(bop.command.opcode), "txn_status",
                         TA_INT32(status));
    } else {
      // should not get here
      FDF_LOGL(ERROR, logger(), "invalid block op %d", op);
      TRACE_INSTANT("sdmmc", "unknown", TRACE_SCOPE_PROCESS, "opcode",
                    TA_INT32(bop.rw.command.opcode), "txn_status", TA_INT32(status));
      __UNREACHABLE;
    }

    for (auto& btxn : btxns) {
      BlockComplete(btxn, status);
    }
  }
}

void SdmmcBlockDevice::HandleRpmbRequests(std::deque<RpmbRequestInfo>& rpmb_list) {
  for (size_t i = 0; i < kRoundRobinRequestCount && !rpmb_list.empty(); i++) {
    RpmbRequestInfo& request = *rpmb_list.begin();
    zx_status_t status = RpmbRequest(request);
    if (status == ZX_OK) {
      request.completer.ReplySuccess();
    } else {
      request.completer.ReplyError(status);
    }

    rpmb_list.pop_front();
  }
}

void SdmmcBlockDevice::WorkerLoop() {
  block::BorrowedOperationQueue<PartitionInfo> txn_list;
  std::deque<RpmbRequestInfo> rpmb_list;

  for (;;) {
    fbl::AutoLock worker_lock(&worker_lock_);
    while (power_suspended_ && !shutdown_)
      worker_condition_.Wait(&worker_lock_);
    if (shutdown_)
      break;

    {
      fbl::AutoLock lock(&queue_lock_);
      if (txn_list_.is_empty() && rpmb_list_.empty()) {
        worker_event_.Reset();
        lock.release();
        worker_lock.release();
        worker_event_.Wait();
        continue;
      }

      txn_list = std::move(txn_list_);
      rpmb_list.swap(rpmb_list_);
    }

    TRACE_DURATION("sdmmc", "work loop");

    while (!txn_list.is_empty() || !rpmb_list.empty()) {
      HandleBlockOps(txn_list);
      HandleRpmbRequests(rpmb_list);
    }
  }

  FDF_LOGL(DEBUG, logger(), "worker thread terminated successfully");
}

zx_status_t SdmmcBlockDevice::SuspendPower() {
  if (power_suspended_ == true) {
    return ZX_OK;
  }

  if (zx_status_t status = Flush(); status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to flush: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = sdmmc_->MmcSelectCard(/*select=*/false); status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to (de-)SelectCard before sleep: %s",
             zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = sdmmc_->MmcSleepOrAwake(/*sleep=*/true); status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to sleep: %s", zx_status_get_string(status));
    return status;
  }

  trace_async_id_ = TRACE_NONCE();
  TRACE_ASYNC_BEGIN("sdmmc", "suspend", trace_async_id_);
  power_suspended_ = true;
  properties_.power_suspended_.Set(power_suspended_);
  FDF_LOGL(INFO, logger(), "Power suspended.");
  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::ResumePower() {
  if (power_suspended_ == false) {
    return ZX_OK;
  }

  if (zx_status_t status = sdmmc_->MmcSleepOrAwake(/*sleep=*/false); status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to awake: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = sdmmc_->MmcSelectCard(/*select=*/true); status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to SelectCard after awake: %s", zx_status_get_string(status));
    return status;
  }

  TRACE_ASYNC_END("sdmmc", "suspend", trace_async_id_);
  power_suspended_ = false;
  properties_.power_suspended_.Set(power_suspended_);

  FDF_LOGL(INFO, logger(), "Power resumed.");
  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::WaitForTran() {
  uint32_t current_state;
  size_t attempt = 0;
  for (; attempt <= kTranMaxAttempts; attempt++) {
    uint32_t response;
    zx_status_t st = sdmmc_->SdmmcSendStatus(&response);
    if (st != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "SDMMC_SEND_STATUS error, retcode = %d", st);
      return st;
    }

    current_state = MMC_STATUS_CURRENT_STATE(response);
    if (current_state == MMC_STATUS_CURRENT_STATE_RECV) {
      st = sdmmc_->SdmmcStopTransmission();
      continue;
    } else if (current_state == MMC_STATUS_CURRENT_STATE_TRAN) {
      break;
    }

    zx::nanosleep(zx::deadline_after(zx::msec(10)));
  }

  if (attempt == kTranMaxAttempts) {
    // Too many retries, fail.
    return ZX_ERR_TIMED_OUT;
  } else {
    return ZX_OK;
  }
}

void SdmmcBlockDevice::SetBlockInfo(uint32_t block_size, uint64_t block_count) {
  block_info_.block_size = block_size;
  block_info_.block_count = block_count;
}

const inspect::Inspector& SdmmcBlockDevice::inspect() const {
  return parent_->driver_inspector().inspector();
}

fdf::Logger& SdmmcBlockDevice::logger() const { return parent_->logger(); }

void SdmmcBlockDevice::StartThread(block_server::Thread thread) {
  if (auto server_dispatcher = fdf::SynchronizedDispatcher::Create(
          fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "SDMMC Block Server",
          [&](fdf_dispatcher_t* dispatcher) { fdf_dispatcher_destroy(dispatcher); });
      server_dispatcher.is_ok()) {
    async::PostTask(server_dispatcher->async_dispatcher(),
                    [thread = std::move(thread)]() mutable { thread.Run(); });

    // The dispatcher is destroyed in the shutdown handler.
    server_dispatcher->release();
  }
}

void SdmmcBlockDevice::OnNewSession(block_server::Session session) {
  if (auto server_dispatcher = fdf::SynchronizedDispatcher::Create(
          fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "Block Server Session",
          [&](fdf_dispatcher_t* dispatcher) { fdf_dispatcher_destroy(dispatcher); });
      server_dispatcher.is_ok()) {
    async::PostTask(server_dispatcher->async_dispatcher(),
                    [session = std::move(session)]() mutable { session.Run(); });

    // The dispatcher is destroyed in the shutdown handler.
    server_dispatcher->release();
  }
}

// For now this only handles requests for the user partition.
void SdmmcBlockDevice::OnRequests(const block_server::Session& session,
                                  cpp20::span<block_server::Request> requests) {
  fbl::AutoLock lock(&worker_lock_);
  while (power_suspended_ && !shutdown_)
    worker_condition_.Wait(&worker_lock_);
  if (shutdown_)
    return;

  class Packer {
   public:
    Packer(SdmmcBlockDevice* device, const block_server::Session* session, size_t max_requests,
           int max_bytes, uint32_t block_size)
        : device_(*device),
          session_(*session),
          max_requests_(max_requests),
          max_bytes_(max_bytes),
          block_size_(block_size) {}

    void Push(block_server::Request& request) {
      uint64_t bytes = request.operation.read.block_count * block_size_;

      if (bytes == 0)
        return;

      // We need extra when we go from 1 to 2 requests.
      uint64_t extra = requests_.size() == 1 ? zx_system_get_page_size() : 0;

      if (max_bytes_ > 0) {
        for (;;) {
          const uint64_t space = max_bytes_ - total_bytes_;
          if (bytes + extra <= space) {
            break;
          }

          if (space > extra) {
            uint64_t amount = space - extra;
            requests_.push_back(block_server::SplitRequest(
                request, static_cast<uint32_t>(amount / block_size_), block_size_));
            bytes -= amount;
          }

          if (auto result = Flush(/*split_last=*/true); result.is_error()) {
            // The partial request failed which means we ignore the rest of the request.
            return;
          }

          extra = 0;
        }
      }

      requests_.push_back(request);
      total_bytes_ += bytes + extra;
      if (requests_.size() >= max_requests_ || total_bytes_ == max_bytes_) {
        [[maybe_unused]] auto result = Flush();
      }
    }

    // Unfortunately, there's no way for us to tell the compiler that we hold `worker_lock_` here,
    // so we have to skip thread safety analysis.  If `split_last` is true, the last request is a
    // partial request and so the response is not sent if successful since the caller will want to
    // finish the request in the next batch.  If there is a failure, the response is sent and the
    // caller is expected to discard the remaining request.
    zx::result<> Flush(bool split_last = false) TA_NO_THREAD_SAFETY_ANALYSIS {
      if (requests_.empty())
        return zx::ok();
      zx::result<> result = zx::make_result(
          device_.ReadWriteWithRetries(requests_, EmmcPartition::USER_DATA_PARTITION));
      if (split_last && result.is_ok())
        requests_.pop_back();
      for (const block_server::Request& request : requests_) {
        session_.SendReply(request.request_id, request.trace_flow_id, result);
      }
      requests_.clear();
      total_bytes_ = 0;
      return result;
    }

   private:
    SdmmcBlockDevice& device_;
    const block_server::Session& session_;
    const size_t max_requests_;
    const uint64_t max_bytes_;
    uint32_t block_size_;
    std::vector<block_server::Request> requests_;
    uint64_t total_bytes_ = 0;
  };

  zx_status_t status;
  Packer read_packer(this, &session, max_packed_reads_effective_, block_info_.max_transfer_size,
                     block_info_.block_size);
  Packer write_packer(this, &session, max_packed_writes_effective_, block_info_.max_transfer_size,
                      block_info_.block_size);

  [[maybe_unused]] zx::result<> unused_result;

  for (block_server::Request& request : requests) {
    switch (request.operation.tag) {
      case block_server::Operation::Tag::Read:
        read_packer.Push(request);
        break;
      case block_server::Operation::Tag::Write:
        write_packer.Push(request);
        break;

      case block_server::Operation::Tag::Flush:
        TRACE_DURATION_BEGIN("sdmmc", "flush");

        // Technically, we might not need to do this, because there's no guarantee regarding the
        // ordering of requests, but it's arguably safer for us to flush preceding write requests
        // before issuing the flush command.
        unused_result = write_packer.Flush();

        status = Flush();

        session.SendReply(request.request_id, request.trace_flow_id, zx::make_result(status));

        TRACE_DURATION_END("sdmmc", "flush", "opcode",
                           TA_INT32(static_cast<int32_t>(request.operation.tag)), "txn_status",
                           TA_INT32(status));
        break;

      case block_server::Operation::Tag::Trim:
        TRACE_DURATION_BEGIN("sdmmc", "trim");

        status = Trim(
            block_trim_t{
                .length = request.operation.trim.block_count,
                .offset_dev = request.operation.trim.device_block_offset,
            },
            USER_DATA_PARTITION);
        session.SendReply(request.request_id, request.trace_flow_id, zx::make_result(status));

        TRACE_DURATION_END(
            "sdmmc", "trim", "opcode", TA_INT32(static_cast<int32_t>(request.operation.tag)),
            "length", TA_INT32(request.operation.trim.block_count), "offset_dev",
            TA_INT64(request.operation.trim.device_block_offset), "txn_status", TA_INT32(status));
        break;

      case block_server::Operation::Tag::CloseVmo:
        __UNREACHABLE;
    }
  }

  unused_result = read_packer.Flush();
  unused_result = write_packer.Flush();
}

}  // namespace sdmmc
