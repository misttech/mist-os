// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../usb-mass-storage.h"

#include <zircon/process.h>

#include <variant>

#include <fbl/array.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

constexpr uint8_t kBlockSize = 5;

// Mock device based on code from ums-function.c

using usb_descriptor = std::variant<usb_interface_descriptor_t, usb_endpoint_descriptor_t>;

const usb_descriptor kDescriptors[] = {
    // Interface descriptor
    usb_interface_descriptor_t{sizeof(usb_descriptor), USB_DT_INTERFACE, 0, 0, 2, 8, 7, 0x50, 0},
    // IN endpoint
    usb_endpoint_descriptor_t{sizeof(usb_descriptor), USB_DT_ENDPOINT, USB_DIR_IN,
                              USB_ENDPOINT_BULK, 64, 0},
    // OUT endpoint
    usb_endpoint_descriptor_t{sizeof(usb_descriptor), USB_DT_ENDPOINT, USB_DIR_OUT,
                              USB_ENDPOINT_BULK, 64, 0}};

struct Packet;
struct Packet : fbl::DoublyLinkedListable<fbl::RefPtr<Packet>>, fbl::RefCounted<Packet> {
  explicit Packet(fbl::Array<unsigned char>&& source) { data = std::move(source); }
  explicit Packet() { stall = true; }
  bool stall = false;
  fbl::Array<unsigned char> data;
};

class FakeTimer : public ums::WaiterInterface {
 public:
  zx_status_t Wait(sync_completion_t* completion, zx_duration_t duration) override {
    return timeout_handler_(completion, duration);
  }

  void set_timeout_handler(fit::function<zx_status_t(sync_completion_t*, zx_duration_t)> handler) {
    timeout_handler_ = std::move(handler);
  }

 private:
  fit::function<zx_status_t(sync_completion_t*, zx_duration_t)> timeout_handler_;
};

enum ErrorInjection {
  NoFault,
  RejectCacheCbw,
  RejectCacheDataStage,
};

constexpr auto kInitialTagValue = 8;

struct Context {
  fbl::DoublyLinkedList<fbl::RefPtr<Packet>> pending_packets;
  ums_csw_t csw;
  const usb_descriptor* descs;
  size_t desc_length;
  sync_completion_t completion;
  zx_status_t status;
  block_op_t* op;
  uint64_t transfer_offset;
  uint64_t transfer_blocks;
  scsi::Opcode transfer_type;
  uint8_t transfer_lun;
  size_t pending_write;
  ErrorInjection failure_mode;
  fbl::RefPtr<Packet> last_transfer;
  uint32_t tag = kInitialTagValue;
};

static size_t GetDescriptorLength(void* ctx) {
  Context* context = reinterpret_cast<Context*>(ctx);
  return context->desc_length;
}

static void GetDescriptors(void* ctx, uint8_t* buffer, size_t size, size_t* outsize) {
  Context* context = reinterpret_cast<Context*>(ctx);
  *outsize = context->desc_length > size ? size : context->desc_length;
  memcpy(buffer, context->descs, *outsize);
}

static zx_status_t ControlIn(void* ctx, uint8_t request_type, uint8_t request, uint16_t value,
                             uint16_t index, int64_t timeout, uint8_t* out_read_buffer,
                             size_t read_size, size_t* out_read_actual) {
  switch (request) {
    case USB_REQ_GET_MAX_LUN: {
      if (!read_size) {
        *out_read_actual = 0;
        return ZX_OK;
      }
      *reinterpret_cast<unsigned char*>(out_read_buffer) = 3;
      *out_read_actual = 1;
      return ZX_OK;
    }
    default:
      return ZX_ERR_IO_REFUSED;
  }
}

static size_t GetMaxTransferSize(void* ctx, uint8_t ep) {
  switch (ep) {
    case USB_DIR_OUT:
      __FALLTHROUGH;
    case USB_DIR_IN:
      // 10MB transfer size (to test large transfers)
      // (is this even possible in real hardware?)
      return 1000 * 1000 * 10;
    default:
      return 0;
  }
}
static size_t GetRequestSize(void* ctx) { return sizeof(usb_request_t); }
static void RequestQueue(void* ctx, usb_request_t* usb_request,
                         const usb_request_complete_callback_t* complete_cb) {
  Context* context = reinterpret_cast<Context*>(ctx);
  if (context->pending_write) {
    void* data;
    usb_request_mmap(usb_request, &data);
    memcpy(context->last_transfer->data.data(), data, context->pending_write);
    context->pending_write = 0;
    usb_request->response.status = ZX_OK;
    complete_cb->callback(complete_cb->ctx, usb_request);
    return;
  }
  if ((usb_request->header.ep_address & USB_ENDPOINT_DIR_MASK) == USB_ENDPOINT_IN) {
    if (context->pending_packets.begin() == context->pending_packets.end()) {
      usb_request->response.status = ZX_OK;
      complete_cb->callback(complete_cb->ctx, usb_request);
    } else {
      auto packet = context->pending_packets.pop_front();
      if (packet->stall) {
        usb_request->response.actual = 0;
        usb_request->response.status = ZX_ERR_IO_REFUSED;
        complete_cb->callback(complete_cb->ctx, usb_request);
        return;
      }
      size_t len =
          usb_request->size < packet->data.size() ? usb_request->size : packet->data.size();
      size_t result = usb_request_copy_to(usb_request, packet->data.data(), len, 0);
      ZX_ASSERT(result == len);
      usb_request->response.actual = len;
      usb_request->response.status = ZX_OK;
      complete_cb->callback(complete_cb->ctx, usb_request);
    }
    return;
  }
  void* data;
  usb_request_mmap(usb_request, &data);
  uint32_t header;
  memcpy(&header, data, sizeof(header));
  header = le32toh(header);
  switch (header) {
    case CBW_SIGNATURE: {
      ums_cbw_t cbw;
      memcpy(&cbw, data, sizeof(cbw));
      if (cbw.bCBWLUN > 3) {
        usb_request->response.status = ZX_OK;
        complete_cb->callback(complete_cb->ctx, usb_request);
        return;
      }
      auto DataTransfer = [&]() {
        if ((context->transfer_offset == cbw.bCBWLUN) &&
            (context->transfer_blocks == (cbw.bCBWLUN + 1U) * 65534U)) {
          auto opcode = static_cast<scsi::Opcode>(cbw.CBWCB[0]);
          size_t transfer_length = context->transfer_blocks * kBlockSize;
          fbl::Array<unsigned char> transfer(new unsigned char[transfer_length], transfer_length);
          context->last_transfer = fbl::MakeRefCounted<Packet>(std::move(transfer));
          context->transfer_lun = cbw.bCBWLUN;
          if ((opcode == scsi::Opcode::READ_10) || (opcode == scsi::Opcode::READ_12) ||
              (opcode == scsi::Opcode::READ_16)) {
            // Push reply
            context->pending_packets.push_back(context->last_transfer);
          } else {
            if ((opcode == scsi::Opcode::WRITE_10) || (opcode == scsi::Opcode::WRITE_12) ||
                (opcode == scsi::Opcode::WRITE_16)) {
              context->pending_write = transfer_length;
            }
          }
          // Push CSW
          fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
          context->csw.dCSWDataResidue = 0;
          context->csw.dCSWTag = context->tag++;
          context->csw.bmCSWStatus = CSW_SUCCESS;
          memcpy(csw.data(), &context->csw, sizeof(context->csw));
          context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
        }
      };
      auto opcode = static_cast<scsi::Opcode>(cbw.CBWCB[0]);
      switch (opcode) {
        case scsi::Opcode::WRITE_16:
          __FALLTHROUGH;
        case scsi::Opcode::READ_16: {
          scsi::Read16CDB cmd;  // struct-wise equivalent to scsi::Write16CDB here.
          memcpy(&cmd, cbw.CBWCB, sizeof(cmd));
          context->transfer_blocks = be32toh(cmd.transfer_length);
          context->transfer_offset = be64toh(cmd.logical_block_address);
          context->transfer_type = opcode;
          DataTransfer();
          usb_request->response.status = ZX_OK;
          complete_cb->callback(complete_cb->ctx, usb_request);
          break;
        }
        case scsi::Opcode::WRITE_12:
          __FALLTHROUGH;
        case scsi::Opcode::READ_12: {
          scsi::Read12CDB cmd;  // struct-wise equivalent to scsi::Write12CDB here.
          memcpy(&cmd, cbw.CBWCB, sizeof(cmd));
          context->transfer_blocks = be32toh(cmd.transfer_length);
          context->transfer_offset = be32toh(cmd.logical_block_address);
          context->transfer_type = opcode;
          DataTransfer();
          usb_request->response.status = ZX_OK;
          complete_cb->callback(complete_cb->ctx, usb_request);
          break;
        }
        case scsi::Opcode::WRITE_10:
          __FALLTHROUGH;
        case scsi::Opcode::READ_10: {
          scsi::Read10CDB cmd;  // struct-wise equivalent to scsi::Write10CDB here.
          memcpy(&cmd, cbw.CBWCB, sizeof(cmd));
          context->transfer_blocks = be16toh(cmd.transfer_length);
          context->transfer_offset = be32toh(cmd.logical_block_address);
          context->transfer_type = opcode;
          DataTransfer();
          usb_request->response.status = ZX_OK;
          complete_cb->callback(complete_cb->ctx, usb_request);
          break;
        }
        case scsi::Opcode::SYNCHRONIZE_CACHE_10: {
          // Push CSW
          fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
          context->csw.dCSWDataResidue = 0;
          context->csw.dCSWTag = context->tag++;
          context->csw.bmCSWStatus = CSW_SUCCESS;
          context->transfer_lun = cbw.bCBWLUN;
          context->transfer_type = opcode;
          memcpy(csw.data(), &context->csw, sizeof(context->csw));
          context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
          usb_request->response.status = ZX_OK;
          complete_cb->callback(complete_cb->ctx, usb_request);
          break;
        }
        case scsi::Opcode::INQUIRY: {
          scsi::InquiryCDB cmd;
          memcpy(&cmd, cbw.CBWCB, sizeof(cmd));
          if (be16toh(cmd.allocation_length) == UMS_INQUIRY_TRANSFER_LENGTH) {
            // Push reply
            fbl::Array<unsigned char> reply(new unsigned char[UMS_INQUIRY_TRANSFER_LENGTH],
                                            UMS_INQUIRY_TRANSFER_LENGTH);
            reply[0] = 0;     // Peripheral Device Type: Direct access block device
            reply[1] = 0x80;  // Removable
            reply[2] = 6;     // Version SPC-4
            reply[3] = 0x12;  // Response Data Format
            memcpy(reply.data() + 8, "Google  ", 8);
            memcpy(reply.data() + 16, "Zircon UMS      ", 16);
            memcpy(reply.data() + 32, "1.00", 4);
            memset(reply.data(), 0, UMS_INQUIRY_TRANSFER_LENGTH);
            context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(reply)));
            // Push CSW
            fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
            context->csw.dCSWDataResidue = 0;
            context->csw.dCSWTag = context->tag++;
            context->csw.bmCSWStatus = CSW_SUCCESS;
            memcpy(csw.data(), &context->csw, sizeof(context->csw));
            context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
          }
          usb_request->response.status = ZX_OK;
          complete_cb->callback(complete_cb->ctx, usb_request);
          break;
        }
        case scsi::Opcode::TEST_UNIT_READY: {
          // Push CSW
          fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
          context->csw.dCSWDataResidue = 0;
          context->csw.dCSWTag = context->tag++;
          context->csw.bmCSWStatus = CSW_SUCCESS;
          memcpy(csw.data(), &context->csw, sizeof(context->csw));
          context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
          usb_request->response.status = ZX_OK;
          complete_cb->callback(complete_cb->ctx, usb_request);
          break;
        }
        case scsi::Opcode::READ_CAPACITY_16: {
          if (cbw.bCBWLUN == 3) {
            // Push reply
            fbl::Array<unsigned char> reply(
                new unsigned char[sizeof(scsi::ReadCapacity16ParameterData)],
                sizeof(scsi::ReadCapacity16ParameterData));
            scsi::ReadCapacity16ParameterData scsi;
            scsi.block_length_in_bytes = htobe32(kBlockSize);
            scsi.returned_logical_block_address =
                htobe64((976562L * (1 + cbw.bCBWLUN)) + UINT32_MAX);
            memcpy(reply.data(), &scsi, sizeof(scsi));
            context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(reply)));
            // Push CSW
            fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
            context->csw.dCSWDataResidue = 0;
            context->csw.dCSWTag = context->tag++;
            context->csw.bmCSWStatus = CSW_SUCCESS;
            memcpy(csw.data(), &context->csw, sizeof(context->csw));
            context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
          }
          usb_request->response.status = ZX_OK;
          complete_cb->callback(complete_cb->ctx, usb_request);
          break;
        }
        case scsi::Opcode::READ_CAPACITY_10: {
          // Push reply
          fbl::Array<unsigned char> reply(
              new unsigned char[sizeof(scsi::ReadCapacity10ParameterData)],
              sizeof(scsi::ReadCapacity10ParameterData));
          scsi::ReadCapacity10ParameterData scsi;
          scsi.block_length_in_bytes = htobe32(kBlockSize);
          scsi.returned_logical_block_address = htobe32(976562 * (1 + cbw.bCBWLUN));
          if (cbw.bCBWLUN == 3) {
            scsi.returned_logical_block_address = -1;
          }
          memcpy(reply.data(), &scsi, sizeof(scsi));
          context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(reply)));
          // Push CSW
          fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
          context->csw.dCSWDataResidue = 0;
          context->csw.dCSWTag = context->tag++;
          context->csw.bmCSWStatus = CSW_SUCCESS;
          memcpy(csw.data(), &context->csw, sizeof(context->csw));
          context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
          usb_request->response.status = ZX_OK;
          complete_cb->callback(complete_cb->ctx, usb_request);
          break;
        }
        case scsi::Opcode::MODE_SENSE_6: {
          scsi::ModeSense6CDB cmd;
          memcpy(&cmd, cbw.CBWCB, sizeof(cmd));
          // Push reply
          switch (cmd.page_code) {
            case 0x3F: {
              fbl::Array<unsigned char> reply(
                  new unsigned char[sizeof(scsi::ModeSense6ParameterHeader)],
                  sizeof(scsi::ModeSense6ParameterHeader));
              scsi::ModeSense6ParameterHeader scsi = {};
              memcpy(reply.data(), &scsi, sizeof(scsi));
              context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(reply)));
              // Push CSW
              fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)],
                                            sizeof(ums_csw_t));
              context->csw.dCSWDataResidue = 0;
              context->csw.dCSWTag = context->tag++;
              context->csw.bmCSWStatus = CSW_SUCCESS;
              memcpy(csw.data(), &context->csw, sizeof(context->csw));
              context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
              usb_request->response.status = ZX_OK;
              complete_cb->callback(complete_cb->ctx, usb_request);
              break;
            }
            case 0x08: {
              if (context->failure_mode == RejectCacheCbw) {
                usb_request->response.status = ZX_ERR_IO_REFUSED;
                usb_request->response.actual = 0;
                complete_cb->callback(complete_cb->ctx, usb_request);
                return;
              }
              fbl::Array<unsigned char> reply(new unsigned char[20], 20);
              memset(reply.data(), 0, 20);
              reply[6] = 1 << 2;
              if (context->failure_mode == RejectCacheDataStage) {
                complete_cb->callback(complete_cb->ctx, usb_request);
                context->pending_packets.push_back(fbl::MakeRefCounted<Packet>());
                return;
              } else {
                context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(reply)));
              }
              // Push CSW
              fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)],
                                            sizeof(ums_csw_t));
              context->csw.dCSWDataResidue = 0;
              context->csw.dCSWTag = context->tag++;
              context->csw.bmCSWStatus = CSW_SUCCESS;
              memcpy(csw.data(), &context->csw, sizeof(context->csw));
              context->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
              usb_request->response.status = ZX_OK;
              complete_cb->callback(complete_cb->ctx, usb_request);
              break;
            }
            default:
              usb_request->response.status = ZX_OK;
              complete_cb->callback(complete_cb->ctx, usb_request);
          }
          break;
        }
        default:
          ADD_FAILURE("Unexpected SCSI command: %02Xh", cbw.CBWCB[0]);
      }
      break;
    }
    default:
      context->csw.bmCSWStatus = CSW_FAILED;
      usb_request->response.status = ZX_ERR_IO;
      complete_cb->callback(complete_cb->ctx, usb_request);
  }
}
static void CompletionCallback(void* ctx, zx_status_t status, block_op_t* op) {
  Context* context = reinterpret_cast<Context*>(ctx);
  context->status = status;
  context->op = op;
  sync_completion_signal(&context->completion);
}

class UmsTest : public zxtest::Test {
 public:
  UmsTest() : parent_(MockDevice::FakeRootParent()) {}

  void SetUp() override {
    ops_.get_descriptors_length = GetDescriptorLength;
    ops_.get_descriptors = GetDescriptors;
    ops_.get_request_size = GetRequestSize;
    ops_.request_queue = RequestQueue;
    ops_.get_max_transfer_size = GetMaxTransferSize;
    ops_.control_in = ControlIn;
    parent_->AddProtocol(ZX_PROTOCOL_USB, &ops_, &context_);

    context_.pending_write = 0;
    context_.csw.dCSWSignature = htole32(CSW_SIGNATURE);
    context_.csw.bmCSWStatus = CSW_SUCCESS;
    context_.descs = kDescriptors;
    context_.desc_length = sizeof(kDescriptors);
  }

  void TearDown() override {
    ums_->UnbindOp();
    EXPECT_EQ(ZX_OK, ums_->WaitUntilUnbindReplyCalled());
    EXPECT_TRUE(ums_->UnbindReplyCalled());

    ASSERT_FALSE(has_zero_duration_);

    device_async_remove(dev_->zxdev());
    mock_ddk::ReleaseFlaggedDevices(parent_.get());
  }

 protected:
  Context context_;

  MockDevice* ums_;
  ums::UsbMassStorageDevice* dev_;

  void Setup(ErrorInjection inject_failure = NoFault) {
    // Device paramaters for physical (parent) device
    context_.failure_mode = inject_failure;
    // Driver initialization
    auto timer = fbl::MakeRefCounted<FakeTimer>();
    timer->set_timeout_handler([&](sync_completion_t* completion, zx_duration_t duration) {
      if (duration == 0) {
        has_zero_duration_ = true;
      }
      if (duration == ZX_TIME_INFINITE) {
        return sync_completion_wait(completion, duration);
      }
      return ZX_OK;
    });
    auto dev = std::make_unique<ums::UsbMassStorageDevice>(std::move(timer), parent_.get());
    ASSERT_OK(dev->Init(true /* is_test_mode */));
    dev.release();
    EXPECT_EQ(1, parent_->child_count());
    ums_ = parent_->GetLatestChild();
    dev_ = ums_->GetDeviceContext<ums::UsbMassStorageDevice>();
    ums_->InitOp();
    EXPECT_EQ(ZX_OK, ums_->WaitUntilInitReplyCalled());
    EXPECT_TRUE(ums_->InitReplyCalled());

    while (true) {
      if (ums_->child_count() >= 3) {
        break;
      }
      usleep(10);
    }
  }

 private:
  usb_protocol_ops_t ops_;
  std::shared_ptr<zx_device> parent_;
  bool has_zero_duration_ = false;
};

// UMS read test
// This test validates the read functionality on multiple LUNS
// of a USB mass storage device.
TEST_F(UmsTest, TestRead) {
  Setup();
  zx_handle_t vmo;
  uint64_t size;
  zx_vaddr_t mapped;

  // VMO creation to read data into
  EXPECT_EQ(ZX_OK, zx_vmo_create(1000 * 1000 * 10, 0, &vmo), "Failed to create VMO");
  EXPECT_EQ(ZX_OK, zx_vmo_get_size(vmo, &size), "Failed to get size of VMO");
  EXPECT_EQ(ZX_OK, zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ, 0, vmo, 0, size, &mapped),
            "Failed to map VMO");
  // Perform read transactions
  uint32_t i = 0;
  for (auto const& it : ums_->children()) {
    scsi::Disk* block_dev = it->GetDeviceContext<scsi::Disk>();
    ZX_ASSERT(block_dev);
    block_info_t info;
    size_t block_op_size;
    block_dev->BlockImplQuery(&info, &block_op_size);

    auto block_op = std::make_unique<uint8_t[]>(block_op_size);
    block_op_t& op = *reinterpret_cast<block_op_t*>(block_op.get());
    op = {};
    op.command = {.opcode = BLOCK_OPCODE_READ, .flags = 0};
    op.rw.offset_dev = i;
    op.rw.length = (i + 1) * 65534;
    op.rw.offset_vmo = 0;
    op.rw.vmo = vmo;

    block_dev->BlockImplQueue(&op, CompletionCallback, &context_);
    sync_completion_wait(&context_.completion, ZX_TIME_INFINITE);
    sync_completion_reset(&context_.completion);
    scsi::Opcode xfer_type = i == 0   ? scsi::Opcode::READ_10
                             : i == 3 ? scsi::Opcode::READ_16
                                      : scsi::Opcode::READ_12;
    EXPECT_EQ(i, context_.transfer_lun);
    EXPECT_EQ(xfer_type, context_.transfer_type);
    EXPECT_EQ(0, memcmp(reinterpret_cast<void*>(mapped), context_.last_transfer->data.data(),
                        context_.last_transfer->data.size()));
    i++;
  }
}

// This test validates the write functionality on multiple LUNS
// of a USB mass storage device.
TEST_F(UmsTest, TestWrite) {
  Setup();
  zx_handle_t vmo;
  uint64_t size;
  zx_vaddr_t mapped;

  // VMO creation to transfer from
  EXPECT_EQ(ZX_OK, zx_vmo_create(1000 * 1000 * 10, 0, &vmo), "Failed to create VMO");
  EXPECT_EQ(ZX_OK, zx_vmo_get_size(vmo, &size), "Failed to get size of VMO");
  EXPECT_EQ(ZX_OK,
            zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, size,
                        &mapped),
            "Failed to map VMO");
  // Add "entropy" for write operation
  for (size_t i = 0; i < size / sizeof(size_t); i++) {
    reinterpret_cast<size_t*>(mapped)[i] = i;
  }
  // Perform write transactions
  uint32_t i = 0;
  for (auto const& it : ums_->children()) {
    scsi::Disk* block_dev = it->GetDeviceContext<scsi::Disk>();
    ZX_ASSERT(block_dev);
    block_info_t info;
    size_t block_op_size;
    block_dev->BlockImplQuery(&info, &block_op_size);

    auto block_op = std::make_unique<uint8_t[]>(block_op_size);
    block_op_t& op = *reinterpret_cast<block_op_t*>(block_op.get());
    op = {};
    op.command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0};
    op.rw.offset_dev = i;
    op.rw.length = (i + 1) * 65534;
    op.rw.offset_vmo = 0;
    op.rw.vmo = vmo;

    block_dev->BlockImplQueue(&op, CompletionCallback, &context_);
    sync_completion_wait(&context_.completion, ZX_TIME_INFINITE);
    sync_completion_reset(&context_.completion);
    scsi::Opcode xfer_type = i == 0   ? scsi::Opcode::WRITE_10
                             : i == 3 ? scsi::Opcode::WRITE_16
                                      : scsi::Opcode::WRITE_12;
    EXPECT_EQ(i, context_.transfer_lun);
    EXPECT_EQ(xfer_type, context_.transfer_type);
    EXPECT_EQ(0, memcmp(reinterpret_cast<void*>(mapped), context_.last_transfer->data.data(),
                        op.rw.length * kBlockSize));
    i++;
  }
}

// This test validates the flush functionality on multiple LUNS
// of a USB mass storage device.
TEST_F(UmsTest, TestFlush) {
  Setup();

  // Perform flush transactions
  uint32_t i = 0;
  for (auto const& it : ums_->children()) {
    scsi::Disk* block_dev = it->GetDeviceContext<scsi::Disk>();
    ZX_ASSERT(block_dev);
    block_info_t info;
    size_t block_op_size;
    block_dev->BlockImplQuery(&info, &block_op_size);

    auto block_op = std::make_unique<uint8_t[]>(block_op_size);
    block_op_t& op = *reinterpret_cast<block_op_t*>(block_op.get());
    op = {};
    op.command = {.opcode = BLOCK_OPCODE_FLUSH, .flags = 0};

    block_dev->BlockImplQueue(&op, CompletionCallback, &context_);
    sync_completion_wait(&context_.completion, ZX_TIME_INFINITE);
    sync_completion_reset(&context_.completion);
    scsi::Opcode xfer_type = scsi::Opcode::SYNCHRONIZE_CACHE_10;
    EXPECT_EQ(i, context_.transfer_lun);
    EXPECT_EQ(xfer_type, context_.transfer_type);
    i++;
  }
}

TEST_F(UmsTest, CbwStallDoesNotFreezeDriver) { Setup(RejectCacheCbw); }

TEST_F(UmsTest, DataStageStallDoesNotFreezeDriver) { Setup(RejectCacheDataStage); }

}  // namespace
