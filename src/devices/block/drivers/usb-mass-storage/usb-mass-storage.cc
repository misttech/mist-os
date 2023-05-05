// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "usb-mass-storage.h"

#include <endian.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/scsi/controller.h>
#include <lib/scsi/disk.h>
#include <stdio.h>
#include <string.h>
#include <zircon/assert.h>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <usb/ums.h>
#include <usb/usb.h>

namespace ums {
namespace {

constexpr uint8_t kPlaceholderTarget = 0;

void ReqComplete(void* ctx, usb_request_t* req) {
  if (ctx) {
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  }
}

}  // namespace

class WaiterImpl : public WaiterInterface {
 public:
  zx_status_t Wait(sync_completion_t* completion, zx_duration_t duration) {
    return sync_completion_wait(completion, duration);
  }
};

void UsbMassStorageDevice::ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb,
                                               bool is_write, uint32_t block_size_bytes,
                                               scsi::DiskOp* disk_op) {
  Transaction* txn = containerof(disk_op, Transaction, disk_op);

  if (lun > UINT8_MAX) {
    disk_op->Complete(ZX_ERR_OUT_OF_RANGE);
    return;
  }
  if (cdb.iov_len > sizeof(txn->cdb_buffer)) {
    disk_op->Complete(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  memcpy(txn->cdb_buffer, cdb.iov_base, cdb.iov_len);
  txn->cdb_length = static_cast<uint8_t>(cdb.iov_len);
  txn->lun = static_cast<uint8_t>(lun);
  txn->block_size_bytes = block_size_bytes;

  // Queue transaction.
  {
    fbl::AutoLock l(&txn_lock_);
    list_add_tail(&queued_txns_, &txn->node);
  }
  sync_completion_signal(&txn_completion_);
}

void UsbMassStorageDevice::DdkRelease() {
  Release();
  delete this;
}

void UsbMassStorageDevice::Release() {
  if (cbw_req_) {
    usb_request_release(cbw_req_);
  }
  if (data_req_) {
    usb_request_release(data_req_);
  }
  if (csw_req_) {
    usb_request_release(csw_req_);
  }
  if (data_transfer_req_) {
    // release_frees is indirectly cleared by DataTransfer; set it again here so that
    // data_transfer_req_ is freed by usb_request_release.
    data_transfer_req_->release_frees = true;
    usb_request_release(data_transfer_req_);
  }
  if (worker_thread_.has_value() && worker_thread_->joinable()) {
    worker_thread_->join();
  }
}

void UsbMassStorageDevice::DdkUnbind(ddk::UnbindTxn txn) {
  // terminate our worker thread
  {
    fbl::AutoLock l(&txn_lock_);
    dead_ = true;
  }
  sync_completion_signal(&txn_completion_);

  // wait for worker thread to finish before removing devices
  if (worker_thread_.has_value() && worker_thread_->joinable()) {
    worker_thread_->join();
  }
  // Wait for remaining requests to complete
  while (pending_requests_.load()) {
    waiter_->Wait(&txn_completion_, ZX_SEC(1));
  }
  txn.Reply();
}

void UsbMassStorageDevice::RequestQueue(usb_request_t* request,
                                        const usb_request_complete_callback_t* completion) {
  fbl::AutoLock l(&txn_lock_);
  pending_requests_++;
  UsbRequestContext context;
  context.completion = *completion;
  usb_request_complete_callback_t complete;
  complete.callback = [](void* ctx, usb_request_t* req) {
    UsbRequestContext context;
    memcpy(&context,
           reinterpret_cast<unsigned char*>(req) +
               reinterpret_cast<UsbMassStorageDevice*>(ctx)->parent_req_size_,
           sizeof(context));
    reinterpret_cast<UsbMassStorageDevice*>(ctx)->pending_requests_--;
    context.completion.callback(context.completion.ctx, req);
  };
  complete.ctx = this;
  memcpy(reinterpret_cast<unsigned char*>(request) + parent_req_size_, &context, sizeof(context));
  usb_.RequestQueue(request, &complete);
}

// Performs the object initialization.
zx_status_t UsbMassStorageDevice::Init(bool is_test_mode) {
  dead_ = false;
  is_test_mode_ = is_test_mode;
  // Add root device, which will contain block devices for logical units
  zx_status_t status = DdkAdd("ums", DEVICE_ADD_NON_BINDABLE);
  if (status != ZX_OK) {
    delete this;
    return status;
  }
  return status;
}

void UsbMassStorageDevice::DdkInit(ddk::InitTxn txn) {
  usb::UsbDevice usb(parent());
  if (!usb.is_valid()) {
    txn.Reply(ZX_ERR_PROTOCOL_NOT_SUPPORTED);
    return;
  }

  // find our endpoints
  std::optional<usb::InterfaceList> interfaces;
  zx_status_t status = usb::InterfaceList::Create(usb, true, &interfaces);
  if (status != ZX_OK) {
    txn.Reply(status);
    return;
  }
  auto interface = interfaces->begin();
  if (interface == interfaces->end()) {
    txn.Reply(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  const usb_interface_descriptor_t* interface_descriptor = interface->descriptor();
  // Since interface != interface->end(), interface_descriptor is guaranteed not null.
  ZX_DEBUG_ASSERT(interface_descriptor);
  uint8_t interface_number = interface_descriptor->b_interface_number;
  uint8_t bulk_in_addr = 0;
  uint8_t bulk_out_addr = 0;
  size_t bulk_in_max_packet = 0;
  size_t bulk_out_max_packet = 0;

  if (interface_descriptor->b_num_endpoints < 2) {
    zxlogf(DEBUG, "UMS: ums_bind wrong number of endpoints: %d",
           interface_descriptor->b_num_endpoints);
    txn.Reply(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  for (auto ep_itr : interfaces->begin()->GetEndpointList()) {
    const usb_endpoint_descriptor_t* endp = ep_itr.descriptor();
    if (usb_ep_direction(endp) == USB_ENDPOINT_OUT) {
      if (usb_ep_type(endp) == USB_ENDPOINT_BULK) {
        bulk_out_addr = endp->b_endpoint_address;
        bulk_out_max_packet = usb_ep_max_packet(endp);
      }
    } else {
      if (usb_ep_type(endp) == USB_ENDPOINT_BULK) {
        bulk_in_addr = endp->b_endpoint_address;
        bulk_in_max_packet = usb_ep_max_packet(endp);
      }
    }
  }

  if (!is_test_mode_ && (!bulk_in_max_packet || !bulk_out_max_packet)) {
    zxlogf(DEBUG, "UMS: ums_bind could not find endpoints");
    txn.Reply(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  uint8_t max_lun;
  size_t out_length;
  status = usb.ControlIn(USB_DIR_IN | USB_TYPE_CLASS | USB_RECIP_INTERFACE, USB_REQ_GET_MAX_LUN,
                         0x00, 0x00, ZX_TIME_INFINITE, &max_lun, sizeof(max_lun), &out_length);
  if (status == ZX_ERR_IO_REFUSED) {
    // Devices that do not support multiple LUNS may stall this command.
    // See USB Mass Storage Class Spec. 3.2 Get Max LUN.
    // Clear the stall.
    usb.ResetEndpoint(0);
    zxlogf(INFO, "Device does not support multiple LUNs");
    max_lun = 0;
  } else if (status != ZX_OK) {
    txn.Reply(status);
    return;
  } else if (out_length != sizeof(max_lun)) {
    txn.Reply(ZX_ERR_BAD_STATE);
    return;
  }
  fbl::AllocChecker checker;
  fbl::RefPtr<scsi::Disk>* raw_array;
  raw_array = new (&checker) fbl::RefPtr<scsi::Disk>[max_lun + 1];
  if (!checker.check()) {
    txn.Reply(ZX_ERR_NO_MEMORY);
    return;
  }
  block_devs_ = fbl::Array(raw_array, max_lun + 1);
  zxlogf(DEBUG, "UMS: Max lun is: %u", max_lun);
  max_lun_ = max_lun;

  list_initialize(&queued_txns_);
  sync_completion_reset(&txn_completion_);

  usb_ = usb;
  bulk_in_addr_ = bulk_in_addr;
  bulk_out_addr_ = bulk_out_addr;
  bulk_in_max_packet_ = bulk_in_max_packet;
  bulk_out_max_packet_ = bulk_out_max_packet;
  interface_number_ = interface_number;

  size_t max_in = usb.GetMaxTransferSize(bulk_in_addr);
  size_t max_out = usb.GetMaxTransferSize(bulk_out_addr);
  // SendCbw() accepts max transfer length of UINT32_MAX.
  max_transfer_bytes_ = static_cast<uint32_t>((max_in < max_out ? max_in : max_out));
  parent_req_size_ = usb.GetRequestSize();
  ZX_DEBUG_ASSERT(parent_req_size_ != 0);
  size_t usb_request_size = parent_req_size_ + sizeof(UsbRequestContext);
  status = usb_request_alloc(&cbw_req_, sizeof(ums_cbw_t), bulk_out_addr, usb_request_size);
  if (status != ZX_OK) {
    txn.Reply(status);
    return;
  }
  status = usb_request_alloc(&data_req_, zx_system_get_page_size(), bulk_in_addr, usb_request_size);
  if (status != ZX_OK) {
    txn.Reply(status);
    return;
  }
  status = usb_request_alloc(&csw_req_, sizeof(ums_csw_t), bulk_in_addr, usb_request_size);
  if (status != ZX_OK) {
    txn.Reply(status);
    return;
  }

  status = usb_request_alloc(&data_transfer_req_, 0, bulk_in_addr, usb_request_size);
  if (status != ZX_OK) {
    txn.Reply(status);
    return;
  }

  tag_send_ = tag_receive_ = 8;

  worker_thread_.emplace(
      [this, init_txn = std::move(txn)]() mutable { WorkerThread(std::move(init_txn)); });
}

zx_status_t UsbMassStorageDevice::Reset() {
  // UMS Reset Recovery. See section 5.3.4 of
  // "Universal Serial Bus Mass Storage Class Bulk-Only Transport"
  zxlogf(DEBUG, "UMS: performing reset recovery");
  // Step 1: Send  Bulk-Only Mass Storage Reset
  zx_status_t status =
      usb_.ControlOut(USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE, USB_REQ_RESET, 0,
                      interface_number_, ZX_TIME_INFINITE, NULL, 0);
  usb_protocol_t usb;
  usb_.GetProto(&usb);
  if (status != ZX_OK) {
    zxlogf(DEBUG, "UMS: USB_REQ_RESET failed: %s", zx_status_get_string(status));
    return status;
  }
  // Step 2: Clear Feature HALT to the Bulk-In endpoint
  constexpr uint8_t request_type = USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_ENDPOINT;
  status = usb_.ClearFeature(request_type, USB_ENDPOINT_HALT, bulk_in_addr_, ZX_TIME_INFINITE);
  if (status != ZX_OK) {
    zxlogf(DEBUG, "UMS: clear endpoint halt failed: %s", zx_status_get_string(status));
    return status;
  }
  // Step 3: Clear Feature HALT to the Bulk-Out endpoint
  status = usb_.ClearFeature(request_type, USB_ENDPOINT_HALT, bulk_out_addr_, ZX_TIME_INFINITE);
  if (status != ZX_OK) {
    zxlogf(DEBUG, "UMS: clear endpoint halt failed: %s", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

zx_status_t UsbMassStorageDevice::SendCbw(uint8_t lun, uint32_t transfer_length, uint8_t flags,
                                          uint8_t command_len, void* command) {
  usb_request_t* req = cbw_req_;

  ums_cbw_t* cbw;
  zx_status_t status = usb_request_mmap(req, (void**)&cbw);
  if (status != ZX_OK) {
    zxlogf(DEBUG, "UMS: usb request mmap failed: %s", zx_status_get_string(status));
    return status;
  }

  memset(cbw, 0, sizeof(*cbw));
  cbw->dCBWSignature = htole32(CBW_SIGNATURE);
  cbw->dCBWTag = htole32(tag_send_++);
  cbw->dCBWDataTransferLength = htole32(transfer_length);
  cbw->bmCBWFlags = flags;
  cbw->bCBWLUN = lun;
  cbw->bCBWCBLength = command_len;

  // copy command_len bytes from the command passed in into the command_len
  memcpy(cbw->CBWCB, command, command_len);

  sync_completion_t completion;
  usb_request_complete_callback_t complete = {
      .callback = ReqComplete,
      .ctx = &completion,
  };
  RequestQueue(req, &complete);
  waiter_->Wait(&completion, ZX_TIME_INFINITE);
  return req->response.status;
}

zx_status_t UsbMassStorageDevice::ReadCsw(uint32_t* out_residue) {
  sync_completion_t completion;
  usb_request_complete_callback_t complete = {
      .callback = ReqComplete,
      .ctx = &completion,
  };

  usb_request_t* csw_request = csw_req_;
  RequestQueue(csw_request, &complete);
  waiter_->Wait(&completion, ZX_TIME_INFINITE);
  csw_status_t csw_error = VerifyCsw(csw_request, out_residue);

  if (csw_error == CSW_SUCCESS) {
    return ZX_OK;
  } else if (csw_error == CSW_FAILED) {
    return ZX_ERR_BAD_STATE;
  } else {
    // FIXME - best way to handle this?
    // print error and then reset device due to it
    zxlogf(DEBUG, "UMS: CSW verify returned error. Check ums-hw.h csw_status_t for enum = %d",
           csw_error);
    Reset();
    return ZX_ERR_INTERNAL;
  }
}

csw_status_t UsbMassStorageDevice::VerifyCsw(usb_request_t* csw_request, uint32_t* out_residue) {
  ums_csw_t csw = {};
  [[maybe_unused]] size_t result = usb_request_copy_from(csw_request, &csw, sizeof(csw), 0);

  // check signature is "USBS"
  if (letoh32(csw.dCSWSignature) != CSW_SIGNATURE) {
    zxlogf(DEBUG, "UMS: invalid CSW sig: %08x", letoh32(csw.dCSWSignature));
    return CSW_INVALID;
  }

  // check if tag matches the tag of last CBW
  if (letoh32(csw.dCSWTag) != tag_receive_++) {
    zxlogf(DEBUG, "UMS: CSW tag mismatch, expected:%08x got in CSW:%08x", tag_receive_ - 1,
           letoh32(csw.dCSWTag));
    return CSW_TAG_MISMATCH;
  }
  // check if success is true or not?
  if (csw.bmCSWStatus == CSW_FAILED) {
    return CSW_FAILED;
  } else if (csw.bmCSWStatus == CSW_PHASE_ERROR) {
    return CSW_PHASE_ERROR;
  }

  if (out_residue) {
    *out_residue = letoh32(csw.dCSWDataResidue);
  }
  return CSW_SUCCESS;
}

zx_status_t UsbMassStorageDevice::ReadSync(size_t transfer_length) {
  // Read response code from device
  usb_request_t* read_request = data_req_;
  read_request->header.length = transfer_length;
  sync_completion_t completion;
  usb_request_complete_callback_t complete = {
      .callback = ReqComplete,
      .ctx = &completion,
  };
  RequestQueue(read_request, &complete);
  sync_completion_wait(&completion, ZX_TIME_INFINITE);
  return read_request->response.status;
}

zx_status_t UsbMassStorageDevice::ExecuteCommandSync(uint8_t target, uint16_t lun, iovec cdb,
                                                     bool is_write, iovec data) {
  if (lun > UINT8_MAX) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (data.iov_len > max_transfer_bytes_) {
    zxlogf(ERROR, "Request exceeding max transfer size.");
    return ZX_ERR_INVALID_ARGS;
  }

  // Per section 6.5 of UMS specification version 1.0
  // the device should report any errors in the CSW stage,
  // which seems to suggest that stalling here is out-of-spec.
  // Some devices that we tested with do stall the CBW or data transfer stage,
  // so to accommodate those devices we consider the transfer to have ended (with an error)
  // when we receive a stall condition from the device.
  zx_status_t status =
      SendCbw(static_cast<uint8_t>(lun), static_cast<uint32_t>(data.iov_len),
              is_write ? USB_DIR_OUT : USB_DIR_IN, static_cast<uint8_t>(cdb.iov_len), cdb.iov_base);
  if (status != ZX_OK) {
    zxlogf(WARNING, "UMS: SendCbw failed with status %s", zx_status_get_string(status));
    return status;
  }

  const bool read_data_transfer = !is_write && data.iov_base != nullptr;
  if (read_data_transfer) {
    // read response
    status = ReadSync(data.iov_len);
    if (status != ZX_OK) {
      zxlogf(WARNING, "UMS: ReadSync failed with status %s", zx_status_get_string(status));
      return status;
    }
  }

  // wait for CSW
  status = ReadCsw(NULL);
  if (status == ZX_OK && read_data_transfer) {
    memset(data.iov_base, 0, data.iov_len);
    [[maybe_unused]] auto result = usb_request_copy_from(data_req_, data.iov_base, data.iov_len, 0);
  }
  return status;
}

zx_status_t UsbMassStorageDevice::DataTransfer(zx_handle_t vmo_handle, zx_off_t offset,
                                               size_t length, uint8_t ep_address) {
  usb_request_t* req = data_transfer_req_;

  zx_status_t status = usb_request_init(req, vmo_handle, offset, length, ep_address);
  if (status != ZX_OK) {
    return status;
  }

  sync_completion_t completion;
  usb_request_complete_callback_t complete = {
      .callback = ReqComplete,
      .ctx = &completion,
  };
  RequestQueue(req, &complete);
  waiter_->Wait(&completion, ZX_TIME_INFINITE);

  status = req->response.status;
  if (status == ZX_OK && req->response.actual != length) {
    status = ZX_ERR_IO;
  }

  usb_request_release(req);
  return status;
}

zx_status_t UsbMassStorageDevice::DoTransaction(Transaction* txn, uint8_t flags, uint8_t ep_address,
                                                const std::string& action) {
  const block_op_t& op = txn->disk_op.op;
  const size_t num_bytes = op.rw.length * txn->block_size_bytes;

  zx_status_t status =
      SendCbw(txn->lun, static_cast<uint32_t>(num_bytes), flags, txn->cdb_length, txn->cdb_buffer);
  if (status != ZX_OK) {
    zxlogf(WARNING, "UMS: SendCbw during %s failed with status %s", action.c_str(),
           zx_status_get_string(status));
    return status;
  }

  if (num_bytes) {
    zx_off_t vmo_offset = op.rw.offset_vmo * txn->block_size_bytes;
    status = DataTransfer(op.rw.vmo, vmo_offset, num_bytes, ep_address);
    if (status != ZX_OK) {
      return status;
    }
  }

  // receive CSW
  uint32_t residue;
  status = ReadCsw(&residue);
  if (status == ZX_OK && residue) {
    zxlogf(ERROR, "unexpected residue in %s", action.c_str());
    status = ZX_ERR_IO;
  }

  return status;
}

zx_status_t UsbMassStorageDevice::CheckLunsReady() {
  zx_status_t status = ZX_OK;
  for (uint8_t lun = 0; lun <= max_lun_ && status == ZX_OK; lun++) {
    bool ready = false;

    status = TestUnitReady(kPlaceholderTarget, lun);
    if (status == ZX_OK) {
      ready = true;
    }
    if (status == ZX_ERR_BAD_STATE) {
      ready = false;
      // command returned CSW_FAILED. device is there but media is not ready.
      uint8_t request_sense_data[UMS_REQUEST_SENSE_TRANSFER_LENGTH];
      status = RequestSense(kPlaceholderTarget, lun,
                            {request_sense_data, UMS_REQUEST_SENSE_TRANSFER_LENGTH});
    }
    if (status != ZX_OK) {
      break;
    }
    if (ready && !block_devs_[lun]) {
      zx::result disk =
          scsi::Disk::Bind(zxdev(), this, kPlaceholderTarget, lun, max_transfer_bytes_);
      if (disk.is_ok() && disk->block_size_bytes() != 0) {
        block_devs_[lun] = disk.value();
        scsi::Disk* dev = block_devs_[lun].get();
        zxlogf(DEBUG, "UMS: block size is: 0x%08x", dev->block_size_bytes());
        zxlogf(DEBUG, "UMS: total blocks is: %lu", dev->block_count());
        zxlogf(DEBUG, "UMS: total size is: %lu", dev->block_count() * dev->block_size_bytes());
        zxlogf(DEBUG, "UMS: read-only: %d removable: %d", dev->write_protected(), dev->removable());
      } else {
        zx_status_t error = disk.status_value();
        if (error == ZX_OK) {
          zxlogf(ERROR, "UMS zero block size");
          error = ZX_ERR_INVALID_ARGS;
        }
        zxlogf(ERROR, "UMS: device_add for block device failed: %s", zx_status_get_string(error));
      }
    } else if (!ready && block_devs_[lun]) {
      block_devs_[lun]->DdkAsyncRemove();
      block_devs_[lun] = nullptr;
    }
  }

  return status;
}

int UsbMassStorageDevice::WorkerThread(ddk::InitTxn&& init_txn) {
  zx_status_t status = ZX_OK;
  for (uint8_t lun = 0; lun <= max_lun_; lun++) {
    zx::result inquiry_data = Inquiry(kPlaceholderTarget, lun);
    if (inquiry_data.is_error()) {
      init_txn.Reply(status);
      return 0;
    }
  }

  init_txn.Reply(ZX_OK);
  bool wait = true;
  if (CheckLunsReady() != ZX_OK) {
    return status;
  }

  ums::Transaction* current_txn = nullptr;
  while (1) {
    if (wait) {
      status = waiter_->Wait(&txn_completion_, ZX_SEC(1));
      if (list_is_empty(&queued_txns_) && !dead_) {
        if (CheckLunsReady() != ZX_OK) {
          return status;
        }
        continue;
      }
      sync_completion_reset(&txn_completion_);
    }
    Transaction* txn = nullptr;
    {
      fbl::AutoLock l(&txn_lock_);
      if (dead_) {
        break;
      }
      txn = list_remove_head_type(&queued_txns_, Transaction, node);
      if (txn == NULL) {
        wait = true;
        continue;
      } else {
        wait = false;
      }
      current_txn = txn;
    }
    const block_op_t& op = txn->disk_op.op;
    zxlogf(DEBUG, "UMS PROCESS (%p)", &op);

    scsi::Disk* dev = block_devs_[txn->lun].get();
    zx_status_t status;
    if (dev == nullptr) {
      // Device is absent (likely been removed).
      status = ZX_ERR_PEER_CLOSED;
    } else {
      switch (op.command & BLOCK_OP_MASK) {
        case BLOCK_OP_READ:
          if ((status = DoTransaction(txn, USB_DIR_IN, bulk_in_addr_, "Read")) != ZX_OK) {
            zxlogf(ERROR, "UMS: Read of %u @ %zu failed: %s", op.rw.length, op.rw.offset_dev,
                   zx_status_get_string(status));
          }
          break;
        case BLOCK_OP_WRITE:
          if ((status = DoTransaction(txn, USB_DIR_OUT, bulk_out_addr_, "Write")) != ZX_OK) {
            zxlogf(ERROR, "UMS: Write of %u @ %zu failed: %s", op.rw.length, op.rw.offset_dev,
                   zx_status_get_string(status));
          }
          break;
        case BLOCK_OP_FLUSH:
          if ((status = DoTransaction(txn, USB_DIR_OUT, 0, "Flush")) != ZX_OK) {
            zxlogf(ERROR, "UMS: Flush failed: %s", zx_status_get_string(status));
          }
          break;
        default:
          status = ZX_ERR_INVALID_ARGS;
          break;
      }
    }
    {
      fbl::AutoLock l(&txn_lock_);
      if (current_txn == txn) {
        zxlogf(DEBUG, "UMS DONE %d (%p)", status, &op);
        txn->disk_op.Complete(status);
        current_txn = nullptr;
      }
    }
  }

  // complete any pending txns
  list_node_t txns = LIST_INITIAL_VALUE(txns);
  {
    fbl::AutoLock l(&txn_lock_);
    list_move(&queued_txns_, &txns);
  }

  Transaction* txn;
  while ((txn = list_remove_head_type(&queued_txns_, Transaction, node)) != NULL) {
    const block_op_t& op = txn->disk_op.op;
    switch (op.command & BLOCK_OP_MASK) {
      case BLOCK_OP_READ:
        zxlogf(ERROR, "UMS: read of %u @ %zu discarded during unbind", op.rw.length,
               op.rw.offset_dev);
        break;
      case BLOCK_OP_WRITE:
        zxlogf(ERROR, "UMS: write of %u @ %zu discarded during unbind", op.rw.length,
               op.rw.offset_dev);
        break;
    }
    txn->disk_op.Complete(ZX_ERR_IO_NOT_PRESENT);
  }

  return ZX_OK;
}

static zx_status_t bind(void* ctx, zx_device_t* parent) {
  fbl::AllocChecker checker;
  UsbMassStorageDevice* device(new (&checker)
                                   UsbMassStorageDevice(fbl::MakeRefCounted<WaiterImpl>(), parent));
  if (!checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = device->Init(false /* is_test_mode */);
  return status;
}
static constexpr zx_driver_ops_t usb_mass_storage_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = bind;
  return ops;
}();
}  // namespace ums

ZIRCON_DRIVER(usb_mass_storage, ums::usb_mass_storage_driver_ops, "zircon", "0.1");
