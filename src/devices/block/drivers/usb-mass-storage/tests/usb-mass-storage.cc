// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../usb-mass-storage.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <variant>

#include <fbl/array.h>
#include <fbl/intrusive_double_list.h>
#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

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

class UsbBanjoServer : public ddk::UsbProtocol<UsbBanjoServer> {
 public:
  void SetContext(Context* context) { context_ = context; }

  compat::DeviceServer::BanjoConfig GetBanjoConfig() {
    compat::DeviceServer::BanjoConfig config{ZX_PROTOCOL_USB};
    config.callbacks[ZX_PROTOCOL_USB] = banjo_server_.callback();
    return config;
  }

  size_t UsbGetDescriptorsLength() { return context_->desc_length; }

  void UsbGetDescriptors(uint8_t* buffer, size_t size, size_t* outsize) {
    *outsize = context_->desc_length > size ? size : context_->desc_length;
    memcpy(buffer, context_->descs, *outsize);
  }

  zx_status_t UsbControlIn(uint8_t request_type, uint8_t request, uint16_t value, uint16_t index,
                           int64_t timeout, uint8_t* out_read_buffer, size_t read_size,
                           size_t* out_read_actual) {
    switch (request) {
      case USB_REQ_GET_MAX_LUN: {
        if (!read_size) {
          *out_read_actual = 0;
          return ZX_OK;
        }
        *reinterpret_cast<unsigned char*>(out_read_buffer) = 1;  // Max lun number
        *out_read_actual = 1;
        return ZX_OK;
      }
      default:
        return ZX_ERR_IO_REFUSED;
    }
  }

  size_t UsbGetMaxTransferSize(uint8_t ep) {
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
  size_t UsbGetRequestSize() { return sizeof(usb_request_t); }

  void UsbRequestQueue(usb_request_t* usb_request,
                       const usb_request_complete_callback_t* complete_cb) {
    if (context_->pending_write) {
      void* data;
      usb_request_mmap(usb_request, &data);
      memcpy(context_->last_transfer->data.data(), data, context_->pending_write);
      context_->pending_write = 0;
      usb_request->response.status = ZX_OK;
      complete_cb->callback(complete_cb->ctx, usb_request);
      return;
    }
    if ((usb_request->header.ep_address & USB_ENDPOINT_DIR_MASK) == USB_ENDPOINT_IN) {
      if (context_->pending_packets.begin() == context_->pending_packets.end()) {
        usb_request->response.status = ZX_OK;
        complete_cb->callback(complete_cb->ctx, usb_request);
      } else {
        auto packet = context_->pending_packets.pop_front();
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
          if ((context_->transfer_offset == cbw.bCBWLUN) &&
              (context_->transfer_blocks == (cbw.bCBWLUN + 1U) * 65534U)) {
            auto opcode = static_cast<scsi::Opcode>(cbw.CBWCB[0]);
            size_t transfer_length = context_->transfer_blocks * kBlockSize;
            fbl::Array<unsigned char> transfer(new unsigned char[transfer_length], transfer_length);
            context_->last_transfer = fbl::MakeRefCounted<Packet>(std::move(transfer));
            context_->transfer_lun = cbw.bCBWLUN;
            if ((opcode == scsi::Opcode::READ_10) || (opcode == scsi::Opcode::READ_12) ||
                (opcode == scsi::Opcode::READ_16)) {
              // Push reply
              context_->pending_packets.push_back(context_->last_transfer);
            } else {
              if ((opcode == scsi::Opcode::WRITE_10) || (opcode == scsi::Opcode::WRITE_12) ||
                  (opcode == scsi::Opcode::WRITE_16)) {
                context_->pending_write = transfer_length;
              }
            }
            // Push CSW
            fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
            context_->csw.dCSWDataResidue = 0;
            context_->csw.dCSWTag = context_->tag++;
            context_->csw.bmCSWStatus = CSW_SUCCESS;
            memcpy(csw.data(), &context_->csw, sizeof(context_->csw));
            context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
          }
        };
        auto opcode = static_cast<scsi::Opcode>(cbw.CBWCB[0]);
        switch (opcode) {
          case scsi::Opcode::WRITE_16:
            __FALLTHROUGH;
          case scsi::Opcode::READ_16: {
            scsi::Read16CDB cmd;  // struct-wise equivalent to scsi::Write16CDB here.
            memcpy(&cmd, cbw.CBWCB, sizeof(cmd));
            context_->transfer_blocks = be32toh(cmd.transfer_length);
            context_->transfer_offset = be64toh(cmd.logical_block_address);
            context_->transfer_type = opcode;
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
            context_->transfer_blocks = be32toh(cmd.transfer_length);
            context_->transfer_offset = be32toh(cmd.logical_block_address);
            context_->transfer_type = opcode;
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
            context_->transfer_blocks = be16toh(cmd.transfer_length);
            context_->transfer_offset = be32toh(cmd.logical_block_address);
            context_->transfer_type = opcode;
            DataTransfer();
            usb_request->response.status = ZX_OK;
            complete_cb->callback(complete_cb->ctx, usb_request);
            break;
          }
          case scsi::Opcode::SYNCHRONIZE_CACHE_10: {
            // Push CSW
            fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
            context_->csw.dCSWDataResidue = 0;
            context_->csw.dCSWTag = context_->tag++;
            context_->csw.bmCSWStatus = CSW_SUCCESS;
            context_->transfer_lun = cbw.bCBWLUN;
            context_->transfer_type = opcode;
            memcpy(csw.data(), &context_->csw, sizeof(context_->csw));
            context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
            usb_request->response.status = ZX_OK;
            complete_cb->callback(complete_cb->ctx, usb_request);
            break;
          }
          case scsi::Opcode::UNMAP: {
            // Push CSW
            fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
            context_->csw.dCSWDataResidue = 0;
            context_->csw.dCSWTag = context_->tag++;
            context_->csw.bmCSWStatus = CSW_SUCCESS;
            context_->transfer_lun = cbw.bCBWLUN;
            context_->transfer_type = opcode;
            memcpy(csw.data(), &context_->csw, sizeof(context_->csw));
            context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
            usb_request->response.status = ZX_OK;
            complete_cb->callback(complete_cb->ctx, usb_request);
            break;
          }
          case scsi::Opcode::INQUIRY: {
            scsi::InquiryCDB cmd;
            memcpy(&cmd, cbw.CBWCB, sizeof(cmd));

            // Push reply
            fbl::Array<unsigned char> reply(new unsigned char[UMS_INQUIRY_TRANSFER_LENGTH],
                                            UMS_INQUIRY_TRANSFER_LENGTH);
            if (!cmd.reserved_and_evpd) {
              memset(reply.data(), 0, UMS_INQUIRY_TRANSFER_LENGTH);
              reply[0] = 0;     // Peripheral Device Type: Direct access block device
              reply[1] = 0x80;  // Removable
              reply[2] = 6;     // Version SPC-4
              reply[3] = 0x12;  // Response Data Format
              memcpy(reply.data() + 8, "Google  ", 8);
              memcpy(reply.data() + 16, "Zircon UMS      ", 16);
              memcpy(reply.data() + 32, "1.00", 4);
            } else {
              if (cmd.page_code == scsi::InquiryCDB::kPageListVpdPageCode) {
                auto vpd_page_list = reinterpret_cast<scsi::VPDPageList*>(reply.data());
                vpd_page_list->peripheral_qualifier_device_type = 0;
                vpd_page_list->page_code = 0x00;
                vpd_page_list->page_length = 2;
                vpd_page_list->pages[0] = scsi::InquiryCDB::kBlockLimitsVpdPageCode;
                vpd_page_list->pages[1] = scsi::InquiryCDB::kLogicalBlockProvisioningVpdPageCode;
              } else if (cmd.page_code == scsi::InquiryCDB::kBlockLimitsVpdPageCode) {
                auto block_limits = reinterpret_cast<scsi::VPDBlockLimits*>(reply.data());
                block_limits->peripheral_qualifier_device_type = 0;
                block_limits->page_code = scsi::InquiryCDB::kBlockLimitsVpdPageCode;
                block_limits->maximum_unmap_lba_count = htobe32(UINT32_MAX);
              } else if (cmd.page_code == scsi::InquiryCDB::kLogicalBlockProvisioningVpdPageCode) {
                auto provisioning =
                    reinterpret_cast<scsi::VPDLogicalBlockProvisioning*>(reply.data());
                provisioning->peripheral_qualifier_device_type = 0;
                provisioning->page_code = scsi::InquiryCDB::kLogicalBlockProvisioningVpdPageCode;
                provisioning->set_lbpu(true);
                provisioning->set_provisioning_type(0x02);  // The logical unit is thin provisioned
              } else {
                usb_request->response.status = ZX_ERR_NOT_SUPPORTED;
                usb_request->response.actual = 0;
                complete_cb->callback(complete_cb->ctx, usb_request);
                return;
              }
            }
            context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(reply)));
            // Push CSW
            fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
            context_->csw.dCSWDataResidue = 0;
            context_->csw.dCSWTag = context_->tag++;
            context_->csw.bmCSWStatus = CSW_SUCCESS;
            memcpy(csw.data(), &context_->csw, sizeof(context_->csw));
            context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));

            usb_request->response.status = ZX_OK;
            complete_cb->callback(complete_cb->ctx, usb_request);
            break;
          }
          case scsi::Opcode::TEST_UNIT_READY: {
            // Push CSW
            fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
            context_->csw.dCSWDataResidue = 0;
            context_->csw.dCSWTag = context_->tag++;
            context_->csw.bmCSWStatus = CSW_SUCCESS;
            memcpy(csw.data(), &context_->csw, sizeof(context_->csw));
            context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
            usb_request->response.status = ZX_OK;
            complete_cb->callback(complete_cb->ctx, usb_request);
            break;
          }
          case scsi::Opcode::READ_CAPACITY_16: {
            if (cbw.bCBWLUN == 1) {
              // Push reply
              fbl::Array<unsigned char> reply(
                  new unsigned char[sizeof(scsi::ReadCapacity16ParameterData)],
                  sizeof(scsi::ReadCapacity16ParameterData));
              scsi::ReadCapacity16ParameterData scsi;
              scsi.block_length_in_bytes = htobe32(kBlockSize);
              scsi.returned_logical_block_address =
                  htobe64((976562L * (1 + cbw.bCBWLUN)) + UINT32_MAX);
              memcpy(reply.data(), &scsi, sizeof(scsi));
              context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(reply)));
              // Push CSW
              fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)],
                                            sizeof(ums_csw_t));
              context_->csw.dCSWDataResidue = 0;
              context_->csw.dCSWTag = context_->tag++;
              context_->csw.bmCSWStatus = CSW_SUCCESS;
              memcpy(csw.data(), &context_->csw, sizeof(context_->csw));
              context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
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
            if (cbw.bCBWLUN == 1) {
              scsi.returned_logical_block_address = -1;
            }
            memcpy(reply.data(), &scsi, sizeof(scsi));
            context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(reply)));
            // Push CSW
            fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
            context_->csw.dCSWDataResidue = 0;
            context_->csw.dCSWTag = context_->tag++;
            context_->csw.bmCSWStatus = CSW_SUCCESS;
            memcpy(csw.data(), &context_->csw, sizeof(context_->csw));
            context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
            usb_request->response.status = ZX_OK;
            complete_cb->callback(complete_cb->ctx, usb_request);
            break;
          }
          case scsi::Opcode::MODE_SENSE_6: {
            scsi::ModeSense6CDB cmd;
            memcpy(&cmd, cbw.CBWCB, sizeof(cmd));
            // Push reply
            switch (cmd.page_code()) {
              case scsi::PageCode::kAllPageCode: {
                fbl::Array<unsigned char> reply(
                    new unsigned char[sizeof(scsi::Mode6ParameterHeader)],
                    sizeof(scsi::Mode6ParameterHeader));
                scsi::Mode6ParameterHeader mode_page = {};
                mode_page.set_dpo_fua_available(true);
                mode_page.set_write_protected(false);
                memcpy(reply.data(), &mode_page, sizeof(mode_page));
                context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(reply)));
                // Push CSW
                fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)],
                                              sizeof(ums_csw_t));
                context_->csw.dCSWDataResidue = 0;
                context_->csw.dCSWTag = context_->tag++;
                context_->csw.bmCSWStatus = CSW_SUCCESS;
                memcpy(csw.data(), &context_->csw, sizeof(context_->csw));
                context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
                usb_request->response.status = ZX_OK;
                complete_cb->callback(complete_cb->ctx, usb_request);
                break;
              }
              case scsi::PageCode::kCachingPageCode: {
                if (context_->failure_mode == RejectCacheCbw) {
                  usb_request->response.status = ZX_ERR_IO_REFUSED;
                  usb_request->response.actual = 0;
                  complete_cb->callback(complete_cb->ctx, usb_request);
                  return;
                }
                if (context_->failure_mode == RejectCacheDataStage) {
                  complete_cb->callback(complete_cb->ctx, usb_request);
                  context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>());
                  return;
                } else {
                  size_t mode_page_size =
                      sizeof(scsi::Mode6ParameterHeader) + sizeof(scsi::CachingModePage);
                  fbl::Array<unsigned char> reply(new unsigned char[mode_page_size],
                                                  mode_page_size);
                  scsi::CachingModePage mode_page = {};
                  mode_page.set_page_code(static_cast<uint8_t>(scsi::PageCode::kCachingPageCode));
                  mode_page.set_write_cache_enabled(true);
                  memset(reply.data(), 0, mode_page_size);
                  memcpy(reply.data() + sizeof(scsi::Mode6ParameterHeader), &mode_page,
                         sizeof(mode_page));
                  context_->pending_packets.push_back(
                      fbl::MakeRefCounted<Packet>(std::move(reply)));
                }
                // Push CSW
                fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)],
                                              sizeof(ums_csw_t));
                context_->csw.dCSWDataResidue = 0;
                context_->csw.dCSWTag = context_->tag++;
                context_->csw.bmCSWStatus = CSW_SUCCESS;
                memcpy(csw.data(), &context_->csw, sizeof(context_->csw));
                context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
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
          case scsi::Opcode::REQUEST_SENSE: {
            scsi::RequestSenseCDB cmd;
            memcpy(&cmd, cbw.CBWCB, sizeof(cmd));
            // Push reply
            fbl::Array<unsigned char> reply(new unsigned char[cmd.allocation_length],
                                            cmd.allocation_length);
            scsi::FixedFormatSenseDataHeader sense_data;
            sense_data.set_response_code(0x70);  // Current information
            sense_data.set_valid(0);
            sense_data.set_sense_key(scsi::SenseKey::NO_SENSE);
            memcpy(reply.data(), &sense_data, sizeof(sense_data));
            context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(reply)));
            // Push CSW
            fbl::Array<unsigned char> csw(new unsigned char[sizeof(ums_csw_t)], sizeof(ums_csw_t));
            context_->csw.dCSWDataResidue = 0;
            context_->csw.dCSWTag = context_->tag++;
            context_->csw.bmCSWStatus = CSW_SUCCESS;
            memcpy(csw.data(), &context_->csw, sizeof(context_->csw));
            context_->pending_packets.push_back(fbl::MakeRefCounted<Packet>(std::move(csw)));
            usb_request->response.status = ZX_OK;
            complete_cb->callback(complete_cb->ctx, usb_request);
            break;
          }
          default:
            // Unexpected SCSI command
            ADD_FAILURE();
        }
        break;
      }
      default:
        context_->csw.bmCSWStatus = CSW_FAILED;
        usb_request->response.status = ZX_ERR_IO;
        complete_cb->callback(complete_cb->ctx, usb_request);
    }
  }

  // Unimplemented methods.
  zx_status_t UsbControlOut(uint8_t request_type, uint8_t request, uint16_t value, uint16_t index,
                            zx_time_t timeout, const uint8_t* write_buffer, size_t write_size) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  usb_speed_t UsbGetSpeed() {
    ZX_ASSERT_MSG(0, "Unimplemented.");
    return 0;
  }
  zx_status_t UsbSetInterface(uint8_t interface_number, uint8_t alt_setting) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  uint8_t UsbGetConfiguration() {
    ZX_ASSERT_MSG(0, "Unimplemented.");
    return 0;
  }
  zx_status_t UsbSetConfiguration(uint8_t configuration) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t UsbEnableEndpoint(const usb_endpoint_descriptor_t* ep_desc,
                                const usb_ss_ep_comp_descriptor_t* ss_com_desc, bool enable) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t UsbResetEndpoint(uint8_t ep_address) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t UsbResetDevice() { return ZX_ERR_NOT_SUPPORTED; }
  uint32_t UsbGetDeviceId() {
    ZX_ASSERT_MSG(0, "Unimplemented.");
    return 0;
  }
  void UsbGetDeviceDescriptor(usb_device_descriptor_t* out_desc) {
    ZX_ASSERT_MSG(0, "Unimplemented.");
  }
  zx_status_t UsbGetConfigurationDescriptorLength(uint8_t configuration, uint64_t* out_length) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t UsbGetConfigurationDescriptor(uint8_t configuration, uint8_t* out_desc_buffer,
                                            size_t desc_size, size_t* out_desc_actual) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t UsbGetStringDescriptor(uint8_t desc_id, uint16_t lang_id, uint16_t* out_lang_id,
                                     uint8_t* out_string_buffer, size_t string_size,
                                     size_t* out_string_actual) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t UsbCancelAll(uint8_t ep_address) { return ZX_ERR_NOT_SUPPORTED; }
  uint64_t UsbGetCurrentFrame() {
    ZX_ASSERT_MSG(0, "Unimplemented.");
    return 0;
  }

 private:
  Context* context_;
  compat::BanjoServer banjo_server_{ZX_PROTOCOL_USB, this, &usb_protocol_ops_};
};

static void CompletionCallback(void* ctx, zx_status_t status, block_op_t* op) {
  Context* context = reinterpret_cast<Context*>(ctx);
  context->status = status;
  context->op = op;
  sync_completion_signal(&context->completion);
}

class TestUsbMassStorageDevice : public ums::UsbMassStorageDevice {
 public:
  TestUsbMassStorageDevice(fdf::DriverStartArgs start_args,
                           fdf::UnownedSynchronizedDispatcher dispatcher)
      : UsbMassStorageDevice(std::move(start_args), std::move(dispatcher)) {
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
    set_waiter(std::move(timer));
  }

  void PrepareStop(fdf::PrepareStopCompleter completer) override {
    ASSERT_FALSE(has_zero_duration_);
    UsbMassStorageDevice::PrepareStop(std::move(completer));
  }

 private:
  bool has_zero_duration_ = false;
};

class Environment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    usb_banjo_server_.SetContext(&context_);
    device_server_.Initialize(component::kDefaultInstance, std::nullopt,
                              usb_banjo_server_.GetBanjoConfig());
    return zx::make_result(
        device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs));
  }

  Context& context() { return context_; }

 private:
  Context context_;
  UsbBanjoServer usb_banjo_server_;
  compat::DeviceServer device_server_;
};

class TestConfig final {
 public:
  using DriverType = TestUsbMassStorageDevice;
  using EnvironmentType = Environment;
};

class UmsTest : public ::testing::Test {
 public:
  void SetUp() override {
    driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
      env.context().pending_write = 0;
      env.context().csw.dCSWSignature = htole32(CSW_SIGNATURE);
      env.context().csw.bmCSWStatus = CSW_SUCCESS;
      env.context().descs = kDescriptors;
      env.context().desc_length = sizeof(kDescriptors);
    });
  }

  fdf_testing::ForegroundDriverTest<TestConfig>& driver_test() { return driver_test_; }

  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_OK(result);
  }

 protected:
  fdf_testing::ForegroundDriverTest<TestConfig> driver_test_;

  void StartDriver(ErrorInjection inject_failure = NoFault) {
    driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
      // Device parameters for physical (parent) device
      env.context().failure_mode = inject_failure;
    });

    zx::result<> result = driver_test().StartDriver();
    ASSERT_OK(result);

    ASSERT_GE(driver_test().driver()->block_devs().size(), size_t{2});
  }
};

// UMS read test
// This test validates the read functionality on multiple LUNS of a USB mass storage device.
TEST_F(UmsTest, TestRead) {
  StartDriver();
  zx_handle_t vmo;
  uint64_t size;
  zx_vaddr_t mapped;

  // VMO creation to read data into
  ZX_ASSERT_MSG(ZX_OK == zx_vmo_create(1000 * 1000 * 10, 0, &vmo), "Failed to create VMO");
  ZX_ASSERT_MSG(ZX_OK == zx_vmo_get_size(vmo, &size), "Failed to get size of VMO");
  ZX_ASSERT_MSG(
      ZX_OK == zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ, 0, vmo, 0, size, &mapped),
      "Failed to map VMO");
  // Perform read transactions
  uint32_t lun = 0;
  for (auto const& block_dev : driver_test().driver()->block_devs()) {
    ZX_ASSERT(block_dev);
    block_info_t info;
    size_t block_op_size;
    block_dev->BlockImplQuery(&info, &block_op_size);

    auto block_op = std::make_unique<uint8_t[]>(block_op_size);
    block_op_t& op = *reinterpret_cast<block_op_t*>(block_op.get());
    op = {};
    op.command = {.opcode = BLOCK_OPCODE_READ, .flags = 0};
    op.rw.offset_dev = lun;
    op.rw.length = (lun + 1) * 65534;
    op.rw.offset_vmo = 0;
    op.rw.vmo = vmo;

    driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
      block_dev->BlockImplQueue(&op, CompletionCallback, &env.context());
      sync_completion_wait(&env.context().completion, ZX_TIME_INFINITE);
      sync_completion_reset(&env.context().completion);
      EXPECT_EQ(lun, env.context().transfer_lun);
      const scsi::Opcode xfer_type = lun == 1 ? scsi::Opcode::READ_16 : scsi::Opcode::READ_10;
      EXPECT_EQ(xfer_type, env.context().transfer_type);
      EXPECT_EQ(0, memcmp(reinterpret_cast<void*>(mapped), env.context().last_transfer->data.data(),
                          env.context().last_transfer->data.size()));
    });
    ++lun;
  }
}

// This test validates the write functionality on multiple LUNS of a USB mass storage device.
TEST_F(UmsTest, TestWrite) {
  StartDriver();
  zx_handle_t vmo;
  uint64_t size;
  zx_vaddr_t mapped;

  // VMO creation to transfer from
  ZX_ASSERT_MSG(ZX_OK == zx_vmo_create(1000 * 1000 * 10, 0, &vmo), "Failed to create VMO");
  ZX_ASSERT_MSG(ZX_OK == zx_vmo_get_size(vmo, &size), "Failed to get size of VMO");
  ZX_ASSERT_MSG(ZX_OK == zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0,
                                     vmo, 0, size, &mapped),
                "Failed to map VMO");
  // Add "entropy" for write operation
  for (size_t i = 0; i < size / sizeof(size_t); i++) {
    reinterpret_cast<size_t*>(mapped)[i] = i;
  }
  // Perform write transactions
  uint32_t lun = 0;
  for (auto const& block_dev : driver_test().driver()->block_devs()) {
    ZX_ASSERT(block_dev);
    block_info_t info;
    size_t block_op_size;
    block_dev->BlockImplQuery(&info, &block_op_size);

    auto block_op = std::make_unique<uint8_t[]>(block_op_size);
    block_op_t& op = *reinterpret_cast<block_op_t*>(block_op.get());
    op = {};
    op.command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0};
    op.rw.offset_dev = lun;
    op.rw.length = (lun + 1) * 65534;
    op.rw.offset_vmo = 0;
    op.rw.vmo = vmo;

    driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
      block_dev->BlockImplQueue(&op, CompletionCallback, &env.context());
      sync_completion_wait(&env.context().completion, ZX_TIME_INFINITE);
      sync_completion_reset(&env.context().completion);
      EXPECT_EQ(lun, env.context().transfer_lun);
      const scsi::Opcode xfer_type = lun == 1 ? scsi::Opcode::WRITE_16 : scsi::Opcode::WRITE_10;
      EXPECT_EQ(xfer_type, env.context().transfer_type);
      EXPECT_EQ(0, memcmp(reinterpret_cast<void*>(mapped), env.context().last_transfer->data.data(),
                          op.rw.length * kBlockSize));
    });
    ++lun;
  }
}

// This test validates the flush functionality on multiple LUNS of a USB mass storage device.
TEST_F(UmsTest, TestFlush) {
  StartDriver();

  // Perform flush transactions
  uint32_t lun = 0;
  for (auto const& block_dev : driver_test().driver()->block_devs()) {
    ZX_ASSERT(block_dev);
    block_info_t info;
    size_t block_op_size;
    block_dev->BlockImplQuery(&info, &block_op_size);

    auto block_op = std::make_unique<uint8_t[]>(block_op_size);
    block_op_t& op = *reinterpret_cast<block_op_t*>(block_op.get());
    op = {};
    op.command = {.opcode = BLOCK_OPCODE_FLUSH, .flags = 0};

    driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
      block_dev->BlockImplQueue(&op, CompletionCallback, &env.context());
      sync_completion_wait(&env.context().completion, ZX_TIME_INFINITE);
      sync_completion_reset(&env.context().completion);
      EXPECT_EQ(lun, env.context().transfer_lun);
      EXPECT_EQ(scsi::Opcode::SYNCHRONIZE_CACHE_10, env.context().transfer_type);
    });
    ++lun;
  }
}

// This test validates the trim functionality on multiple LUNS of a USB mass storage device.
TEST_F(UmsTest, TestTrim) {
  StartDriver();

  // Perform trim transactions
  uint32_t lun = 0;
  for (auto const& block_dev : driver_test().driver()->block_devs()) {
    ZX_ASSERT(block_dev);
    block_info_t info;
    size_t block_op_size;
    block_dev->BlockImplQuery(&info, &block_op_size);

    auto block_op = std::make_unique<uint8_t[]>(block_op_size);
    block_op_t& op = *reinterpret_cast<block_op_t*>(block_op.get());
    op = {};
    op.command = {.opcode = BLOCK_OPCODE_TRIM, .flags = 0};
    op.trim.offset_dev = 0;
    op.trim.length = 1;

    driver_test().RunInEnvironmentTypeContext([&](Environment& env) {
      block_dev->BlockImplQueue(&op, CompletionCallback, &env.context());
      sync_completion_wait(&env.context().completion, ZX_TIME_INFINITE);
      sync_completion_reset(&env.context().completion);
      EXPECT_EQ(lun, env.context().transfer_lun);
      EXPECT_EQ(scsi::Opcode::UNMAP, env.context().transfer_type);
    });
    ++lun;
  }
}

TEST_F(UmsTest, CbwStallDoesNotFreezeDriver) { StartDriver(RejectCacheCbw); }

TEST_F(UmsTest, DataStageStallDoesNotFreezeDriver) { StartDriver(RejectCacheDataStage); }

FUCHSIA_DRIVER_EXPORT(TestUsbMassStorageDevice);

}  // namespace
