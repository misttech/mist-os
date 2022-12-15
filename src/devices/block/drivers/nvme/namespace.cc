// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/nvme/namespace.h"

#include <fuchsia/hardware/block/cpp/banjo.h>
#include <lib/fzl/vmo-mapper.h>
#include <threads.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <ddktl/device.h>
#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <hwreg/bitfields.h>

#include "src/devices/block/drivers/nvme/commands/identify.h"
#include "src/devices/block/drivers/nvme/commands/nvme-io.h"
#include "src/devices/block/drivers/nvme/nvme.h"
#include "src/devices/block/drivers/nvme/queue-pair.h"

namespace nvme {

struct IoCommand {
  block_op_t op;
  list_node_t node;
  block_impl_queue_callback completion_cb;
  void* cookie;
  uint16_t pending_txns;
  uint8_t opcode;
  uint8_t flags;

  DEF_SUBBIT(flags, 0, command_failed);
  DEF_SUBBIT(flags, 1, forced_unit_access);
};

static inline void IoCommandComplete(IoCommand* io_cmd, zx_status_t status) {
  io_cmd->completion_cb(io_cmd->cookie, status, &io_cmd->op);
}

bool Namespace::SubmitAllTxnsForIoCommand(IoCommand* io_cmd) {
  while (true) {
    uint32_t blocks = io_cmd->op.rw.length;
    if (blocks > max_transfer_blocks_) {
      blocks = max_transfer_blocks_;
    }

    // Total transfer size in bytes
    size_t bytes = blocks * block_info_.block_size;

    NvmIoSubmission submission(io_cmd->opcode == BLOCK_OP_WRITE);
    submission.namespace_id = namespace_id_;
    ZX_ASSERT(blocks - 1 <= UINT16_MAX);
    submission.set_start_lba(io_cmd->op.rw.offset_dev).set_block_count(blocks - 1);
    if (io_cmd->forced_unit_access()) {
      submission.set_force_unit_access(true);
    }

    zx_status_t status = controller_->io_queue()->Submit(
        submission, zx::unowned_vmo(io_cmd->op.rw.vmo), io_cmd->op.rw.offset_vmo, bytes, io_cmd);
    if (status != ZX_OK) {
      if (status == ZX_ERR_SHOULD_WAIT) {
        // We can't proceed if there is no available space in the submission queue, and we tell the
        // caller to retain the command (false).
        return false;
      } else {
        zxlogf(ERROR, "Failed to submit transaction (command %p): %s", io_cmd,
               zx_status_get_string(status));
        break;
      }
    }

    // keep track of where we are
    io_cmd->op.rw.offset_dev += blocks;
    io_cmd->op.rw.offset_vmo += bytes;
    io_cmd->op.rw.length -= blocks;
    io_cmd->pending_txns++;

    // If there are no more transactions remaining, we're done. We move this command to the active
    // list and tell the caller not to retain the command (true).
    if (io_cmd->op.rw.length == 0) {
      fbl::AutoLock lock(&commands_lock_);
      list_add_tail(&active_commands_, &io_cmd->node);
      return true;
    }
  }

  {
    fbl::AutoLock lock(&commands_lock_);
    io_cmd->set_command_failed(true);
    if (io_cmd->pending_txns) {
      // If there are earlier uncompleted transactions, we become active now and will finish
      // erroring out when they complete.
      list_add_tail(&active_commands_, &io_cmd->node);
      io_cmd = nullptr;
    }
  }

  if (io_cmd != nullptr) {
    IoCommandComplete(io_cmd, ZX_ERR_INTERNAL);
  }

  // Either successful or not, we tell the caller not to retain the command (true).
  return true;
}

void Namespace::ProcessIoSubmissions() {
  IoCommand* io_cmd;
  while (true) {
    {
      fbl::AutoLock lock(&commands_lock_);
      io_cmd = list_remove_head_type(&pending_commands_, IoCommand, node);
    }

    if (io_cmd == nullptr) {
      return;
    }

    if (!SubmitAllTxnsForIoCommand(io_cmd)) {
      // put command back at front of queue for further processing later
      fbl::AutoLock lock(&commands_lock_);
      list_add_head(&pending_commands_, &io_cmd->node);
      return;
    }
  }
}

void Namespace::ProcessIoCompletions() {
  bool ring_doorbell = false;
  Completion* completion = nullptr;
  IoCommand* io_cmd = nullptr;
  while (controller_->io_queue()->CheckForNewCompletion(&completion, &io_cmd) !=
         ZX_ERR_SHOULD_WAIT) {
    ring_doorbell = true;

    if (io_cmd == nullptr) {
      zxlogf(ERROR, "Completed transaction isn't associated with a command.");
      continue;
    }

    if (completion->status_code_type() == StatusCodeType::kGeneric &&
        completion->status_code() == 0) {
      zxlogf(TRACE, "Completed transaction #%u command %p OK.", completion->command_id(), io_cmd);
    } else {
      zxlogf(ERROR, "Completed transaction #%u command %p ERROR: status type=%01x, status=%02x",
             completion->command_id(), io_cmd, completion->status_code_type(),
             completion->status_code());
      io_cmd->set_command_failed(true);
      // Discard any remaining bytes -- no reason to keep creating further txns once one has failed.
      io_cmd->op.rw.length = 0;
    }

    io_cmd->pending_txns--;
    if ((io_cmd->pending_txns == 0) && (io_cmd->op.rw.length == 0)) {
      // remove from either pending or active list
      {
        fbl::AutoLock lock(&commands_lock_);
        list_delete(&io_cmd->node);
      }
      zxlogf(TRACE, "Completed command %p %s", io_cmd,
             io_cmd->command_failed() ? "FAILED." : "OK.");
      IoCommandComplete(io_cmd, io_cmd->command_failed() ? ZX_ERR_IO : ZX_OK);
    }
  }

  if (ring_doorbell) {
    controller_->io_queue()->RingCompletionDb();
  }
}

int Namespace::IoLoop() {
  while (true) {
    if (sync_completion_wait(&io_signal_, ZX_TIME_INFINITE)) {
      break;
    }
    if (driver_shutdown_) {
      // TODO: cancel out pending IO
      zxlogf(DEBUG, "IO thread exiting.");
      break;
    }

    sync_completion_reset(&io_signal_);

    // process completion messages
    ProcessIoCompletions();

    // process work queue
    ProcessIoSubmissions();
  }
  return 0;
}

zx_status_t Namespace::AddNamespace() {
  return DdkAdd(ddk::DeviceAddArgs(NamespaceName().c_str()));
}

zx::result<Namespace*> Namespace::Bind(Nvme* controller, uint32_t namespace_id) {
  if (namespace_id == 0 || namespace_id == ~0u) {
    zxlogf(ERROR, "Attempted to create namespace with invalid id %u.", namespace_id);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto driver = std::make_unique<Namespace>(controller->zxdev(), controller, namespace_id);
  zx_status_t status = driver->AddNamespace();
  if (status != ZX_OK) {
    return zx::error(status);
  }

  // The DDK takes ownership of the device.
  return zx::ok(driver.release());
}

void Namespace::DdkRelease() {
  zxlogf(DEBUG, "Releasing driver.");
  driver_shutdown_ = true;
  if (io_thread_started_) {
    sync_completion_signal(&io_signal_);
    int unused;
    thrd_join(io_thread_, &unused);
  }

  // Error out any pending commands
  {
    fbl::AutoLock lock(&commands_lock_);
    IoCommand* io_cmd;
    while ((io_cmd = list_remove_head_type(&active_commands_, IoCommand, node)) != nullptr) {
      IoCommandComplete(io_cmd, ZX_ERR_PEER_CLOSED);
    }
    while ((io_cmd = list_remove_head_type(&pending_commands_, IoCommand, node)) != nullptr) {
      IoCommandComplete(io_cmd, ZX_ERR_PEER_CLOSED);
    }
  }

  delete this;
}

void Namespace::DdkInit(ddk::InitTxn txn) {
  // The driver initialization has numerous error conditions. Wrap the initialization here to ensure
  // we always call txn.Reply() in any outcome.
  zx_status_t status = Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Driver initialization failed: %s", zx_status_get_string(status));
  }
  txn.Reply(status);
}

static void PopulateNamespaceInspect(const IdentifyNvmeNamespace& ns,
                                     const fbl::String& namespace_name,
                                     uint16_t atomic_write_unit_normal,
                                     uint16_t atomic_write_unit_power_fail,
                                     uint32_t max_transfer_bytes, uint32_t block_size_bytes,
                                     inspect::Inspector* inspect) {
  auto inspect_ns = inspect->GetRoot().CreateChild(namespace_name);
  uint16_t nawun = ns.ns_atomics() ? ns.n_aw_un + 1 : atomic_write_unit_normal;
  uint16_t nawupf = ns.ns_atomics() ? ns.n_aw_u_pf + 1 : atomic_write_unit_power_fail;
  inspect_ns.CreateInt("atomic_write_unit_normal_blocks", nawun, inspect);
  inspect_ns.CreateInt("atomic_write_unit_power_fail_blocks", nawupf, inspect);
  inspect_ns.CreateInt("namespace_atomic_boundary_size_normal_blocks", ns.n_abs_n, inspect);
  inspect_ns.CreateInt("namespace_atomic_boundary_offset_blocks", ns.n_ab_o, inspect);
  inspect_ns.CreateInt("namespace_atomic_boundary_size_power_fail_blocks", ns.n_abs_pf, inspect);
  inspect_ns.CreateInt("namespace_optimal_io_boundary_blocks", ns.n_oio_b, inspect);
  // table of block formats
  for (int i = 0; i < ns.n_lba_f; i++) {
    if (ns.lba_formats[i].value) {
      auto& fmt = ns.lba_formats[i];
      inspect_ns.CreateInt(fbl::StringPrintf("lba_format_%u_block_size_bytes", i),
                           fmt.lba_data_size_bytes(), inspect);
      inspect_ns.CreateInt(fbl::StringPrintf("lba_format_%u_relative_performance", i),
                           fmt.relative_performance(), inspect);
      inspect_ns.CreateInt(fbl::StringPrintf("lba_format_%u_metadata_size_bytes", i),
                           fmt.metadata_size_bytes(), inspect);
    }
  }
  inspect_ns.CreateInt("active_lba_format_index", ns.lba_format_index(), inspect);
  inspect_ns.CreateInt("data_protection_caps", ns.dpc & 0x3F, inspect);
  inspect_ns.CreateInt("data_protection_set", ns.dps & 3, inspect);
  inspect_ns.CreateInt("namespace_size_blocks", ns.n_sze, inspect);
  inspect_ns.CreateInt("namespace_cap_blocks", ns.n_cap, inspect);
  inspect_ns.CreateInt("namespace_util_blocks", ns.n_use, inspect);
  inspect_ns.CreateInt("max_transfer_bytes", max_transfer_bytes, inspect);
  inspect_ns.CreateInt("block_size_bytes", block_size_bytes, inspect);
  inspect->emplace(std::move(inspect_ns));
}

zx_status_t Namespace::Init() {
  list_initialize(&pending_commands_);
  list_initialize(&active_commands_);

  zx::vmo admin_data;
  const uint32_t kPageSize = zx_system_get_page_size();
  zx_status_t status = zx::vmo::create(kPageSize, 0, &admin_data);
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

  // Identify namespace.
  IdentifySubmission identify_ns;
  identify_ns.namespace_id = namespace_id_;
  identify_ns.set_structure(IdentifySubmission::IdentifyCns::kIdentifyNamespace);
  status = controller_->DoAdminCommandSync(identify_ns, admin_data.borrow());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to identify namespace %u: %s", namespace_id_,
           zx_status_get_string(status));
    return status;
  }

  auto ns = static_cast<IdentifyNvmeNamespace*>(mapper.start());

  block_info_.block_count = ns->n_sze;
  auto& fmt = ns->lba_formats[ns->lba_format_index()];
  block_info_.block_size = fmt.lba_data_size_bytes();
  // TODO(fxbug.dev/102133): Explore the option of bounding this and relying on the block driver to
  // break up large IOs.
  block_info_.max_transfer_size = BLOCK_MAX_TRANSFER_UNBOUNDED;

  if (fmt.metadata_size_bytes()) {
    zxlogf(ERROR, "NVMe drive uses LBA format with metadata (%u bytes), which we do not support.",
           fmt.metadata_size_bytes());
    return ZX_ERR_NOT_SUPPORTED;
  }
  // The NVMe spec only mentions a lower bound. The upper bound may be a false requirement.
  if ((block_info_.block_size < 512) || (block_info_.block_size > 32768)) {
    zxlogf(ERROR, "Cannot handle LBA size of %u.", block_info_.block_size);
    return ZX_ERR_NOT_SUPPORTED;
  }

  // NVME r/w commands operate in block units, maximum of 64K:
  const uint32_t max_bytes_per_cmd = block_info_.block_size * 65536;
  uint32_t max_transfer_bytes = controller_->max_data_transfer_bytes();
  if (max_transfer_bytes == 0) {
    max_transfer_bytes = max_bytes_per_cmd;
  } else {
    max_transfer_bytes = std::min(max_transfer_bytes, max_bytes_per_cmd);
  }

  // Limit maximum transfer size to 1MB which fits comfortably within our single PRP page per
  // QueuePair setup.
  const uint32_t prp_restricted_transfer_bytes = QueuePair::kMaxTransferPages * kPageSize;
  if (max_transfer_bytes > prp_restricted_transfer_bytes) {
    max_transfer_bytes = prp_restricted_transfer_bytes;
  }

  // Convert to block units.
  max_transfer_blocks_ = max_transfer_bytes / block_info_.block_size;

  PopulateNamespaceInspect(*ns, NamespaceName(), controller_->atomic_write_unit_normal(),
                           controller_->atomic_write_unit_power_fail(), max_transfer_bytes,
                           block_info_.block_size, &controller_->inspect());

  // Spin up IO thread so we can start issuing IO commands to the namespace.
  auto name = fbl::StringPrintf("nvme-io-thread-%u", namespace_id_);
  int thrd_status = thrd_create_with_name(&io_thread_, IoThread, this, name.c_str());
  if (thrd_status) {
    zxlogf(ERROR, " Failed to create IO thread: %d", thrd_status);
    return ZX_ERR_INTERNAL;
  }
  io_thread_started_ = true;

  return ZX_OK;
}

void Namespace::BlockImplQuery(block_info_t* out_info, uint64_t* out_block_op_size) {
  *out_info = block_info_;
  *out_block_op_size = sizeof(IoCommand);
}

void Namespace::BlockImplQueue(block_op_t* op, block_impl_queue_callback callback, void* cookie) {
  IoCommand* io_cmd = containerof(op, IoCommand, op);
  io_cmd->completion_cb = callback;
  io_cmd->cookie = cookie;
  io_cmd->opcode = io_cmd->op.command & BLOCK_OP_MASK;
  io_cmd->flags = 0;
  if (io_cmd->op.command & BLOCK_FL_FORCE_ACCESS) {
    io_cmd->set_forced_unit_access(true);
  }

  switch (io_cmd->opcode) {
    case BLOCK_OP_READ:
    case BLOCK_OP_WRITE:
      break;
    case BLOCK_OP_FLUSH:
      // TODO
      IoCommandComplete(io_cmd, ZX_OK);
      return;
    default:
      IoCommandComplete(io_cmd, ZX_ERR_NOT_SUPPORTED);
      return;
  }

  if (io_cmd->op.rw.length == 0) {
    IoCommandComplete(io_cmd, ZX_ERR_INVALID_ARGS);
    return;
  }
  // Transaction must fit within device
  if ((io_cmd->op.rw.offset_dev >= block_info_.block_count) ||
      (block_info_.block_count - io_cmd->op.rw.offset_dev < io_cmd->op.rw.length)) {
    IoCommandComplete(io_cmd, ZX_ERR_OUT_OF_RANGE);
    return;
  }

  // convert vmo offset to a byte offset
  io_cmd->op.rw.offset_vmo *= block_info_.block_size;

  io_cmd->pending_txns = 0;

  zxlogf(TRACE, "Block IO: %s: %u blocks @ LBA %zu", io_cmd->opcode == BLOCK_OP_WRITE ? "wr" : "rd",
         io_cmd->op.rw.length, io_cmd->op.rw.offset_dev);

  {
    fbl::AutoLock lock(&commands_lock_);
    list_add_tail(&pending_commands_, &io_cmd->node);
  }

  sync_completion_signal(&io_signal_);
}

}  // namespace nvme
