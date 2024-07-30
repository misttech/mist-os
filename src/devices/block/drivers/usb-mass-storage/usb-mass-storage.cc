// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "usb-mass-storage.h"

#include <endian.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/scsi/block-device.h>
#include <lib/scsi/controller.h>
#include <stdio.h>
#include <string.h>
#include <zircon/assert.h>

#include <mutex>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>
#include <safemath/safe_conversions.h>
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
                                               scsi::DeviceOp* device_op, iovec data) {
  Transaction* txn = containerof(device_op, Transaction, device_op);

  if (lun > UINT8_MAX) {
    device_op->Complete(ZX_ERR_OUT_OF_RANGE);
    return;
  }
  if (cdb.iov_len > sizeof(txn->cdb_buffer)) {
    device_op->Complete(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  memcpy(txn->cdb_buffer, cdb.iov_base, cdb.iov_len);
  txn->cdb_length = static_cast<uint8_t>(cdb.iov_len);
  txn->lun = static_cast<uint8_t>(lun);
  txn->block_size_bytes = block_size_bytes;

  // Currently, data is only used in the UNMAP command.
  if (device_op->op.command.opcode == BLOCK_OPCODE_TRIM && data.iov_len != 0) {
    if (sizeof(txn->data_buffer) != data.iov_len) {
      FDF_LOG(ERROR,
              "The size of the requested data buffer(%zu) and data_buffer(%lu) are different.",
              data.iov_len, sizeof(txn->data_buffer));
      device_op->Complete(ZX_ERR_INVALID_ARGS);
      return;
    }
    memcpy(txn->data_buffer, data.iov_base, data.iov_len);
  }

  // Queue transaction.
  {
    std::lock_guard<std::mutex> l(txn_lock_);
    list_add_tail(&queued_txns_, &txn->node);
  }
  sync_completion_signal(&txn_completion_);
}

void UsbMassStorageDevice::PrepareStop(fdf::PrepareStopCompleter completer) {
  // wait for worker loop to finish before removing devices
  if (worker_dispatcher_.get()) {
    // terminate our worker loop
    {
      std::lock_guard<std::mutex> l(txn_lock_);
      dead_ = true;
    }

    sync_completion_signal(&txn_completion_);
    worker_dispatcher_.ShutdownAsync();
    worker_shutdown_completion_.Wait();
  }

  // Wait for remaining requests to complete
  while (pending_requests_.load()) {
    waiter_->Wait(&txn_completion_, ZX_SEC(1));
  }

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
  completer(zx::ok());
}

void UsbMassStorageDevice::RequestQueue(usb_request_t* request,
                                        const usb_request_complete_callback_t* completion) {
  std::lock_guard<std::mutex> l(txn_lock_);
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
zx_status_t UsbMassStorageDevice::Init() {
  zx::result<ddk::UsbProtocolClient> client =
      compat::ConnectBanjo<ddk::UsbProtocolClient>(incoming());
  if (client.is_error()) {
    FDF_LOG(ERROR, "Failed to connect USB protocol client: %s", client.status_string());
    return client.status_value();
  }

  usb_protocol_t proto;
  client->GetProto(&proto);
  usb::UsbDevice usb(&proto);
  if (!usb.is_valid()) {
    return ZX_ERR_PROTOCOL_NOT_SUPPORTED;
  }

  // find our endpoints
  std::optional<usb::InterfaceList> interfaces;
  zx_status_t status = usb::InterfaceList::Create(usb, true, &interfaces);
  if (status != ZX_OK) {
    return status;
  }
  auto interface = interfaces->begin();
  if (interface == interfaces->end()) {
    return ZX_ERR_NOT_SUPPORTED;
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
    FDF_LOG(DEBUG, "UMS: ums_bind wrong number of endpoints: %d",
            interface_descriptor->b_num_endpoints);
    return ZX_ERR_NOT_SUPPORTED;
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

  if (!bulk_in_max_packet || !bulk_out_max_packet) {
    FDF_LOG(DEBUG, "UMS: ums_bind could not find endpoints");
    return ZX_ERR_NOT_SUPPORTED;
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
    FDF_LOG(INFO, "Device does not support multiple LUNs");
    max_lun = 0;
  } else if (status != ZX_OK) {
    return status;
  } else if (out_length != sizeof(max_lun)) {
    return ZX_ERR_BAD_STATE;
  }
  block_devs_ = std::vector<std::unique_ptr<scsi::BlockDevice>>(max_lun + 1);
  FDF_LOG(DEBUG, "UMS: Max lun is: %u", max_lun);
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
    return status;
  }
  status = usb_request_alloc(&data_req_, zx_system_get_page_size(), bulk_in_addr, usb_request_size);
  if (status != ZX_OK) {
    return status;
  }
  status = usb_request_alloc(&csw_req_, sizeof(ums_csw_t), bulk_in_addr, usb_request_size);
  if (status != ZX_OK) {
    return status;
  }

  status = usb_request_alloc(&data_transfer_req_, 0, bulk_in_addr, usb_request_size);
  if (status != ZX_OK) {
    return status;
  }

  tag_send_ = tag_receive_ = 8;

  for (uint8_t lun = 0; lun <= max_lun_; lun++) {
    zx::result inquiry_data = Inquiry(kPlaceholderTarget, lun);
    if (inquiry_data.is_error()) {
      return inquiry_data.status_value();
    }
  }

  status = CheckLunsReady();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed initial check of whether LUNs are ready: %s",
            zx_status_get_string(status));
    return status;
  }

  auto dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "ums-worker",
      [&](fdf_dispatcher_t*) { worker_shutdown_completion_.Signal(); });
  if (dispatcher.is_error()) {
    FDF_LOG(ERROR, "Failed to create dispatcher: %s",
            zx_status_get_string(dispatcher.status_value()));
    return dispatcher.status_value();
  }
  worker_dispatcher_ = *std::move(dispatcher);

  status = async::PostTask(worker_dispatcher_.async_dispatcher(), [this] { WorkerLoop(); });
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to start worker loop: %s", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

zx_status_t UsbMassStorageDevice::Reset() {
  // UMS Reset Recovery. See section 5.3.4 of
  // "Universal Serial Bus Mass Storage Class Bulk-Only Transport"
  FDF_LOG(DEBUG, "UMS: performing reset recovery");
  // Step 1: Send  Bulk-Only Mass Storage Reset
  zx_status_t status =
      usb_.ControlOut(USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE, USB_REQ_RESET, 0,
                      interface_number_, ZX_TIME_INFINITE, NULL, 0);
  usb_protocol_t usb;
  usb_.GetProto(&usb);
  if (status != ZX_OK) {
    FDF_LOG(DEBUG, "UMS: USB_REQ_RESET failed: %s", zx_status_get_string(status));
    return status;
  }
  // Step 2: Clear Feature HALT to the Bulk-In endpoint
  constexpr uint8_t request_type = USB_DIR_OUT | USB_RECIP_ENDPOINT;
  status = usb_.ClearFeature(request_type, USB_ENDPOINT_HALT, bulk_in_addr_, ZX_TIME_INFINITE);
  if (status != ZX_OK) {
    FDF_LOG(DEBUG, "UMS: clear endpoint halt failed: %s", zx_status_get_string(status));
    return status;
  }
  // Step 3: Clear Feature HALT to the Bulk-Out endpoint
  status = usb_.ClearFeature(request_type, USB_ENDPOINT_HALT, bulk_out_addr_, ZX_TIME_INFINITE);
  if (status != ZX_OK) {
    FDF_LOG(DEBUG, "UMS: clear endpoint halt failed: %s", zx_status_get_string(status));
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
    FDF_LOG(DEBUG, "UMS: usb request mmap failed: %s", zx_status_get_string(status));
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

zx_status_t UsbMassStorageDevice::ReadCsw(uint32_t* out_residue, bool retry) {
  sync_completion_t completion;
  usb_request_complete_callback_t complete = {
      .callback = ReqComplete,
      .ctx = &completion,
  };

  usb_request_t* csw_request = csw_req_;
  RequestQueue(csw_request, &complete);
  waiter_->Wait(&completion, ZX_TIME_INFINITE);
  if (csw_request->response.status != ZX_OK) {
    if (csw_request->response.status == ZX_ERR_IO_REFUSED) {
      if (retry) {
        Reset();
        return csw_request->response.status;
      }
      // Stalled. Clear Bulk In endpoint and try to receive CSW again.
      auto status = usb_.ResetEndpoint(bulk_in_addr_);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "ResetEndpoint failed %d", status);
        return status;
      }
      constexpr uint8_t request_type = USB_DIR_OUT | USB_RECIP_ENDPOINT;
      status = usb_.ClearFeature(request_type, USB_ENDPOINT_HALT, bulk_in_addr_, ZX_TIME_INFINITE);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "UMS: clear endpoint halt failed: %s", zx_status_get_string(status));
        return status;
      }
      return ReadCsw(out_residue, true);
    }
    return csw_request->response.status;
  }
  csw_status_t csw_error = VerifyCsw(csw_request, out_residue);

  if (csw_error == CSW_SUCCESS) {
    return ZX_OK;
  } else if (csw_error == CSW_FAILED) {
    return ZX_ERR_BAD_STATE;
  } else {
    // FIXME - best way to handle this?
    // print error and then reset device due to it
    FDF_LOG(DEBUG, "UMS: CSW verify returned error. Check ums-hw.h csw_status_t for enum = %d",
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
    FDF_LOG(DEBUG, "UMS: invalid CSW sig: %08x", letoh32(csw.dCSWSignature));
    return CSW_INVALID;
  }

  // check if tag matches the tag of last CBW
  if (letoh32(csw.dCSWTag) != tag_receive_++) {
    FDF_LOG(DEBUG, "UMS: CSW tag mismatch, expected:%08x got in CSW:%08x", tag_receive_ - 1,
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
    FDF_LOG(ERROR, "Request exceeding max transfer size.");
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
    FDF_LOG(WARNING, "UMS: SendCbw failed with status %s", zx_status_get_string(status));
    return status;
  }

  const bool read_data_transfer = !is_write && data.iov_base != nullptr;
  if (read_data_transfer) {
    // read response
    status = ReadSync(data.iov_len);
    if (status != ZX_OK) {
      FDF_LOG(WARNING, "UMS: ReadSync failed with status %s", zx_status_get_string(status));
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
  const block_op_t& op = txn->device_op.op;

  const size_t num_bytes = op.command.opcode == BLOCK_OPCODE_TRIM
                               ? sizeof(txn->data_buffer)
                               : op.rw.length * txn->block_size_bytes;

  zx_status_t status =
      SendCbw(txn->lun, static_cast<uint32_t>(num_bytes), flags, txn->cdb_length, txn->cdb_buffer);
  if (status != ZX_OK) {
    FDF_LOG(WARNING, "UMS: SendCbw during %s failed with status %s", action.c_str(),
            zx_status_get_string(status));
    return status;
  }

  if (num_bytes) {
    if (op.command.opcode == BLOCK_OPCODE_TRIM) {
      status = DataTransfer(txn->data_vmo.get(), 0, num_bytes, ep_address);
    } else {
      zx_off_t vmo_offset = op.rw.offset_vmo * txn->block_size_bytes;
      status = DataTransfer(op.rw.vmo, vmo_offset, num_bytes, ep_address);
    }

    if (status != ZX_OK) {
      return status;
    }
  }

  // receive CSW
  uint32_t residue;
  status = ReadCsw(&residue);
  if (status == ZX_OK && residue) {
    FDF_LOG(ERROR, "unexpected residue in %s", action.c_str());
    status = ZX_ERR_IO;
  }

  return status;
}

zx_status_t UsbMassStorageDevice::CheckLunsReady() {
  if (dead_) {
    return ZX_OK;
  }
  std::lock_guard<std::mutex> lock(luns_lock_);

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
      scsi::DeviceOptions options(/*check_unmap_support*/ true, /*use_mode_sense_6*/ true,
                                  /*use_read_write_12*/ false);
      zx::result block_device =
          scsi::BlockDevice::Bind(this, kPlaceholderTarget, lun, max_transfer_bytes_, options);
      if (block_device.is_ok() && block_device->block_size_bytes() != 0) {
        block_devs_[lun] = std::move(block_device.value());
        scsi::BlockDevice* dev = block_devs_[lun].get();
        FDF_LOG(DEBUG, "UMS: block size is: 0x%08x", dev->block_size_bytes());
        FDF_LOG(DEBUG, "UMS: total blocks is: %lu", dev->block_count());
        FDF_LOG(DEBUG, "UMS: total size is: %lu", dev->block_count() * dev->block_size_bytes());
        FDF_LOG(DEBUG, "UMS: read-only: %d removable: %d", dev->write_protected(),
                dev->removable());
      } else {
        zx_status_t error = block_device.status_value();
        if (error == ZX_OK) {
          FDF_LOG(ERROR, "UMS zero block size");
          error = ZX_ERR_INVALID_ARGS;
        }
        FDF_LOG(ERROR, "UMS: device_add for block device failed: %s", zx_status_get_string(error));
      }
    } else if (!ready && block_devs_[lun]) {
      block_devs_[lun]->RemoveDevice();
      block_devs_[lun] = nullptr;
    }
  }

  return status;
}

zx::result<> UsbMassStorageDevice::AllocatePages(zx::vmo& vmo, fzl::VmoMapper& mapper,
                                                 size_t size) {
  const uint32_t data_size =
      fbl::round_up(safemath::checked_cast<uint32_t>(size), zx_system_get_page_size());
  if (zx_status_t status = zx::vmo::create(data_size, 0, &vmo); status != ZX_OK) {
    return zx::error(status);
  }

  if (zx_status_t status = mapper.Map(vmo, 0, data_size); status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map IO buffer: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

void UsbMassStorageDevice::WorkerLoop() {
  bool wait = true;
  ums::Transaction* current_txn = nullptr;
  while (1) {
    if (wait) {
      waiter_->Wait(&txn_completion_, ZX_SEC(1));
      if (list_is_empty(&queued_txns_) && !dead_) {
        async::PostTask(dispatcher(), [&] {
          // This must be done in the default dispatcher because it accesses
          // fdf::DriverBase::outgoing().
          zx_status_t status = CheckLunsReady();
          if (status != ZX_OK) {
            FDF_LOG(ERROR, "Failed to periodically check whether LUNs are ready: %s",
                    zx_status_get_string(status));
          }
        });
        continue;
      }
      sync_completion_reset(&txn_completion_);
    }
    std::lock_guard<std::mutex> lock(luns_lock_);
    Transaction* txn = nullptr;
    {
      std::lock_guard<std::mutex> l(txn_lock_);
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
    const block_op_t& op = txn->device_op.op;
    FDF_LOG(DEBUG, "UMS PROCESS (%p)", &op);

    scsi::BlockDevice* dev = block_devs_[txn->lun].get();
    zx_status_t status;
    if (dev == nullptr) {
      // Device is absent (likely been removed).
      status = ZX_ERR_PEER_CLOSED;
    } else {
      switch (op.command.opcode) {
        case BLOCK_OPCODE_READ:
          if ((status = DoTransaction(txn, USB_DIR_IN, bulk_in_addr_, "Read")) != ZX_OK) {
            FDF_LOG(ERROR, "UMS: Read of %u @ %zu failed: %s", op.rw.length, op.rw.offset_dev,
                    zx_status_get_string(status));
          }
          break;
        case BLOCK_OPCODE_WRITE:
          if ((status = DoTransaction(txn, USB_DIR_OUT, bulk_out_addr_, "Write")) != ZX_OK) {
            FDF_LOG(ERROR, "UMS: Write of %u @ %zu failed: %s", op.rw.length, op.rw.offset_dev,
                    zx_status_get_string(status));
          }
          break;
        case BLOCK_OPCODE_FLUSH:
          if ((status = DoTransaction(txn, USB_DIR_OUT, 0, "Flush")) != ZX_OK) {
            FDF_LOG(ERROR, "UMS: Flush failed: %s", zx_status_get_string(status));
          }
          break;
        case BLOCK_OPCODE_TRIM: {
          // For the UNMAP command, a data buffer is required for the parameter list.
          // TODO: The data_vmo should be modified to recycle.
          zx::vmo data_vmo;
          fzl::VmoMapper mapper;
          if (zx::result<> result = AllocatePages(data_vmo, mapper, sizeof(txn->data_buffer));
              result.is_error()) {
            FDF_LOG(ERROR, "Failed to allocate data buffer (command %p): %s", txn,
                    result.status_string());
            status = result.status_value();
            break;
          }
          memcpy(mapper.start(), txn->data_buffer, sizeof(txn->data_buffer));
          txn->data_vmo = std::move(data_vmo);

          if ((status = DoTransaction(txn, USB_DIR_OUT, bulk_out_addr_, "Trim")) != ZX_OK) {
            FDF_LOG(ERROR, "UMS: Trim failed: %s", zx_status_get_string(status));
          }
          break;
        }
        default:
          status = ZX_ERR_INVALID_ARGS;
          break;
      }
    }
    {
      std::lock_guard<std::mutex> l(txn_lock_);
      if (current_txn == txn) {
        FDF_LOG(DEBUG, "UMS DONE %d (%p)", status, &op);
        txn->data_vmo.reset();
        txn->device_op.Complete(status);
        current_txn = nullptr;
      }
    }
  }

  // complete any pending txns
  list_node_t txns = LIST_INITIAL_VALUE(txns);
  {
    std::lock_guard<std::mutex> l(txn_lock_);
    list_move(&queued_txns_, &txns);
  }

  Transaction* txn;
  while ((txn = list_remove_head_type(&queued_txns_, Transaction, node)) != NULL) {
    const block_op_t& op = txn->device_op.op;
    switch (op.command.opcode) {
      case BLOCK_OPCODE_READ:
        FDF_LOG(ERROR, "UMS: read of %u @ %zu discarded during unbind", op.rw.length,
                op.rw.offset_dev);
        break;
      case BLOCK_OPCODE_WRITE:
        FDF_LOG(ERROR, "UMS: write of %u @ %zu discarded during unbind", op.rw.length,
                op.rw.offset_dev);
        break;
    }
    txn->device_op.Complete(ZX_ERR_IO_NOT_PRESENT);
  }
}

zx::result<> UsbMassStorageDevice::Start() {
  parent_node_.Bind(std::move(node()));

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
  auto [node_client_end, node_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::Node>::Create();

  node_controller_.Bind(std::move(controller_client_end));
  root_node_.Bind(std::move(node_client_end));

  fidl::Arena arena;

  const auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, name()).Build();

  // Add root device, which will contain block devices for logical units
  auto result =
      parent_node_->AddChild(args, std::move(controller_server_end), std::move(node_server_end));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
    return zx::error(result.status());
  }

  waiter_ = fbl::MakeRefCounted<WaiterImpl>();
  zx_status_t status = Init();
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok();
}

}  // namespace ums

FUCHSIA_DRIVER_EXPORT(ums::UsbMassStorageDevice);
