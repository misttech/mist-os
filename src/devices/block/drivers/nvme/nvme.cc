// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/nvme/nvme.h"

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
#include <zircon/types.h>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <hwreg/bitfields.h>

#include "src/devices/block/drivers/nvme/commands/features.h"
#include "src/devices/block/drivers/nvme/commands/identify.h"
#include "src/devices/block/drivers/nvme/commands/queue.h"
#include "src/devices/block/drivers/nvme/namespace.h"
#include "src/devices/block/drivers/nvme/nvme_bind.h"
#include "src/devices/block/drivers/nvme/registers.h"

namespace nvme {

// For safety - so that the driver doesn't draw too many resources.
constexpr size_t kMaxNamespacesToBind = 4;

// c.f. NVMe Base Specification 2.0, section 3.1.3.8 "AQA - Admin Queue Attributes"
constexpr size_t kAdminQueueMaxEntries = 4096;

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
    if (irq_mode_ != PCI_INTERRUPT_MODE_MSI_X) {
      InterruptReg::MaskSet().FromValue(1).WriteTo(&mmio_);
    }

    Completion* admin_completion;
    if (admin_queue_->CheckForNewCompletion(&admin_completion) != ZX_ERR_SHOULD_WAIT) {
      admin_result_ = *admin_completion;
      sync_completion_signal(&admin_signal_);
      admin_queue_->RingCompletionDb();
    }

    // TODO(fxbug.dev/102133): Consider using interrupt vector (dedicated interrupt per
    // namespace/queue).
    for (Namespace* ns : namespaces_) {
      ns->SignalIo();
    }

    if (irq_mode_ != PCI_INTERRUPT_MODE_MSI_X) {
      // Unmask the interrupt.
      InterruptReg::MaskClear().FromValue(1).WriteTo(&mmio_);
    }

    if (irq_mode_ == PCI_INTERRUPT_MODE_LEGACY) {
      status = pci_ack_interrupt(&pci_);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Failed to ack interrupt: %s", zx_status_get_string(status));
        break;
      }
    }
  }
  return 0;
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

void Nvme::DdkRelease() {
  zxlogf(DEBUG, "Releasing driver.");
  if (mmio_.get_vmo() != ZX_HANDLE_INVALID) {
    pci_set_bus_mastering(&pci_, false);
  }
  irq_.destroy();  // Make irq_.wait() in IrqLoop() return ZX_ERR_CANCELED.
  if (irq_thread_started_) {
    int unused;
    thrd_join(irq_thread_, &unused);
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

static void PopulateVersionInspect(const VersionReg& version_reg, inspect::Inspector* inspect) {
  auto version = inspect->GetRoot().CreateChild("version");
  version.CreateInt("major", version_reg.major(), inspect);
  version.CreateInt("minor", version_reg.minor(), inspect);
  version.CreateInt("tertiary", version_reg.tertiary(), inspect);
  inspect->emplace(std::move(version));
}

static void PopulateCapabilitiesInspect(const CapabilityReg& caps_reg,
                                        const VersionReg& version_reg,
                                        inspect::Inspector* inspect) {
  auto caps = inspect->GetRoot().CreateChild("capabilities");
  if (version_reg >= VersionReg::FromVer(1, 4, 0)) {
    caps.CreateBool("controller_ready_independent_media_supported",
                    caps_reg.controller_ready_independent_media_supported(), inspect);
    caps.CreateBool("controller_ready_with_media_supported",
                    caps_reg.controller_ready_with_media_supported(), inspect);
  }
  caps.CreateBool("subsystem_shutdown_supported", caps_reg.subsystem_shutdown_supported(), inspect);
  caps.CreateBool("controller_memory_buffer_supported",
                  caps_reg.controller_memory_buffer_supported(), inspect);
  caps.CreateBool("persistent_memory_region_supported",
                  caps_reg.persistent_memory_region_supported(), inspect);
  caps.CreateInt("memory_page_size_max_bytes", caps_reg.memory_page_size_max_bytes(), inspect);
  caps.CreateInt("memory_page_size_min_bytes", caps_reg.memory_page_size_min_bytes(), inspect);
  caps.CreateInt("controller_power_scope", caps_reg.controller_power_scope(), inspect);
  caps.CreateBool("boot_partition_support", caps_reg.boot_partition_support(), inspect);
  caps.CreateBool("no_io_command_set_support", caps_reg.no_io_command_set_support(), inspect);
  caps.CreateBool("identify_io_command_set_support", caps_reg.identify_io_command_set_support(),
                  inspect);
  caps.CreateBool("nvm_command_set_support", caps_reg.nvm_command_set_support(), inspect);
  caps.CreateBool("nvm_subsystem_reset_supported", caps_reg.nvm_subsystem_reset_supported(),
                  inspect);
  caps.CreateInt("doorbell_stride_bytes", caps_reg.doorbell_stride_bytes(), inspect);
  // TODO(fxbug.dev/102133): interpret CRTO register if version > 1.4
  caps.CreateInt("ready_timeout_ms", caps_reg.timeout_ms(), inspect);
  caps.CreateBool("vendor_specific_arbitration_supported",
                  caps_reg.vendor_specific_arbitration_supported(), inspect);
  caps.CreateBool("weighted_round_robin_arbitration_supported",
                  caps_reg.weighted_round_robin_arbitration_supported(), inspect);
  caps.CreateBool("contiguous_queues_required", caps_reg.contiguous_queues_required(), inspect);
  caps.CreateInt("max_queue_entries", caps_reg.max_queue_entries(), inspect);
  inspect->emplace(std::move(caps));
}

static void PopulateControllerInspect(const IdentifyController& identify,
                                      uint32_t max_data_transfer_bytes,
                                      uint16_t atomic_write_unit_normal,
                                      uint16_t atomic_write_unit_power_fail,
                                      bool volatile_write_cache_present,
                                      bool volatile_write_cache_enabled,
                                      inspect::Inspector* inspect) {
  auto controller = inspect->GetRoot().CreateChild("controller");
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
  controller.CreateString("model_number", model_number, inspect);
  controller.CreateString("serial_number", serial_number, inspect);
  controller.CreateString("firmware_rev", firmware_rev, inspect);
  controller.CreateInt("max_outstanding_commands", identify.max_cmd, inspect);
  controller.CreateInt("num_namespaces", identify.num_namespaces, inspect);
  controller.CreateInt("max_allowed_namespaces", identify.max_allowed_namespaces, inspect);
  controller.CreateBool("sgl_support", identify.sgl_support & 3, inspect);
  controller.CreateInt("max_data_transfer_bytes", max_data_transfer_bytes, inspect);
  controller.CreateInt("sanitize_caps", identify.sanicap & 3, inspect);
  controller.CreateInt("abort_command_limit", identify.acl + 1, inspect);
  controller.CreateInt("asynch_event_req_limit", identify.aerl + 1, inspect);
  controller.CreateInt("firmware_slots", (identify.frmw >> 1) & 3, inspect);
  controller.CreateBool("firmware_reset", !(identify.frmw & (1 << 4)), inspect);
  controller.CreateBool("firmware_slot1ro", identify.frmw & 1, inspect);
  controller.CreateInt("host_buffer_min_pages", identify.hmmin, inspect);
  controller.CreateInt("host_buffer_preferred_pages", identify.hmpre, inspect);
  controller.CreateInt("capacity_total", identify.tnvmcap[0], inspect);
  controller.CreateInt("capacity_unalloc", identify.unvmcap[0], inspect);
  controller.CreateBool("volatile_write_cache_present", volatile_write_cache_present, inspect);
  controller.CreateBool("volatile_write_cache_enabled", volatile_write_cache_enabled, inspect);
  controller.CreateInt("atomic_write_unit_normal_blocks", atomic_write_unit_normal, inspect);
  controller.CreateInt("atomic_write_unit_power_fail_blocks", atomic_write_unit_power_fail,
                       inspect);
  controller.CreateBool("doorbell_buffer_config", identify.doorbell_buffer_config(), inspect);
  controller.CreateBool("virtualization_management", identify.virtualization_management(), inspect);
  controller.CreateBool("nvme_mi_send_recv", identify.nvme_mi_send_recv(), inspect);
  controller.CreateBool("directive_send_recv", identify.directive_send_recv(), inspect);
  controller.CreateBool("device_self_test", identify.device_self_test(), inspect);
  controller.CreateBool("namespace_management", identify.namespace_management(), inspect);
  controller.CreateBool("firmware_download_commit", identify.firmware_download_commit(), inspect);
  controller.CreateBool("format_nvm", identify.format_nvm(), inspect);
  controller.CreateBool("security_send_recv", identify.security_send_recv(), inspect);
  controller.CreateBool("timestamp", identify.timestamp(), inspect);
  controller.CreateBool("reservations", identify.reservations(), inspect);
  controller.CreateBool("save_select_nonzero", identify.save_select_nonzero(), inspect);
  controller.CreateBool("write_uncorrectable", identify.write_uncorrectable(), inspect);
  controller.CreateBool("compare", identify.compare(), inspect);
  inspect->emplace(std::move(controller));
}

zx_status_t Nvme::Init() {
  VersionReg version_reg = VersionReg::Get().ReadFrom(&mmio_);
  CapabilityReg caps_reg = CapabilityReg::Get().ReadFrom(&mmio_);

  PopulateVersionInspect(version_reg, &inspect_);
  PopulateCapabilitiesInspect(caps_reg, version_reg, &inspect_);

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
  inspect_.GetRoot().CreateInt("io_submission_queue_size", io_queue_->submission().entry_count(),
                               &inspect_);
  inspect_.GetRoot().CreateInt("io_completion_queue_size", io_queue_->completion().entry_count(),
                               &inspect_);

  // Spin up IRQ thread so we can start issuing admin commands to the device.
  int thrd_status = thrd_create_with_name(&irq_thread_, IrqThread, this, "nvme-irq-thread");
  if (thrd_status) {
    zxlogf(ERROR, " Failed to create IRQ thread: %d", thrd_status);
    return ZX_ERR_INTERNAL;
  }
  irq_thread_started_ = true;

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
                            volatile_write_cache_enabled_, &inspect_);

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
    if (namespaces_.size() >= kMaxNamespacesToBind) {
      zxlogf(WARNING, "Skipping additional namespaces after adding %zu.", namespaces_.size());
      break;
    }
    zx::result result = Namespace::Bind(this, ns_list->nsid[i]);
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to add namespace %u: %s", ns_list->nsid[i], result.status_string());
      return result.status_value();
    }
    namespaces_.push_back(result.value());
  }

  return ZX_OK;
}

zx_status_t Nvme::AddDevice() {
  auto cleanup = fit::defer([&] { DdkRelease(); });

  zx_status_t status = DdkAdd(ddk::DeviceAddArgs("nvme").set_inspect_vmo(inspect_.DuplicateVmo()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed DdkAdd: %s", zx_status_get_string(status));
    return status;
  }

  cleanup.cancel();
  return ZX_OK;
}

zx_status_t Nvme::Bind(void* ctx, zx_device_t* dev) {
  pci_protocol_t pci;
  zx_status_t status = device_get_fragment_protocol(dev, "pci", ZX_PROTOCOL_PCI, &pci);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to find PCI fragment: %s", zx_status_get_string(status));
    return status;
  }

  mmio_buffer_t mmio_buffer;
  status = pci_map_bar_buffer(&pci, 0u, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mmio_buffer);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to map registers: %s", zx_status_get_string(status));
    return status;
  }
  auto mmio = fdf::MmioBuffer(mmio_buffer);

  pci_interrupt_mode_t irq_mode;
  status = pci_configure_interrupt_mode(&pci, 1, &irq_mode);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to configure interrupt: %s", zx_status_get_string(status));
    return status;
  }
  zxlogf(DEBUG, "Interrupt mode: %u", irq_mode);

  zx_handle_t irq_handle;
  status = pci_map_interrupt(&pci, 0, &irq_handle);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to map interrupt: %s", zx_status_get_string(status));
    return status;
  }
  auto irq = zx::interrupt(irq_handle);

  status = pci_set_bus_mastering(&pci, true);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to enable bus mastering: %s", zx_status_get_string(status));
    return status;
  }

  zx_handle_t bti_handle;
  status = pci_get_bti(&pci, 0, &bti_handle);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get BTI handle: %s", zx_status_get_string(status));
    return status;
  }
  auto bti = zx::bti(bti_handle);

  auto driver = std::make_unique<nvme::Nvme>(dev, pci, std::move(mmio), irq_mode, std::move(irq),
                                             std::move(bti));
  status = driver->AddDevice();
  if (status != ZX_OK) {
    return status;
  }

  // The DriverFramework now owns driver.
  [[maybe_unused]] auto placeholder = driver.release();
  return ZX_OK;
}

static zx_driver_ops_t driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = Nvme::Bind,
};

}  // namespace nvme

ZIRCON_DRIVER(nvme, nvme::driver_ops, "zircon", "0.1");
