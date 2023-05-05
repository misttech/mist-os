
// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/ums-function/ums-function.h"

#include <assert.h>
#include <fuchsia/hardware/usb/function/cpp/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/scsi/controller.h>
#include <lib/zx/vmar.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/process.h>
#include <zircon/syscalls.h>

#include <fbl/alloc_checker.h>
#include <usb/peripheral.h>
#include <usb/request-cpp.h>
#include <usb/ums.h>
#include <usb/usb-request.h>

namespace ums {

static struct {
  usb_interface_descriptor_t intf;
  usb_endpoint_descriptor_t out_ep;
  usb_endpoint_descriptor_t in_ep;
} descriptors = {
    .intf =
        {
            .b_length = sizeof(usb_interface_descriptor_t),
            .b_descriptor_type = USB_DT_INTERFACE,
            //      .b_interface_number set later
            .b_alternate_setting = 0,
            .b_num_endpoints = 2,
            .b_interface_class = USB_CLASS_MSC,
            .b_interface_sub_class = USB_SUBCLASS_MSC_SCSI,
            .b_interface_protocol = USB_PROTOCOL_MSC_BULK_ONLY,
            .i_interface = 0,
        },
    .out_ep =
        {
            .b_length = sizeof(usb_endpoint_descriptor_t),
            .b_descriptor_type = USB_DT_ENDPOINT,
            //      .b_endpoint_address set later
            .bm_attributes = USB_ENDPOINT_BULK,
            .w_max_packet_size = htole16(UmsFunction::kBulkMaxPacket),
            .b_interval = 0,
        },
    .in_ep =
        {
            .b_length = sizeof(usb_endpoint_descriptor_t),
            .b_descriptor_type = USB_DT_ENDPOINT,
            //      .b_endpoint_address set later
            .bm_attributes = USB_ENDPOINT_BULK,
            .w_max_packet_size = htole16(UmsFunction::kBulkMaxPacket),
            .b_interval = 0,
        },
};

void UmsFunction::RequestQueue(usb::Request<>* req,
                               const usb_request_complete_callback_t* completion) {
  atomic_fetch_add(&pending_request_count_, 1);
  function_.RequestQueue(req->request(), completion);
}

void UmsFunction::CompletionCallback(void* ctx, usb_request_t* req) {
  auto ums = reinterpret_cast<UmsFunction*>(ctx);
  ums->mtx_.Acquire();
  if (req == ums->cbw_req_->request()) {
    ums->cbw_req_complete_ = true;
  } else {
    if (req == ums->data_req_->request()) {
      ums->data_req_complete_ = true;
    } else {
      ums->csw_req_complete_ = true;
    }
  }
  ums->condvar_.Signal();
  ums->mtx_.Release();
}

void UmsFunction::QueueData(usb::Request<>* req) {
  data_length_ += req->request()->header.length;
  req->request()->header.ep_address =
      current_cbw_.bmCBWFlags & USB_DIR_IN ? bulk_in_addr_ : bulk_out_addr_;
  usb_request_complete_callback_t complete = {
      .callback = CompletionCallback,
      .ctx = this,
  };
  RequestQueue(req, &complete);
}

void UmsFunction::QueueCsw(uint8_t status) {
  // first queue next cbw so it is ready to go
  usb_request_complete_callback_t cbw_complete = {
      .callback = CompletionCallback,
      .ctx = this,
  };
  RequestQueue(&cbw_req_.value(), &cbw_complete);

  usb::Request<>* req = &csw_req_.value();
  ums_csw_t* csw;
  req->Mmap(reinterpret_cast<void**>(&csw));

  csw->dCSWSignature = htole32(CSW_SIGNATURE);
  csw->dCSWTag = current_cbw_.dCBWTag;
  csw->dCSWDataResidue = htole32(le32toh(current_cbw_.dCBWDataTransferLength) - data_length_);
  csw->bmCSWStatus = status;

  req->request()->header.length = sizeof(ums_csw_t);
  usb_request_complete_callback_t csw_complete = {
      .callback = CompletionCallback,
      .ctx = this,
  };
  RequestQueue(&csw_req_.value(), &csw_complete);
}

void UmsFunction::ContinueTransfer() {
  usb::Request<>* req = &data_req_.value();

  size_t length = data_remaining_;
  if (length > kDataReqSize) {
    length = kDataReqSize;
  }
  req->request()->header.length = length;

  if (data_state_ == DATA_STATE_READ) {
    size_t result = req->CopyTo(static_cast<char*>(storage_) + data_offset_, length, 0);
    ZX_ASSERT(result == length);
    QueueData(req);
  } else if (data_state_ == DATA_STATE_WRITE) {
    QueueData(req);
  } else {
    zxlogf(ERROR, "ContinueTransfer: bad data state %d", data_state_);
  }
}

void UmsFunction::StartTransfer(DataState state, uint64_t lba, uint32_t blocks) {
  zx_off_t offset = lba * kBlockSize;
  size_t length = blocks * kBlockSize;

  if (offset + length > kStorageSize) {
    zxlogf(ERROR, "StartTransfer: transfer out of range state: %d, lba: %zu blocks: %u", state, lba,
           blocks);
    // TODO(voydanoff) report error to host
    return;
  }

  data_state_ = state;
  data_offset_ = offset;
  data_remaining_ = length;

  ContinueTransfer();
}

void UmsFunction::HandleInquiry(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleInquiry");

  usb::Request<>* req = &data_req_.value();
  scsi::InquiryData* data;
  req->Mmap(reinterpret_cast<void**>(&data));
  memset(data, 0, UMS_INQUIRY_TRANSFER_LENGTH);
  req->request()->header.length = UMS_INQUIRY_TRANSFER_LENGTH;

  // fill in inquiry result
  data->peripheral_device_type = 0;  // Peripheral Device Type: Direct access block device
  data->removable = 0x80;            // Removable
  data->version = 6;                 // Version SPC-4
  data->response_data_format_and_control = 0x12;  // Response Data Format
  memcpy(data->t10_vendor_id, "Google  ", 8);
  memcpy(data->product_id, "Zircon UMS      ", 16);
  memcpy(data->product_revision, "1.00", 4);

  QueueData(req);
}

void UmsFunction::HandleTestUnitReady(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleTestUnitReady");

  // no data phase here. Just return status OK
  QueueCsw(CSW_SUCCESS);
}

void UmsFunction::HandleRequestSense(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleRequestSense");

  usb::Request<>* req = &data_req_.value();
  scsi::SenseDataHeader* data;
  req->Mmap(reinterpret_cast<void**>(&data));
  memset(data, 0, UMS_REQUEST_SENSE_TRANSFER_LENGTH);
  req->request()->header.length = UMS_REQUEST_SENSE_TRANSFER_LENGTH;

  data->response_code = 0x70;
  data->additional_sense_code = 0x20;
  data->additional_sense_length = 10;

  QueueData(req);
}

void UmsFunction::HandleReadCapacity10(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleReadCapacity10");

  usb::Request<>* req = &data_req_.value();
  scsi::ReadCapacity10ParameterData* data;
  req->Mmap(reinterpret_cast<void**>(&data));

  uint64_t lba = kBlockCount - 1;
  if (lba > UINT32_MAX) {
    data->returned_logical_block_address = htobe32(UINT32_MAX);
  } else {
    data->returned_logical_block_address = htobe32(lba);
  }
  data->block_length_in_bytes = htobe32(kBlockSize);

  req->request()->header.length = sizeof(*data);
  QueueData(req);
}

void UmsFunction::HandleReadCapacity16(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleReadCapacity16");

  usb::Request<>* req = &data_req_.value();
  scsi::ReadCapacity16ParameterData* data;
  req->Mmap(reinterpret_cast<void**>(&data));
  memset(data, 0, sizeof(*data));

  data->returned_logical_block_address = htobe64(kBlockCount - 1);
  data->block_length_in_bytes = htobe32(kBlockSize);

  req->request()->header.length = sizeof(*data);
  QueueData(req);
}

void UmsFunction::HandleModeSense6(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleModeSense6");
  usb::Request<>* req = &data_req_.value();
  scsi::ModeSense6ParameterHeader* data;
  req->Mmap(reinterpret_cast<void**>(&data));
  memset(data, 0, sizeof(*data));
  req->request()->header.length = sizeof(*data);
  QueueData(req);
}

void UmsFunction::HandleRead10(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleRead10");

  scsi::Read10CDB* command = reinterpret_cast<scsi::Read10CDB*>(cbw->CBWCB);
  uint64_t lba = be32toh(command->logical_block_address);
  uint32_t blocks = be16toh(command->transfer_length);
  StartTransfer(DATA_STATE_READ, lba, blocks);
}

void UmsFunction::HandleRead12(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleRead12");

  scsi::Read12CDB* command = reinterpret_cast<scsi::Read12CDB*>(cbw->CBWCB);
  uint64_t lba = be32toh(command->logical_block_address);
  uint32_t blocks = be32toh(command->transfer_length);
  StartTransfer(DATA_STATE_READ, lba, blocks);
}

void UmsFunction::HandleRead16(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleRead16");

  scsi::Read16CDB* command = reinterpret_cast<scsi::Read16CDB*>(cbw->CBWCB);
  uint64_t lba = be64toh(command->logical_block_address);
  uint32_t blocks = be32toh(command->transfer_length);
  StartTransfer(DATA_STATE_READ, lba, blocks);
}

void UmsFunction::HandleWrite10(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleWrite10");

  scsi::Write10CDB* command = reinterpret_cast<scsi::Write10CDB*>(cbw->CBWCB);
  uint64_t lba = be32toh(command->logical_block_address);
  uint32_t blocks = be16toh(command->transfer_length);
  StartTransfer(DATA_STATE_WRITE, lba, blocks);
}

void UmsFunction::HandleWrite12(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleWrite12");

  scsi::Write12CDB* command = reinterpret_cast<scsi::Write12CDB*>(cbw->CBWCB);
  uint64_t lba = be32toh(command->logical_block_address);
  uint32_t blocks = be32toh(command->transfer_length);
  StartTransfer(DATA_STATE_WRITE, lba, blocks);
}

void UmsFunction::HandleWrite16(ums_cbw_t* cbw) {
  zxlogf(DEBUG, "HandleWrite16");

  scsi::Write16CDB* command = reinterpret_cast<scsi::Write16CDB*>(cbw->CBWCB);
  uint64_t lba = be64toh(command->logical_block_address);
  uint32_t blocks = be32toh(command->transfer_length);
  StartTransfer(DATA_STATE_WRITE, lba, blocks);
}

void UmsFunction::HandleCbw(ums_cbw_t* cbw) {
  if (le32toh(cbw->dCBWSignature) != CBW_SIGNATURE) {
    zxlogf(ERROR, "HandleCbw: bad dCBWSignature 0x%x", le32toh(cbw->dCBWSignature));
    return;
  }

  // reset data length for computing residue
  data_length_ = 0;

  // all SCSI commands have opcode in the first byte.
  auto opcode = static_cast<scsi::Opcode>(cbw->CBWCB[0]);
  switch (opcode) {
    case scsi::Opcode::INQUIRY:
      HandleInquiry(cbw);
      break;
    case scsi::Opcode::TEST_UNIT_READY:
      HandleTestUnitReady(cbw);
      break;
    case scsi::Opcode::REQUEST_SENSE:
      HandleRequestSense(cbw);
      break;
    case scsi::Opcode::READ_CAPACITY_10:
      HandleReadCapacity10(cbw);
      break;
    case scsi::Opcode::READ_CAPACITY_16:
      HandleReadCapacity16(cbw);
      break;
    case scsi::Opcode::MODE_SENSE_6:
      HandleModeSense6(cbw);
      break;
    case scsi::Opcode::READ_10:
      HandleRead10(cbw);
      break;
    case scsi::Opcode::READ_12:
      HandleRead12(cbw);
      break;
    case scsi::Opcode::READ_16:
      HandleRead16(cbw);
      break;
    case scsi::Opcode::WRITE_10:
      HandleWrite10(cbw);
      break;
    case scsi::Opcode::WRITE_12:
      HandleWrite12(cbw);
      break;
    case scsi::Opcode::WRITE_16:
      HandleWrite16(cbw);
      break;
    case scsi::Opcode::SYNCHRONIZE_CACHE_10:
      // TODO: This is presently untestable.
      // Implement this once we have a means of testing this.
      break;
    default:
      zxlogf(DEBUG, "HandleCbw: unsupported opcode %02Xh", cbw->CBWCB[0]);
      if (cbw->dCBWDataTransferLength) {
        // queue zero length packet to satisfy data phase
        usb::Request<>* req = &data_req_.value();
        req->request()->header.length = 0;
        data_state_ = DATA_STATE_FAILED;
        QueueData(req);
      }
      QueueCsw(CSW_FAILED);
      break;
  }
}

void UmsFunction::CbwComplete(usb::Request<>* req) {
  zxlogf(DEBUG, "CbwComplete %d %ld", req->request()->response.status,
         req->request()->response.actual);

  if (req->request()->response.status == ZX_OK &&
      req->request()->response.actual == sizeof(ums_cbw_t)) {
    ums_cbw_t* cbw = &current_cbw_;
    memset(cbw, 0, sizeof(*cbw));
    [[maybe_unused]] size_t result = req->CopyFrom(cbw, sizeof(*cbw), 0);
    HandleCbw(cbw);
  }
}

void UmsFunction::DataComplete(usb::Request<>* req) {
  zxlogf(DEBUG, "DataComplete %d %ld", req->request()->response.status,
         req->request()->response.actual);

  if (data_state_ == DATA_STATE_WRITE) {
    size_t result = req->CopyFrom(static_cast<char*>(storage_) + data_offset_,
                                  req->request()->response.actual, 0);
    ZX_ASSERT(result == req->request()->response.actual);
  } else if (data_state_ == DATA_STATE_FAILED) {
    data_state_ = DATA_STATE_NONE;
    QueueCsw(CSW_FAILED);
    return;
  } else {
    data_state_ = DATA_STATE_NONE;
    QueueCsw(CSW_SUCCESS);
    return;
  }

  data_offset_ += req->request()->response.actual;
  if (data_remaining_ > req->request()->response.actual) {
    data_remaining_ -= req->request()->response.actual;
  } else {
    data_remaining_ = 0;
  }

  if (data_remaining_ > 0) {
    ContinueTransfer();
  } else {
    data_state_ = DATA_STATE_NONE;
    QueueCsw(CSW_SUCCESS);
  }
}

static void CswComplete(usb::Request<>* req) {
  zxlogf(DEBUG, "CswComplete %d %ld", req->request()->response.status,
         req->request()->response.actual);
}

size_t UmsFunction::UsbFunctionInterfaceGetDescriptorsSize() { return sizeof(descriptors); }

void UmsFunction::UsbFunctionInterfaceGetDescriptors(uint8_t* out_descriptors_buffer,
                                                     size_t descriptors_size,
                                                     size_t* out_descriptors_actual) {
  size_t length = sizeof(descriptors);
  if (length > descriptors_size) {
    length = descriptors_size;
  }
  memcpy(out_descriptors_buffer, &descriptors, length);
  *out_descriptors_actual = length;
}

zx_status_t UmsFunction::UsbFunctionInterfaceControl(const usb_setup_t* setup,
                                                     const uint8_t* write_buffer, size_t write_size,
                                                     uint8_t* out_read_buffer, size_t read_size,
                                                     size_t* out_read_actual) {
  if (setup->bm_request_type == (USB_DIR_IN | USB_TYPE_CLASS | USB_RECIP_INTERFACE) &&
      setup->b_request == USB_REQ_GET_MAX_LUN && setup->w_value == 0 && setup->w_index == 0 &&
      setup->w_length >= sizeof(uint8_t)) {
    *((uint8_t*)out_read_buffer) = 0;
    *out_read_actual = sizeof(uint8_t);
    return ZX_OK;
  }

  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t UmsFunction::UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed) {
  zxlogf(DEBUG, "UsbFunctionInterfaceSetConfigured %d %d", configured, speed);
  zx_status_t status;

  // TODO(voydanoff) fullspeed and superspeed support
  if (configured) {
    if ((status = function_.ConfigEp(&descriptors.out_ep, NULL)) != ZX_OK ||
        (status = function_.ConfigEp(&descriptors.in_ep, NULL)) != ZX_OK) {
      zxlogf(ERROR, "SetConfigured: ConfigEp failed");
    }
  } else {
    if ((status = function_.DisableEp(bulk_out_addr_)) != ZX_OK ||
        (status = function_.DisableEp(bulk_in_addr_)) != ZX_OK) {
      zxlogf(ERROR, "SetConfigured: DisableEp failed");
    }
  }

  if (configured && status == ZX_OK) {
    // queue first read on OUT endpoint
    usb_request_complete_callback_t cbw_complete = {
        .callback = CompletionCallback,
        .ctx = this,
    };
    RequestQueue(&cbw_req_.value(), &cbw_complete);
  }
  return status;
}

zx_status_t UmsFunction::UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting) {
  return ZX_ERR_NOT_SUPPORTED;
}

void UmsFunction::DdkUnbind(ddk::UnbindTxn txn) {
  zxlogf(DEBUG, "UmsFunction::DdkUnbind");

  function_.CancelAll(bulk_out_addr_);
  function_.CancelAll(bulk_in_addr_);
  function_.CancelAll(descriptors.intf.b_interface_number);

  mtx_.Acquire();
  active_ = false;
  condvar_.Signal();
  mtx_.Release();
  int retval;
  thrd_join(thread_, &retval);
  txn.Reply();
}

void UmsFunction::DdkRelease() {
  zxlogf(DEBUG, "UmsFunction::DdkRelease");

  if (storage_) {
    zx_vmar_unmap(zx_vmar_root_self(), (uintptr_t)storage_, kStorageSize);
  }
  if (cbw_req_) {
    cbw_req_->Release();
  }
  if (data_req_) {
    data_req_->Release();
  }
  if (cbw_req_) {
    csw_req_->Release();
  }
  delete this;
}

zx::vmo UmsFunction::vmo_ = zx::vmo();

int UmsFunction::WorkerLoop() {
  ZX_ASSERT(active_);
  while (active_) {
    mtx_.Acquire();
    if (!(cbw_req_complete_ || csw_req_complete_ || data_req_complete_ || (!active_))) {
      condvar_.Wait(&mtx_);
    }
    mtx_.Release();
    if (!active_ && !atomic_load(&pending_request_count_)) {
      return 0;
    }
    if (cbw_req_complete_) {
      atomic_fetch_add(&pending_request_count_, -1);
      cbw_req_complete_ = false;
      CbwComplete(&cbw_req_.value());
    }
    if (csw_req_complete_) {
      atomic_fetch_add(&pending_request_count_, -1);
      csw_req_complete_ = false;
      CswComplete(&csw_req_.value());
    }
    if (data_req_complete_) {
      atomic_fetch_add(&pending_request_count_, -1);
      data_req_complete_ = false;
      DataComplete(&data_req_.value());
    }
  }
  return 0;
}

zx_status_t UmsFunction::Bind(void* ctx, zx_device_t* parent) {
  zxlogf(INFO, "UmsFunction::Bind");

  ddk::UsbFunctionProtocolClient function;
  zx_status_t status = device_get_protocol(parent, ZX_PROTOCOL_USB_FUNCTION, &function);
  if (status != ZX_OK) {
    return status;
  }

  fbl::AllocChecker ac;
  auto driver = fbl::make_unique_checked<UmsFunction>(&ac, parent, function);
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate memory.");
    return ZX_ERR_NO_MEMORY;
  }

  status = driver->DdkAdd(kDriverName);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed DdkAdd: %s", zx_status_get_string(status));
  }

  // The DriverFramework now owns driver.
  driver.release();
  return status;
}

void UmsFunction::DdkInit(ddk::InitTxn txn) {
  // The drive initialization has numerous error conditions. Wrap the initialization here to ensure
  // we always call txn.Reply() in any outcome.
  zx_status_t status = Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Driver initialization failed: %s", zx_status_get_string(status));
  }
  txn.Reply(status);
}

zx_status_t UmsFunction::Init() {
  data_state_ = DATA_STATE_NONE;
  active_ = true;
  atomic_init(&pending_request_count_, 0);
  zx_status_t status = ZX_OK;

  parent_req_size_ = function_.GetRequestSize();
  ZX_DEBUG_ASSERT(parent_req_size_ != 0);

  status = function_.AllocInterface(&descriptors.intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Init: AllocInterface failed");
    return status;
  }
  status = function_.AllocEp(USB_DIR_OUT, &bulk_out_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Init: AllocEp(USB_DIR_OUT, ...) failed");
    return status;
  }
  status = function_.AllocEp(USB_DIR_IN, &bulk_in_addr_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Init: AllocEp(USB_DIR_IN, ...) failed");
    return status;
  }
  descriptors.out_ep.b_endpoint_address = bulk_out_addr_;
  descriptors.in_ep.b_endpoint_address = bulk_in_addr_;

  status = usb::Request<>::Alloc(&cbw_req_, kBulkMaxPacket, bulk_out_addr_, parent_req_size_);
  if (status != ZX_OK) {
    return status;
  }
  // Endpoint for data_req depends on current_cbw.bmCBWFlags,
  // and will be set in QueueData.
  status = usb::Request<>::Alloc(&data_req_, kDataReqSize, 0, parent_req_size_);
  if (status != ZX_OK) {
    return status;
  }
  status = usb::Request<>::Alloc(&csw_req_, kBulkMaxPacket, bulk_in_addr_, parent_req_size_);
  if (status != ZX_OK) {
    return status;
  }
  // create and map a VMO
  if (!vmo_.is_valid()) {
    status = vmo_.create(kStorageSize, 0, &vmo_);
    if (status != ZX_OK) {
      return status;
    }
  }
  status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo_, 0, kStorageSize,
                                      (zx_vaddr_t*)&storage_);
  if (status != ZX_OK) {
    return status;
  }

  csw_req_->request()->header.length = sizeof(ums_csw_t);

  function_.SetInterface(this, &usb_function_interface_protocol_ops_);
  thrd_create_with_name(
      &thread_, [](void* ctx) { return reinterpret_cast<UmsFunction*>(ctx)->WorkerLoop(); }, this,
      "ums_worker");
  return ZX_OK;
}

static zx_driver_ops_t usb_ums_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = UmsFunction::Bind,
};

}  // namespace ums

// clang-format off
ZIRCON_DRIVER(usb_ums, ums::usb_ums_ops, "zircon", "0.1");
