// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_CONTROLLER_H_
#define SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_CONTROLLER_H_

#include <lib/ddk/debug.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <stdint.h>
#include <sys/uio.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <optional>

#include <hwreg/bitfields.h>

namespace scsi {

enum class Opcode : uint8_t {
  TEST_UNIT_READY = 0x00,
  REQUEST_SENSE = 0x03,
  INQUIRY = 0x12,
  MODE_SELECT_6 = 0x15,
  MODE_SENSE_6 = 0x1A,
  START_STOP_UNIT = 0x1B,
  TOGGLE_REMOVABLE = 0x1E,
  READ_FORMAT_CAPACITIES = 0x23,
  READ_CAPACITY_10 = 0x25,
  READ_10 = 0x28,
  WRITE_10 = 0x2A,
  SYNCHRONIZE_CACHE_10 = 0x35,
  MODE_SELECT_10 = 0x55,
  MODE_SENSE_10 = 0x5A,
  READ_16 = 0x88,
  WRITE_16 = 0x8A,
  READ_CAPACITY_16 = 0x9E,
  REPORT_LUNS = 0xA0,
  READ_12 = 0xA8,
  WRITE_12 = 0xAA,
};

// SCSI command structures (CDBs)

struct TestUnitReadyCDB {
  Opcode opcode;
  uint8_t reserved[4];
  uint8_t control;
} __PACKED;

static_assert(sizeof(TestUnitReadyCDB) == 6, "TestUnitReady CDB must be 6 bytes");

struct InquiryCDB {
  static constexpr uint8_t kBlockLimitsVpdPageCode = 0xB0;

  Opcode opcode;
  // reserved_and_evpd(0) is 'Enable Vital Product Data'
  uint8_t reserved_and_evpd;
  uint8_t page_code;
  // allocation_length is in network-byte-order.
  uint16_t allocation_length;
  uint8_t control;
} __PACKED;

static_assert(sizeof(InquiryCDB) == 6, "Inquiry CDB must be 6 bytes");

// Standard INQUIRY Data Header.
struct InquiryData {
  // Peripheral Device Type Header and qualifier.
  uint8_t peripheral_device_type;
  // removable(7) is the 'Removable' bit.
  uint8_t removable;
  uint8_t version;
  // response_data_format_and_control(3 downto 0) is Response Data Format
  // response_data_format_and_control(4) is HiSup
  // response_data_format_and_control(5) is NormACA
  uint8_t response_data_format_and_control;
  uint8_t additional_length;
  // Various control bits, unused currently.
  uint8_t control[3];
  char t10_vendor_id[8];
  char product_id[16];
  char product_revision[4];
  // Vendor specific after 36 bytes.

  DEF_SUBBIT(removable, 7, removable_media);
} __PACKED;

static_assert(sizeof(InquiryData) == 36, "Inquiry data must be 36 bytes");
static_assert(offsetof(InquiryData, t10_vendor_id) == 8, "T10 Vendor ID is at offset 8");
static_assert(offsetof(InquiryData, product_id) == 16, "Product ID is at offset 16");

struct VPDBlockLimits {
  uint8_t peripheral_qualifier_device_type;
  uint8_t page_code;
  uint8_t reserved1;
  uint8_t page_length;
  uint8_t reserved2[2];
  uint16_t optimal_xfer_granularity;
  uint32_t max_xfer_length_blocks;
  uint32_t optimal_xfer_length;
} __PACKED;

static_assert(sizeof(VPDBlockLimits) == 16, "BlockLimits Page must be 16 bytes");

struct VPDPageList {
  uint8_t peripheral_qualifier_device_type;
  uint8_t page_code;
  uint8_t reserved;
  uint8_t page_length;
  uint8_t pages[255];
};

struct RequestSenseCDB {
  Opcode opcode;
  uint8_t desc;
  uint16_t reserved;
  uint8_t allocation_length;
  uint8_t control;
} __PACKED;

static_assert(sizeof(RequestSenseCDB) == 6, "Request Sense CDB must be 6 bytes");

struct ModeSense6CDB {
  static constexpr uint8_t kCachingPageCode = 0x08;
  static constexpr uint8_t kAllPageCode = 0x3F;

  Opcode opcode;
  // If disable_block_descriptors(3) is '1', device will not return Block Descriptors.
  uint8_t disable_block_descriptors;
  // page_code(7 downto 6) is 'page control'. Should be 00h for current devices.
  uint8_t page_code;
  uint8_t subpage_code;
  uint8_t allocation_length;
  uint8_t control;
} __PACKED;

static_assert(sizeof(ModeSense6CDB) == 6, "Mode Sense 6 CDB must be 6 bytes");

struct ModeSense6ParameterHeader {
  uint8_t mode_data_length;
  // 00h is 'Direct Access Block Device'
  uint8_t medium_type;
  // For Direct Access Block Devices:
  // device_specific_parameter(7) is write-protected bit
  // device_specific_parameter(4) is disable page out/force unit access available
  uint8_t device_specific_parameter;
  uint8_t block_descriptor_length;

  DEF_SUBBIT(device_specific_parameter, 7, write_protected);
} __PACKED;

static_assert(sizeof(ModeSense6ParameterHeader) == 4, "Mode Sense 6 parameters must be 4 bytes");

struct CachingModePage {
  ModeSense6ParameterHeader header;
  uint8_t page_code;
  uint8_t page_length;
  // control_bits (2) is write_cache_enabled.
  uint8_t control_bits;
  uint8_t retention_priorities;
  uint16_t prefetch_transfer_length;
  uint16_t min_prefetch;
  uint16_t max_prefetch;
  uint16_t max_prefetch_ceiling;
  uint8_t control_bits_1;
  uint8_t num_cache_segments;
  uint16_t cache_segment_size;
  uint8_t reserved;
  uint8_t obsolete[3];

  DEF_SUBBIT(control_bits, 2, write_cache_enabled);
} __PACKED;

static_assert(sizeof(CachingModePage) == 24, "Caching Mode Page must be 24 bytes");

struct ReadCapacity10CDB {
  Opcode opcode;
  uint8_t reserved0;
  uint32_t obsolete;
  uint16_t reserved1;
  uint8_t reserved2;
  uint8_t control;
} __PACKED;

static_assert(sizeof(ReadCapacity10CDB) == 10, "Read Capacity 10 CDB must be 10 bytes");

struct ReadCapacity10ParameterData {
  uint32_t returned_logical_block_address;
  uint32_t block_length_in_bytes;
} __PACKED;

static_assert(sizeof(ReadCapacity10ParameterData) == 8, "Read Capacity 10 Params are 8 bytes");

struct ReadCapacity16CDB {
  Opcode opcode;
  uint8_t service_action;
  uint64_t obsolete;
  uint32_t allocation_length;
  uint8_t reserved;
  uint8_t control;
} __PACKED;

static_assert(sizeof(ReadCapacity16CDB) == 16, "Read Capacity 16 CDB must be 16 bytes");

struct ReadCapacity16ParameterData {
  uint64_t returned_logical_block_address;
  uint32_t block_length_in_bytes;
  uint8_t prot_info;
  uint8_t logical_blocks_exponent_info;
  uint16_t lowest_aligned_logical_block;
  uint8_t reserved[16];
} __PACKED;

static_assert(sizeof(ReadCapacity16ParameterData) == 32, "Read Capacity 16 Params are 32 bytes");

struct ReportLunsCDB {
  Opcode opcode;
  uint8_t reserved0;
  uint8_t select_report;
  uint8_t reserved1[3];
  uint32_t allocation_length;
  uint8_t reserved2;
  uint8_t control;
} __PACKED;

static_assert(sizeof(ReportLunsCDB) == 12, "Report LUNs CDB must be 12 bytes");

struct ReportLunsParameterDataHeader {
  uint32_t lun_list_length;
  uint32_t reserved;
  uint64_t lun;  // Need space for at least one LUN.
                 // Followed by 8-byte LUN structures.
} __PACKED;

static_assert(sizeof(ReportLunsParameterDataHeader) == 16, "Report LUNs Header must be 16 bytes");

struct Read10CDB {
  Opcode opcode;
  // dpo_fua(4) - DPO - Disable Page Out
  // dpo_fua(3) - FUA - Force Unit Access
  uint8_t dpo_fua;
  // Network byte order
  uint32_t logical_block_address;
  uint8_t group_number;
  uint16_t transfer_length;
  uint8_t control;

  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
} __PACKED;

static_assert(sizeof(Read10CDB) == 10, "Read 10 CDB must be 10 bytes");

struct Read12CDB {
  Opcode opcode;
  // dpo_fua(4) - DPO - Disable Page Out
  // dpo_fua(3) - FUA - Force Unit Access
  uint8_t dpo_fua;
  // Network byte order
  uint32_t logical_block_address;
  uint32_t transfer_length;
  uint8_t group_number;
  uint8_t control;

  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
} __PACKED;

static_assert(sizeof(Read12CDB) == 12, "Read 12 CDB must be 12 bytes");

struct Read16CDB {
  Opcode opcode;
  // dpo_fua(4) - DPO - Disable Page Out
  // dpo_fua(3) - FUA - Force Unit Access
  uint8_t dpo_fua;
  // Network byte order
  uint64_t logical_block_address;
  uint32_t transfer_length;
  uint8_t group_number;
  uint8_t control;

  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
} __PACKED;

static_assert(sizeof(Read16CDB) == 16, "Read 16 CDB must be 16 bytes");

struct Write10CDB {
  Opcode opcode;
  // dpo_fua(4) - DPO - Disable Page Out
  // dpo_fua(3) - FUA - Write to medium
  // dpo_fua(1) - FUA_NV - If NV_SUP is 1, prefer write to nonvolatile cache.
  uint8_t dpo_fua;
  // Network byte order
  uint32_t logical_block_address;
  uint8_t group_number;
  uint16_t transfer_length;
  uint8_t control;

  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
} __PACKED;

static_assert(sizeof(Write10CDB) == 10, "Write 10 CDB must be 10 bytes");

struct Write12CDB {
  Opcode opcode;
  // dpo_fua(4) - DPO - Disable Page Out
  // dpo_fua(3) - FUA - Write to medium
  // dpo_fua(1) - FUA_NV - If NV_SUP is 1, prefer write to nonvolatile cache.
  uint8_t dpo_fua;
  // Network byte order
  uint32_t logical_block_address;
  uint32_t transfer_length;
  uint8_t group_number;
  uint8_t control;

  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
} __PACKED;

static_assert(sizeof(Write12CDB) == 12, "Write 12 CDB must be 12 bytes");

struct Write16CDB {
  Opcode opcode;
  // dpo_fua(4) - DPO - Disable Page Out
  // dpo_fua(3) - FUA - Write to medium
  // dpo_fua(1) - FUA_NV - If NV_SUP is 1, prefer write to nonvolatile cache.
  uint8_t dpo_fua;
  // Network byte order
  uint64_t logical_block_address;
  uint32_t transfer_length;
  uint8_t group_number;
  uint8_t control;

  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
} __PACKED;

static_assert(sizeof(Write16CDB) == 16, "Write 16 CDB must be 16 bytes");

struct SynchronizeCache10CDB {
  Opcode opcode;
  // syncnv_immed(2) - SYNC_NV - If SYNC_NV is 1 prefer write to nonvolatile cache.
  // syncnv_immed(1) - IMMED - If IMMED is 1 return after CDB has been
  //                           validated.
  uint8_t syncnv_immed;
  uint32_t logical_block_address;
  uint8_t group_number;
  uint16_t num_blocks;
  uint8_t control;
} __PACKED;

static_assert(sizeof(SynchronizeCache10CDB) == 10, "Synchronize Cache 10 CDB must be 10 bytes");

struct DiskOp;

class Controller {
 public:
  virtual ~Controller() = default;

  // Size of metadata struct required for each command transaction by this controller. This metadata
  // struct must include scsi::DiskOp as its first (and possibly only) member.
  virtual size_t BlockOpSize() = 0;

  // Synchronously execute a SCSI command on the device at target:lun.
  // |cdb| contains the SCSI CDB to execute.
  // |data| and |is_write| specify optional data-out or data-in regions.
  // Returns ZX_OK if the command was successful at both the transport layer and no check
  // condition happened.
  // Typically used for administrative commands where data resides in process memory.
  virtual zx_status_t ExecuteCommandSync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                         iovec data) = 0;

  // Asynchronously execute a SCSI command on the device at target:lun.
  // |cdb| contains the SCSI CDB to execute.
  // |disk_op|, |block_size_bytes|, and |is_write| specify optional data-out or data-in regions.
  // Command execution status is returned by invoking |disk_op|->Complete(status).
  // Typically used for IO commands where data may not reside in process memory.
  virtual void ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                   uint32_t block_size_bytes, DiskOp* disk_op) = 0;

  // Test whether the target-lun is ready.
  zx_status_t TestUnitReady(uint8_t target, uint16_t lun);

  // Read Sense data to |data|.
  zx_status_t RequestSense(uint8_t target, uint16_t lun, iovec data);

  // Return InquiryData for the specified lun.
  zx::result<InquiryData> Inquiry(uint8_t target, uint16_t lun);

  // Read Block Limits VPD Page (0xB0), if supported and return the max transfer size
  // (in blocks) supported by the target.
  zx::result<uint32_t> InquiryMaxTransferBlocks(uint8_t target, uint16_t lun);

  // Return ModeSense6ParameterHeader for the specified lun.
  zx::result<ModeSense6ParameterHeader> ModeSense(uint8_t target, uint16_t lun);

  // Determine if the lun has write cache enabled.
  zx::result<bool> ModeSenseWriteCacheEnabled(uint8_t target, uint16_t lun);

  // Return the block count and block size (in bytes) for the specified lun.
  zx_status_t ReadCapacity(uint8_t target, uint16_t lun, uint64_t* block_count,
                           uint32_t* block_size_bytes);

  // Count the number of addressable LUNs attached to a target.
  zx::result<uint32_t> ReportLuns(uint8_t target);
};

}  // namespace scsi

#endif  // SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_CONTROLLER_H_
