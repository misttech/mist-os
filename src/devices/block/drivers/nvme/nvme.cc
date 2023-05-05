// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/nvme/nvme.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/device-protocol/pci.h>
#include <lib/fit/defer.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/sync/completion.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <threads.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <hwreg/bitfields.h>

#include "src/devices/block/drivers/nvme/commands/features.h"
#include "src/devices/block/drivers/nvme/commands/identify.h"
#include "src/devices/block/drivers/nvme/commands/nvme-io.h"
#include "src/devices/block/drivers/nvme/commands/queue.h"
#include "src/devices/block/drivers/nvme/namespace.h"
#include "src/devices/block/drivers/nvme/registers.h"

namespace nvme {

// For safety - so that the driver doesn't draw too many resources.
constexpr size_t kMaxNamespacesToBind = 4;

// c.f. NVMe Base Specification 2.0, section 3.1.3.8 "AQA - Admin Queue Attributes"
constexpr size_t kAdminQueueMaxEntries = 4096;

// TODO(fxbug.dev/102133): Consider using interrupt vector - dedicated interrupt (and IO thread) per
// namespace/queue.
int Nvme::IrqLoop() {
  while (true) {
    zx_status_t status = irq_.wait(nullptr);
    if (status != ZX_OK) {
      if (status == ZX_ERR_CANCELED) {
        zxlogf(DEBUG, "Interrupt cancelled. Exiting IRQ loop.");
      } else {
        zxlogf(ERROR, "Failed to wait for interrupt: %s", zx_status_get_string(status));
      }
      break;
    }

    // The interrupt mask register should only be used when not using MSI-X.
    if (irq_mode_ != fuchsia_hardware_pci::InterruptMode::kMsiX) {
      InterruptReg::MaskSet().FromValue(1).WriteTo(&mmio_);
    }

    Completion* admin_completion;
    if (admin_queue_->CheckForNewCompletion(&admin_completion) != ZX_ERR_SHOULD_WAIT) {
      admin_result_ = *admin_completion;
      sync_completion_signal(&admin_signal_);
      admin_queue_->RingCompletionDb();
    }

    sync_completion_signal(&io_signal_);

    if (irq_mode_ != fuchsia_hardware_pci::InterruptMode::kMsiX) {
      // Unmask the interrupt.
      InterruptReg::MaskClear().FromValue(1).WriteTo(&mmio_);
    }

    if (irq_mode_ == fuchsia_hardware_pci::InterruptMode::kLegacy) {
      status = pci_.AckInterrupt();
      if (status != ZX_OK) {
        zxlogf(ERROR, "Failed to ack interrupt: %s", zx_status_get_string(status));
        break;
      }
    }
  }
  return thrd_success;
}

zx_status_t Nvme::DoAdminCommandSync(Submission& submission,
                                     std::optional<zx::unowned_vmo> admin_data) {
  zx_status_t status;
  fbl::AutoLock lock(&admin_lock_);
  sync_completion_reset(&admin_signal_);

  uint64_t data_size = 0;
  if (admin_data.has_value()) {
    status = admin_data.value()->get_size(&data_size);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to get size of vmo: %s", zx_status_get_string(status));
      return status;
    }
  }
  status = admin_queue_->Submit(submission, admin_data, 0, data_size, nullptr);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to submit admin command: %s", zx_status_get_string(status));
    return status;
  }

  status = sync_completion_wait(&admin_signal_, ZX_SEC(10));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Timed out waiting for admin command: %s", zx_status_get_string(status));
    return status;
  }

  if (admin_result_.status_code_type() == StatusCodeType::kGeneric &&
      admin_result_.status_code() == 0) {
    zxlogf(TRACE, "Completed admin command OK.");
  } else {
    zxlogf(ERROR, "Completed admin command ERROR: status type=%01x, status=%02x",
           admin_result_.status_code_type(), admin_result_.status_code());
    return ZX_ERR_IO;
  }
  return ZX_OK;
}

void Nvme::ProcessIoSubmissions() {
  while (true) {
    IoCommand* io_cmd;
    {
      fbl::AutoLock lock(&commands_lock_);
      io_cmd = list_remove_head_type(&pending_commands_, IoCommand, node);
    }

    if (io_cmd == nullptr) {
      return;
    }

    zx_status_t status;
    const auto opcode = io_cmd->op.command & BLOCK_OP_MASK;
    if (opcode == BLOCK_OP_FLUSH) {
      NvmIoFlushSubmission submission;
      submission.namespace_id = io_cmd->namespace_id;

      status = io_queue_->Submit(submission, std::nullopt, 0, 0, io_cmd);
    } else {
      NvmIoSubmission submission(opcode == BLOCK_OP_WRITE);
      submission.namespace_id = io_cmd->namespace_id;
      submission.set_start_lba(io_cmd->op.rw.offset_dev).set_block_count(io_cmd->op.rw.length - 1);
      if (io_cmd->op.command & BLOCK_FL_FORCE_ACCESS) {
        submission.set_force_unit_access(true);
      }

      // Convert op.rw.offset_vmo and op.rw.length to bytes.
      status = io_queue_->Submit(submission, zx::unowned_vmo(io_cmd->op.rw.vmo),
                                 io_cmd->op.rw.offset_vmo * io_cmd->block_size_bytes,
                                 io_cmd->op.rw.length * io_cmd->block_size_bytes, io_cmd);
    }
    switch (status) {
      case ZX_OK:
        break;
      case ZX_ERR_SHOULD_WAIT:
        // We can't proceed if there is no available space in the submission queue. Put command back
        // at front of queue for further processing later.
        {
          fbl::AutoLock lock(&commands_lock_);
          list_add_head(&pending_commands_, &io_cmd->node);
        }
        return;
      default:
        zxlogf(ERROR, "Failed to submit transaction (command %p): %s", io_cmd,
               zx_status_get_string(status));
        io_cmd->Complete(ZX_ERR_INTERNAL);
        break;
    }
  }
}

void Nvme::ProcessIoCompletions() {
  bool ring_doorbell = false;
  Completion* completion = nullptr;
  IoCommand* io_cmd = nullptr;
  while (io_queue_->CheckForNewCompletion(&completion, &io_cmd) != ZX_ERR_SHOULD_WAIT) {
    ring_doorbell = true;

    if (io_cmd == nullptr) {
      zxlogf(ERROR, "Completed transaction isn't associated with a command.");
      continue;
    }

    if (completion->status_code_type() == StatusCodeType::kGeneric &&
        completion->status_code() == 0) {
      zxlogf(TRACE, "Completed transaction #%u command %p OK.", completion->command_id(), io_cmd);
      io_cmd->Complete(ZX_OK);
    } else {
      zxlogf(ERROR, "Completed transaction #%u command %p ERROR: status type=%01x, status=%02x",
             completion->command_id(), io_cmd, completion->status_code_type(),
             completion->status_code());
      io_cmd->Complete(ZX_ERR_IO);
    }
  }

  if (ring_doorbell) {
    io_queue_->RingCompletionDb();
  }
}

int Nvme::IoLoop() {
  while (true) {
    if (driver_shutdown_) {  // Check this outside of io_signal_ wait-reset below to avoid deadlock.
      zxlogf(DEBUG, "IO thread exiting.");
      break;
    }

    zx_status_t status = sync_completion_wait(&io_signal_, ZX_TIME_INFINITE);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to wait for sync completion: %s", zx_status_get_string(status));
      break;
    }
    sync_completion_reset(&io_signal_);

    // process completion messages
    ProcessIoCompletions();

    // process work queue
    ProcessIoSubmissions();
  }
  return thrd_success;
}

void Nvme::QueueIoCommand(IoCommand* io_cmd) {
  {
    fbl::AutoLock lock(&commands_lock_);
    list_add_tail(&pending_commands_, &io_cmd->node);
  }

  sync_completion_signal(&io_signal_);
}

void Nvme::DdkRelease() {
  zxlogf(DEBUG, "Releasing driver.");
  driver_shutdown_ = true;
  if (mmio_.get_vmo() != ZX_HANDLE_INVALID) {
    pci_.SetBusMastering(false);
  }
  irq_.destroy();  // Make irq_.wait() in IrqLoop() return ZX_ERR_CANCELED.
  if (irq_thread_started_) {
    thrd_join(irq_thread_, nullptr);
  }
  if (io_thread_started_) {
    sync_completion_signal(&io_signal_);
    thrd_join(io_thread_, nullptr);
  }

  // Error out any pending commands
  {
    fbl::AutoLock lock(&commands_lock_);
    IoCommand* io_cmd;
    while ((io_cmd = list_remove_head_type(&pending_commands_, IoCommand, node)) != nullptr) {
      io_cmd->Complete(ZX_ERR_PEER_CLOSED);
    }
  }

  delete this;
}

static zx_status_t WaitForReset(bool desired_ready_state, fdf::MmioBuffer* mmio) {
  constexpr int kResetWaitMs = 5000;
  int ms_remaining = kResetWaitMs;
  while (ControllerStatusReg::Get().ReadFrom(mmio).ready() != desired_ready_state) {
    if (ms_remaining-- == 0) {
      zxlogf(ERROR, "Timed out waiting for controller ready state %u: ", desired_ready_state);
      return ZX_ERR_TIMED_OUT;
    }
    zx_nanosleep(zx_deadline_after(ZX_MSEC(1)));
  }
  zxlogf(DEBUG, "Controller reached ready state %u (took %u ms).", desired_ready_state,
         kResetWaitMs - ms_remaining);
  return ZX_OK;
}

static zx_status_t CheckMinMaxSize(const std::string& name, size_t our_size, size_t min_size,
                                   size_t max_size) {
  if (our_size < min_size) {
    zxlogf(ERROR, "%s size is too small (ours: %zu, min: %zu).", name.c_str(), our_size, min_size);
    return ZX_ERR_NOT_SUPPORTED;
  }
  if (our_size > max_size) {
    zxlogf(ERROR, "%s size is too large (ours: %zu, max: %zu).", name.c_str(), our_size, max_size);
    return ZX_ERR_NOT_SUPPORTED;
  }
  return ZX_OK;
}

void Nvme::DdkInit(ddk::InitTxn txn) {
  // The drive initialization has numerous error conditions. Wrap the initialization here to ensure
  // we always call txn.Reply() in any outcome.
  zx_status_t status = Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Driver initialization failed: %s", zx_status_get_string(status));
  }
  txn.Reply(status);
}

static void PopulateVersionInspect(const VersionReg& version_reg, inspect::Node* inspect_node,
                                   inspect::Inspector* inspector) {
  auto version = inspect_node->CreateChild("version");
  version.RecordInt("major", version_reg.major());
  version.RecordInt("minor", version_reg.minor());
  version.RecordInt("tertiary", version_reg.tertiary());
  inspector->emplace(std::move(version));
}

static void PopulateCapabilitiesInspect(const CapabilityReg& caps_reg,
                                        const VersionReg& version_reg, inspect::Node* inspect_node,
                                        inspect::Inspector* inspector) {
  auto caps = inspect_node->CreateChild("capabilities");
  if (version_reg >= VersionReg::FromVer(1, 4, 0)) {
    caps.RecordBool("controller_ready_independent_media_supported",
                    caps_reg.controller_ready_independent_media_supported());
    caps.RecordBool("controller_ready_with_media_supported",
                    caps_reg.controller_ready_with_media_supported());
  }
  caps.RecordBool("subsystem_shutdown_supported", caps_reg.subsystem_shutdown_supported());
  caps.RecordBool("controller_memory_buffer_supported",
                  caps_reg.controller_memory_buffer_supported());
  caps.RecordBool("persistent_memory_region_supported",
                  caps_reg.persistent_memory_region_supported());
  caps.RecordInt("memory_page_size_max_bytes", caps_reg.memory_page_size_max_bytes());
  caps.RecordInt("memory_page_size_min_bytes", caps_reg.memory_page_size_min_bytes());
  caps.RecordInt("controller_power_scope", caps_reg.controller_power_scope());
  caps.RecordBool("boot_partition_support", caps_reg.boot_partition_support());
  caps.RecordBool("no_io_command_set_support", caps_reg.no_io_command_set_support());
  caps.RecordBool("identify_io_command_set_support", caps_reg.identify_io_command_set_support());
  caps.RecordBool("nvm_command_set_support", caps_reg.nvm_command_set_support());
  caps.RecordBool("nvm_subsystem_reset_supported", caps_reg.nvm_subsystem_reset_supported());
  caps.RecordInt("doorbell_stride_bytes", caps_reg.doorbell_stride_bytes());
  // TODO(fxbug.dev/102133): interpret CRTO register if version > 1.4
  caps.RecordInt("ready_timeout_ms", caps_reg.timeout_ms());
  caps.RecordBool("vendor_specific_arbitration_supported",
                  caps_reg.vendor_specific_arbitration_supported());
  caps.RecordBool("weighted_round_robin_arbitration_supported",
                  caps_reg.weighted_round_robin_arbitration_supported());
  caps.RecordBool("contiguous_queues_required", caps_reg.contiguous_queues_required());
  caps.RecordInt("max_queue_entries", caps_reg.max_queue_entries());
  inspector->emplace(std::move(caps));
}

static void PopulateControllerInspect(const IdentifyController& identify,
                                      uint32_t max_data_transfer_bytes,
                                      uint16_t atomic_write_unit_normal,
                                      uint16_t atomic_write_unit_power_fail,
                                      bool volatile_write_cache_present,
                                      bool volatile_write_cache_enabled,
                                      inspect::Node* inspect_node, inspect::Inspector* inspector) {
  auto controller = inspect_node->CreateChild("controller");
  auto model_number = std::string(identify.model_number, sizeof(identify.model_number));
  auto serial_number = std::string(identify.serial_number, sizeof(identify.serial_number));
  auto firmware_rev = std::string(identify.firmware_rev, sizeof(identify.firmware_rev));
  // Some vendors don't pad the strings with spaces (0x20). Null-terminate strings to avoid printing
  // illegal characters.
  model_number = std::string(model_number.c_str());
  serial_number = std::string(serial_number.c_str());
  firmware_rev = std::string(firmware_rev.c_str());
  zxlogf(INFO, "Model number:  '%s'", model_number.c_str());
  zxlogf(INFO, "Serial number: '%s'", serial_number.c_str());
  zxlogf(INFO, "Firmware rev.: '%s'", firmware_rev.c_str());
  controller.RecordString("model_number", model_number);
  controller.RecordString("serial_number", serial_number);
  controller.RecordString("firmware_rev", firmware_rev);
  controller.RecordInt("max_outstanding_commands", identify.max_cmd);
  controller.RecordInt("num_namespaces", identify.num_namespaces);
  controller.RecordInt("max_allowed_namespaces", identify.max_allowed_namespaces);
  controller.RecordBool("sgl_support", identify.sgl_support & 3);
  controller.RecordInt("max_data_transfer_bytes", max_data_transfer_bytes);
  controller.RecordInt("sanitize_caps", identify.sanicap & 3);
  controller.RecordInt("abort_command_limit", identify.acl + 1);
  controller.RecordInt("asynch_event_req_limit", identify.aerl + 1);
  controller.RecordInt("firmware_slots", (identify.frmw >> 1) & 3);
  controller.RecordBool("firmware_reset", !(identify.frmw & (1 << 4)));
  controller.RecordBool("firmware_slot1ro", identify.frmw & 1);
  controller.RecordInt("host_buffer_min_pages", identify.hmmin);
  controller.RecordInt("host_buffer_preferred_pages", identify.hmpre);
  controller.RecordInt("capacity_total", identify.tnvmcap[0]);
  controller.RecordInt("capacity_unalloc", identify.unvmcap[0]);
  controller.RecordBool("volatile_write_cache_present", volatile_write_cache_present);
  controller.RecordBool("volatile_write_cache_enabled", volatile_write_cache_enabled);
  controller.RecordInt("atomic_write_unit_normal_blocks", atomic_write_unit_normal);
  controller.RecordInt("atomic_write_unit_power_fail_blocks", atomic_write_unit_power_fail);
  controller.RecordBool("doorbell_buffer_config", identify.doorbell_buffer_config());
  controller.RecordBool("virtualization_management", identify.virtualization_management());
  controller.RecordBool("nvme_mi_send_recv", identify.nvme_mi_send_recv());
  controller.RecordBool("directive_send_recv", identify.directive_send_recv());
  controller.RecordBool("device_self_test", identify.device_self_test());
  controller.RecordBool("namespace_management", identify.namespace_management());
  controller.RecordBool("firmware_download_commit", identify.firmware_download_commit());
  controller.RecordBool("format_nvm", identify.format_nvm());
  controller.RecordBool("security_send_recv", identify.security_send_recv());
  controller.RecordBool("timestamp", identify.timestamp());
  controller.RecordBool("reservations", identify.reservations());
  controller.RecordBool("save_select_nonzero", identify.save_select_nonzero());
  controller.RecordBool("write_uncorrectable", identify.write_uncorrectable());
  controller.RecordBool("compare", identify.compare());
  inspector->emplace(std::move(controller));
}

zx_status_t Nvme::Init() {
  list_initialize(&pending_commands_);

  VersionReg version_reg = VersionReg::Get().ReadFrom(&mmio_);
  CapabilityReg caps_reg = CapabilityReg::Get().ReadFrom(&mmio_);

  inspect_node_ = inspector_.GetRoot().CreateChild("nvme");
  PopulateVersionInspect(version_reg, &inspect_node_, &inspector_);
  PopulateCapabilitiesInspect(caps_reg, version_reg, &inspect_node_, &inspector_);

  const size_t kPageSize = zx_system_get_page_size();
  zx_status_t status =
      CheckMinMaxSize("System page", kPageSize, caps_reg.memory_page_size_min_bytes(),
                      caps_reg.memory_page_size_max_bytes());
  if (status != ZX_OK) {
    return status;
  }

  if (ControllerStatusReg::Get().ReadFrom(&mmio_).ready()) {
    zxlogf(DEBUG, "Controller is already enabled. Resetting it.");
    ControllerConfigReg::Get().ReadFrom(&mmio_).set_enabled(0).WriteTo(&mmio_);
    status = WaitForReset(/*desired_ready_state=*/false, &mmio_);
    if (status != ZX_OK) {
      return status;
    }
  }

  // Set up admin submission and completion queues.
  zx::result admin_queue =
      QueuePair::Create(bti_.borrow(), 0, kAdminQueueMaxEntries, caps_reg, mmio_,
                        /*prealloc_prp=*/false);
  if (admin_queue.is_error()) {
    zxlogf(ERROR, "Failed to set up admin queue: %s", admin_queue.status_string());
    return admin_queue.status_value();
  }
  admin_queue_ = std::move(*admin_queue);

  // Configure the admin queue.
  AdminQueueAttributesReg::Get()
      .ReadFrom(&mmio_)
      .set_completion_queue_size(admin_queue_->completion().entry_count() - 1)
      .set_submission_queue_size(admin_queue_->submission().entry_count() - 1)
      .WriteTo(&mmio_);

  AdminQueueAddressReg::CompletionQueue()
      .ReadFrom(&mmio_)
      .set_addr(admin_queue_->completion().GetDeviceAddress())
      .WriteTo(&mmio_);
  AdminQueueAddressReg::SubmissionQueue()
      .ReadFrom(&mmio_)
      .set_addr(admin_queue_->submission().GetDeviceAddress())
      .WriteTo(&mmio_);

  zxlogf(DEBUG, "Enabling controller.");
  ControllerConfigReg::Get()
      .ReadFrom(&mmio_)
      .set_controller_ready_independent_of_media(0)
      // Queue entry sizes are powers of two.
      .set_io_completion_queue_entry_size(__builtin_ctzl(sizeof(Completion)))
      .set_io_submission_queue_entry_size(__builtin_ctzl(sizeof(Submission)))
      .set_arbitration_mechanism(ControllerConfigReg::ArbitrationMechanism::kRoundRobin)
      // We know that page size is always at least 4096 (required by spec), and we check
      // that zx_system_get_page_size is supported by the controller above.
      .set_memory_page_size(__builtin_ctzl(kPageSize) - 12)
      .set_io_command_set(ControllerConfigReg::CommandSet::kNvm)
      .set_enabled(1)
      .WriteTo(&mmio_);

  status = WaitForReset(/*desired_ready_state=*/true, &mmio_);
  if (status != ZX_OK) {
    return status;
  }

  // Timeout may have changed, so double check it.
  caps_reg.ReadFrom(&mmio_);

  // Set up IO submission and completion queues.
  zx::result io_queue =
      QueuePair::Create(bti_.borrow(), 1, caps_reg.max_queue_entries(), caps_reg, mmio_,
                        /*prealloc_prp=*/true);
  if (io_queue.is_error()) {
    zxlogf(ERROR, "Failed to set up io queue: %s", io_queue.status_string());
    return io_queue.status_value();
  }
  io_queue_ = std::move(*io_queue);
  inspect_node_.RecordInt("io_submission_queue_size", io_queue_->submission().entry_count());
  inspect_node_.RecordInt("io_completion_queue_size", io_queue_->completion().entry_count());

  // Spin up IRQ thread so we can start issuing admin commands to the device.
  int thrd_status = thrd_create_with_name(
      &irq_thread_, [](void* ctx) { return static_cast<Nvme*>(ctx)->IrqLoop(); }, this,
      "nvme-irq-thread");
  if (thrd_status != thrd_success) {
    status = thrd_status_to_zx_status(thrd_status);
    zxlogf(ERROR, " Failed to create IRQ thread: %s", zx_status_get_string(status));
    return status;
  }
  irq_thread_started_ = true;

  // Spin up IO thread so we can start issuing IO commands from namespace(s).
  thrd_status = thrd_create_with_name(
      &io_thread_, [](void* ctx) { return static_cast<Nvme*>(ctx)->IoLoop(); }, this,
      "nvme-io-thread");
  if (thrd_status != thrd_success) {
    status = thrd_status_to_zx_status(thrd_status);
    zxlogf(ERROR, " Failed to create IO thread: %s", zx_status_get_string(status));
    return status;
  }
  io_thread_started_ = true;

  zx::vmo admin_data;
  status = zx::vmo::create(kPageSize, 0, &admin_data);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create vmo: %s", zx_status_get_string(status));
    return status;
  }

  fzl::VmoMapper mapper;
  status = mapper.Map(admin_data);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to map vmo: %s", zx_status_get_string(status));
    return status;
  }

  IdentifySubmission identify_controller;
  identify_controller.set_structure(IdentifySubmission::IdentifyCns::kIdentifyController);
  status = DoAdminCommandSync(identify_controller, admin_data.borrow());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to identify controller: %s", zx_status_get_string(status));
    return status;
  }

  auto identify = static_cast<IdentifyController*>(mapper.start());

  status = CheckMinMaxSize("Submission queue entry", sizeof(Submission),
                           identify->minimum_sq_entry_size(), identify->maximum_sq_entry_size());
  if (status != ZX_OK) {
    return status;
  }
  status = CheckMinMaxSize("Completion queue entry", sizeof(Completion),
                           identify->minimum_cq_entry_size(), identify->maximum_cq_entry_size());
  if (status != ZX_OK) {
    return status;
  }

  if (identify->max_data_transfer == 0) {
    max_data_transfer_bytes_ = 0;
  } else {
    max_data_transfer_bytes_ =
        caps_reg.memory_page_size_min_bytes() * (1 << identify->max_data_transfer);
  }
  atomic_write_unit_normal_ = identify->atomic_write_unit_normal + 1;
  atomic_write_unit_power_fail_ = identify->atomic_write_unit_power_fail + 1;

  bool volatile_write_cache_present = identify->vwc & 1;
  if (volatile_write_cache_present) {
    // Get 'Volatile Write Cache Enable' feature.
    GetVolatileWriteCacheSubmission get_vwc_enable;
    status = DoAdminCommandSync(get_vwc_enable);
    auto& completion = admin_result_.GetCompletion<GetVolatileWriteCacheCompletion>();
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to get 'Volatile Write Cache' feature: %s",
             zx_status_get_string(status));
      return status;
    } else {
      volatile_write_cache_enabled_ = completion.get_volatile_write_cache_enabled();
      zxlogf(DEBUG, "Volatile write cache is %s",
             volatile_write_cache_enabled_ ? "enabled" : "disabled");
    }
  }

  PopulateControllerInspect(*identify, max_data_transfer_bytes_, atomic_write_unit_normal_,
                            atomic_write_unit_power_fail_, volatile_write_cache_present,
                            volatile_write_cache_enabled_, &inspect_node_, &inspector_);

  // Set feature (number of queues) to 1 IO submission queue and 1 IO completion queue.
  SetIoQueueCountSubmission set_queue_count;
  set_queue_count.set_num_submission_queues(1).set_num_completion_queues(1);
  status = DoAdminCommandSync(set_queue_count);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to set feature (number of queues): %s", zx_status_get_string(status));
    return status;
  }
  auto& completion = admin_result_.GetCompletion<SetIoQueueCountCompletion>();
  if (completion.num_submission_queues() < 1) {
    zxlogf(ERROR, "Unexpected IO submission queue count: %u", completion.num_submission_queues());
    return ZX_ERR_IO;
  }
  if (completion.num_completion_queues() < 1) {
    zxlogf(ERROR, "Unexpected IO completion queue count: %u", completion.num_completion_queues());
    return ZX_ERR_IO;
  }

  // Create IO completion queue.
  CreateIoCompletionQueueSubmission create_iocq;
  create_iocq.set_queue_id(io_queue_->completion().id())
      .set_queue_size(io_queue_->completion().entry_count() - 1)
      .set_contiguous(true)
      .set_interrupt_en(true)
      .set_interrupt_vector(0);
  create_iocq.data_pointer[0] = io_queue_->completion().GetDeviceAddress();
  status = DoAdminCommandSync(create_iocq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create IO completion queue: %s", zx_status_get_string(status));
    return status;
  }

  // Create IO submission queue.
  CreateIoSubmissionQueueSubmission create_iosq;
  create_iosq.set_queue_id(io_queue_->submission().id())
      .set_queue_size(io_queue_->submission().entry_count() - 1)
      .set_completion_queue_id(io_queue_->completion().id())
      .set_contiguous(true);
  create_iosq.data_pointer[0] = io_queue_->submission().GetDeviceAddress();
  status = DoAdminCommandSync(create_iosq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create IO submission queue: %s", zx_status_get_string(status));
    return status;
  }

  // Identify active namespaces.
  IdentifySubmission identify_ns_list;
  identify_ns_list.set_structure(IdentifySubmission::IdentifyCns::kActiveNamespaceList);
  status = DoAdminCommandSync(identify_ns_list, admin_data.borrow());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to identify active namespace list: %s", zx_status_get_string(status));
    return status;
  }

  // Bind active namespaces.
  auto ns_list = static_cast<IdentifyActiveNamespaces*>(mapper.start());
  for (size_t i = 0; i < std::size(ns_list->nsid) && ns_list->nsid[i] != 0; i++) {
    if (i >= kMaxNamespacesToBind) {
      zxlogf(WARNING, "Skipping additional namespaces after adding %zu.", i);
      break;
    }
    const uint32_t namespace_id = ns_list->nsid[i];
    status = Namespace::Bind(this, namespace_id);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to add namespace %u: %s", namespace_id, zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}

zx_status_t Nvme::AddDevice() {
  zx_status_t status = DdkAdd(ddk::DeviceAddArgs(kDriverName)
                                  .set_flags(DEVICE_ADD_NON_BINDABLE)
                                  .set_inspect_vmo(inspector_.DuplicateVmo()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed DdkAdd: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t Nvme::Bind(void* ctx, zx_device_t* parent) {
  ddk::Pci pci(parent, "pci");
  if (!pci.is_valid()) {
    zxlogf(ERROR, "Failed to find PCI fragment");
    return ZX_ERR_NOT_SUPPORTED;
  }

  std::optional<fdf::MmioBuffer> mmio;
  zx_status_t status = pci.MapMmio(0u, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to map registers: %s", zx_status_get_string(status));
    return status;
  }

  fuchsia_hardware_pci::InterruptMode irq_mode;
  status = pci.ConfigureInterruptMode(1, &irq_mode);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to configure interrupt: %s", zx_status_get_string(status));
    return status;
  }
  zxlogf(DEBUG, "Interrupt mode: %u", static_cast<uint8_t>(irq_mode));

  zx::interrupt irq;
  status = pci.MapInterrupt(0, &irq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to map interrupt: %s", zx_status_get_string(status));
    return status;
  }

  status = pci.SetBusMastering(true);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to enable bus mastering: %s", zx_status_get_string(status));
    return status;
  }
  auto cleanup = fit::defer([&] { pci.SetBusMastering(false); });

  zx::bti bti;
  status = pci.GetBti(0, &bti);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get BTI handle: %s", zx_status_get_string(status));
    return status;
  }

  fbl::AllocChecker ac;
  auto driver = fbl::make_unique_checked<nvme::Nvme>(&ac, parent, std::move(pci), std::move(*mmio),
                                                     irq_mode, std::move(irq), std::move(bti));
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate memory for nvme driver.");
    return ZX_ERR_NO_MEMORY;
  }

  status = driver->AddDevice();
  if (status != ZX_OK) {
    return status;
  }

  // The DriverFramework now owns driver.
  [[maybe_unused]] auto placeholder = driver.release();
  cleanup.cancel();
  return ZX_OK;
}

static zx_driver_ops_t driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = Nvme::Bind,
};

}  // namespace nvme

ZIRCON_DRIVER(nvme, nvme::driver_ops, "zircon", "0.1");
