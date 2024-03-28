// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_CONTROLLER_H_
#define SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_CONTROLLER_H_

#include <lib/ddk/debug.h>
#include <lib/fit/function.h>
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
  FORMAT_UNIT = 0x04,
  INQUIRY = 0x12,
  MODE_SELECT_6 = 0x15,
  MODE_SENSE_6 = 0x1A,
  START_STOP_UNIT = 0x1B,
  SEND_DIAGNOSTIC = 0x1D,
  TOGGLE_REMOVABLE = 0x1E,
  READ_FORMAT_CAPACITIES = 0x23,
  READ_CAPACITY_10 = 0x25,
  READ_10 = 0x28,
  WRITE_10 = 0x2A,
  VERIFY_10 = 0x2F,
  SYNCHRONIZE_CACHE_10 = 0x35,
  WRITE_BUFFER = 0x3B,
  UNMAP = 0x42,
  MODE_SELECT_10 = 0x55,
  MODE_SENSE_10 = 0x5A,
  READ_16 = 0x88,
  WRITE_16 = 0x8A,
  VERIFY_16 = 0x8F,
  READ_CAPACITY_16 = 0x9E,
  REPORT_LUNS = 0xA0,
  SECURITY_PROTOCOL_IN = 0xA2,
  READ_12 = 0xA8,
  WRITE_12 = 0xAA,
  VERIFY_12 = 0xAF,
  SECURITY_PROTOCOL_OUT = 0xB5,
};

enum class StatusCode : uint8_t {
  GOOD = 0x00,
  CHECK_CONDITION = 0x02,
  CONDITION_MET = 0x04,
  BUSY = 0x08,
  RESERVATION_CONFILCT = 0x18,
  TASK_SET_FULL = 0x28,
  ACA_ACTIVE = 0x30,
  TASK_ABORTED = 0x40,
};

enum class SenseKey : uint8_t {
  NO_SENSE = 0x00,
  RECOVERED_ERROR = 0x01,
  NOT_READY = 0x02,
  MEDIUM_ERROR = 0x03,
  HARDWARE_ERROR = 0x04,
  ILLEGAL_REQUEST = 0x05,
  UNIT_ATTENTION = 0x06,
  DATA_PROTECT = 0x07,
  BLANK_CHECK = 0x08,
  VENDOR_SPECIFIC = 0x09,
  COPY_ABORTED = 0x0A,
  ABORTED_COMMAND = 0x0B,
  RESERVED_1 = 0x0C,
  VOLUME_OVERFLOW = 0x0D,
  MISCOMPARE = 0x0E,
  RESERVED_2 = 0x0F,
};

// SCSI command structures (CDBs)

struct TestUnitReadyCDB {
  Opcode opcode;
  uint8_t reserved[4];
  uint8_t control;
} __PACKED;

static_assert(sizeof(TestUnitReadyCDB) == 6, "TestUnitReady CDB must be 6 bytes");

struct InquiryCDB {
  static constexpr uint8_t kPageListVpdPageCode = 0x00;
  static constexpr uint8_t kBlockLimitsVpdPageCode = 0xB0;
  static constexpr uint8_t kLogicalBlockProvisioningVpdPageCode = 0xB2;

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

// SBC-3, section 6.6.3 "Block Limits VPD page".
struct VPDBlockLimits {
  uint8_t peripheral_qualifier_device_type;
  uint8_t page_code;
  uint16_t page_length;
  uint8_t reserved;
  uint8_t maximum_compare_and_write_blocks;
  uint16_t optimal_transfer_blocks_granularity;
  uint32_t maximum_transfer_blocks;
  uint32_t optimal_transfer_blocks;
  uint32_t maximum_prefetch_blocks;
  uint32_t maximum_unmap_lba_count;
  uint32_t maximum_unmap_block_descriptor_count;
  uint32_t optimal_unmap_granularity;
  uint32_t unmap_granularity_alignment;
  uint64_t maximum_write_same_blocks;
} __PACKED;

static_assert(sizeof(VPDBlockLimits) == 44, "BlockLimits Page must be 44 bytes");

// SBC-3, section 6.6.4 "Logical Block Provisioning VPD page".
struct VPDLogicalBlockProvisioning {
  uint8_t peripheral_qualifier_device_type;
  uint8_t page_code;
  uint16_t page_length;
  uint8_t threshold_exponent;
  uint8_t lbpu_lbpws_lbprz_anc_sup_dp;
  uint8_t reserved_provisioning_type;
  uint8_t reserved;
  uint8_t provisioning_group_descriptor[];

  DEF_SUBBIT(lbpu_lbpws_lbprz_anc_sup_dp, 7, lbpu);     // Logical block provisioning UNMAP
  DEF_SUBBIT(lbpu_lbpws_lbprz_anc_sup_dp, 6, lbpws);    // Logical block provisioning WRITE SAME(16)
  DEF_SUBBIT(lbpu_lbpws_lbprz_anc_sup_dp, 5, lbpws10);  // Logical block provisioning WRITE SAME(10)
  DEF_SUBBIT(lbpu_lbpws_lbprz_anc_sup_dp, 2, lbprz);    // Logical block provisioning read zeros
  DEF_SUBBIT(lbpu_lbpws_lbprz_anc_sup_dp, 1, anc_sup);  // Anchor supported
  DEF_SUBBIT(lbpu_lbpws_lbprz_anc_sup_dp, 0, dp);       // Descriptor present
  DEF_SUBFIELD(reserved_provisioning_type, 2, 0, provisioning_type);
} __PACKED;

static_assert(sizeof(VPDLogicalBlockProvisioning) == 8,
              "LogicalBlockProvisioning Page must be 8 bytes");

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

struct SenseDataHeader {
  uint8_t response_code;
  uint8_t sense_key;
  uint8_t additional_sense_code;
  uint8_t additional_sense_code_qualifier;
  uint8_t sdat_ovfl;
  uint8_t reserved[2];
  uint8_t additional_sense_length;
  // Sense data descriptors follow after 8 bytes.
} __PACKED;

static_assert(sizeof(SenseDataHeader) == 8, "Sense data header must be 8 bytes");

struct FixedFormatSenseDataHeader {
  // valid_resp_code (7) is 'valid'
  // valid_resp_code (6 downto 0) is 'response code'
  uint8_t valid_resp_code;
  uint8_t obsolete;
  // mark_sense_key (7) is 'filemark'
  // mark_sense_key (6) is 'EOM (End-of-Medium)'
  // mark_sense_key (5) is 'ILI (Incorrect length indicator)'
  // mark_sense_key (3 downto 0) is 'sense key'
  uint8_t mark_sense_key;
  uint32_t information;
  uint8_t additional_sense_length;
  uint32_t command_specific_information;
  uint8_t additional_sense_code;
  uint8_t additional_sense_code_qualifier;
  uint8_t field_replaceable_unit_code;
  // sense_key_specific[0] (7) is 'SKSV (Sense-key Specific Valid)'
  uint8_t sense_key_specific[3];

  DEF_SUBBIT(valid_resp_code, 7, valid);
  DEF_SUBFIELD(valid_resp_code, 6, 0, response_code);
  DEF_SUBBIT(mark_sense_key, 7, filemark);
  DEF_SUBBIT(mark_sense_key, 6, eom);
  DEF_SUBBIT(mark_sense_key, 5, ili);
  DEF_ENUM_SUBFIELD(mark_sense_key, SenseKey, 3, 0, sense_key);
  DEF_SUBBIT(sense_key_specific[0], 7, sksv);
  // Additional sense byte follow after 18 bytes.
} __PACKED;

static_assert(sizeof(FixedFormatSenseDataHeader) == 18,
              "Fixed format Sense data header must be 18 bytes");
enum class PageCode : uint8_t {
  kCachingPageCode = 0x08,
  kAllPageCode = 0x3F,
};

// SPC-4, section 6.11 "MODE SENSE(6) command".
struct ModeSense6CDB {
  Opcode opcode;
  // dbd (3) is 'DBD (Disable block descriptors)'
  uint8_t dbd;
  // pc_and_page_code (7 downto 6) is 'PC (Page control)'
  // pc_and_page_code (5 downto 0) is 'PAGE CODE'
  uint8_t pc_and_page_code;
  uint8_t subpage_code;
  uint8_t allocation_length;
  uint8_t control;

  // If disable_block_descriptors is '1', device will not return Block Descriptors.
  DEF_SUBBIT(dbd, 3, disable_block_descriptors);
  // page_control should be 00h for current devices.
  DEF_SUBFIELD(pc_and_page_code, 7, 6, page_control);
  DEF_ENUM_SUBFIELD(pc_and_page_code, PageCode, 5, 0, page_code);
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
  DEF_SUBBIT(device_specific_parameter, 4, dpo_fua_available);
} __PACKED;

static_assert(sizeof(ModeSense6ParameterHeader) == 4, "Mode Sense 6 parameters must be 4 bytes");

// SPC-4, section 6.12 "MODE SENSE(10) command".
struct ModeSense10CDB {
  Opcode opcode;
  // dbd (4) is 'LLBAA (Long LBA accepted)'
  // dbd (3) is 'DBD (Disalbe block descriptors)'
  uint8_t llbaa_dbd;
  // pc_and_page_code (7 downto 6) is 'PC (Page control)'
  // pc_and_page_code (5 downto 0) is 'PAGE CODE'
  uint8_t pc_and_page_code;
  uint8_t subpage_code;
  uint8_t reserved[3];
  uint16_t allocation_length;
  uint8_t control;

  DEF_SUBBIT(llbaa_dbd, 4, long_lba_accepted);
  // If disable_block_descriptors is '1', device will not return Block Descriptors.
  DEF_SUBBIT(llbaa_dbd, 3, disable_block_descriptors);
  // page_control should be 00h for current devices.
  DEF_SUBFIELD(pc_and_page_code, 7, 6, page_control);
  DEF_ENUM_SUBFIELD(pc_and_page_code, PageCode, 5, 0, page_code);
} __PACKED;

static_assert(sizeof(ModeSense10CDB) == 10, "Mode Sense 10 CDB must be 10 bytes");

struct ModeSense10ParameterHeader {
  uint16_t mode_data_length;
  // 00h is 'Direct Access Block Device'
  uint8_t medium_type;
  // For Direct Access Block Devices:
  // device_specific_parameter(7) is write-protected bit
  // device_specific_parameter(4) is disable page out/force unit access available
  uint8_t device_specific_parameter;
  uint8_t reserved[2];
  uint16_t block_descriptor_length;

  DEF_SUBBIT(device_specific_parameter, 7, write_protected);
  DEF_SUBBIT(device_specific_parameter, 4, dpo_fua_available);
  DEF_SUBBIT(reserved[0], 0, long_lba);
} __PACKED;

static_assert(sizeof(ModeSense10ParameterHeader) == 8, "Mode Sense 10 parameters must be 8 bytes");

struct CachingModePage {
  uint8_t ps_spf_and_page_code;
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

  DEF_SUBFIELD(ps_spf_and_page_code, 5, 0, page_code);
  DEF_SUBBIT(control_bits, 2, write_cache_enabled);
  DEF_SUBBIT(control_bits, 0, read_cache_disabled);
} __PACKED;

static_assert(sizeof(CachingModePage) == 20, "Caching Mode Page must be 20 bytes");

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
  // dpo_fua(7 downto 5) - Read protect
  // dpo_fua(4) - DPO - Disable Page Out
  // dpo_fua(3) - FUA - Force Unit Access
  // dpo_fua(1) - FUA_NV - If NV_SUP is 1, prefer read from nonvolatile cache.
  uint8_t dpo_fua;
  // Network byte order
  uint32_t logical_block_address;
  // group_num (4 downto 0) is 'group_number'
  uint8_t group_num;
  uint16_t transfer_length;
  uint8_t control;

  DEF_SUBFIELD(dpo_fua, 7, 5, rd_protect);
  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
  DEF_SUBBIT(dpo_fua, 1, force_unit_access_nv_cache);
  DEF_SUBFIELD(group_num, 4, 0, group_number);
} __PACKED;

static_assert(sizeof(Read10CDB) == 10, "Read 10 CDB must be 10 bytes");

struct Read12CDB {
  Opcode opcode;
  // dpo_fua(4) - DPO - Disable Page Out
  // dpo_fua(3) - FUA - Force Unit Access
  // dpo_fua(1) - FUA_NV - If NV_SUP is 1, prefer read from nonvolatile cache.
  uint8_t dpo_fua;
  // Network byte order
  uint32_t logical_block_address;
  uint32_t transfer_length;
  // group_num (4 downto 0) is 'group_number'
  uint8_t group_num;
  uint8_t control;

  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
  DEF_SUBBIT(dpo_fua, 1, force_unit_access_nv_cache);
  DEF_SUBFIELD(group_num, 4, 0, group_number);
} __PACKED;

static_assert(sizeof(Read12CDB) == 12, "Read 12 CDB must be 12 bytes");

struct Read16CDB {
  Opcode opcode;
  // dpo_fua(4) - DPO - Disable Page Out
  // dpo_fua(3) - FUA - Force Unit Access
  // dpo_fua(1) - FUA_NV - If NV_SUP is 1, prefer read from nonvolatile cache.
  uint8_t dpo_fua;
  // Network byte order
  uint64_t logical_block_address;
  uint32_t transfer_length;
  // group_num (4 downto 0) is 'group_number'
  uint8_t group_num;
  uint8_t control;

  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
  DEF_SUBBIT(dpo_fua, 1, force_unit_access_nv_cache);
  DEF_SUBFIELD(group_num, 4, 0, group_number);
} __PACKED;

static_assert(sizeof(Read16CDB) == 16, "Read 16 CDB must be 16 bytes");

// SBC-3, section 5.29 "VERIFY (10) command".
struct Verify10CDB {
  Opcode opcode;
  // vrprotect_and_dpo_and_bytchk(7 downto 5) is 'VRPROTECT (Verify Protect)'
  // vrprotect_and_dpo_and_bytchk(4) is 'DPO (Disble Page Out)'
  // vrprotect_and_dpo_and_bytchk (2 downto 1) is 'BYTCHK (Byte Check)'
  uint8_t vrprotect_and_dpo_and_bytchk;
  uint32_t logical_block_address;
  // group_num (5 downto 0) - GROUP NUMBER
  uint8_t group_num;
  uint16_t verification_length;
  uint8_t control;

  DEF_SUBFIELD(vrprotect_and_dpo_and_bytchk, 7, 5, vrprotect);
  DEF_SUBBIT(vrprotect_and_dpo_and_bytchk, 4, dpo);
  DEF_SUBFIELD(vrprotect_and_dpo_and_bytchk, 2, 1, bytchk);
  DEF_SUBFIELD(group_num, 5, 0, group_number);
} __PACKED;

static_assert(sizeof(Verify10CDB) == 10, "Verify 10 CDB must be 10 bytes");

struct Write10CDB {
  Opcode opcode;
  // dpo_fua(7 downto 5) - Write protect
  // dpo_fua(4) - DPO - Disable Page Out
  // dpo_fua(3) - FUA - Write to medium
  // dpo_fua(1) - FUA_NV - If NV_SUP is 1, prefer write to nonvolatile cache.
  uint8_t dpo_fua;
  // Network byte order
  uint32_t logical_block_address;
  // group_num (4 downto 0) is 'group_number'
  uint8_t group_num;
  uint16_t transfer_length;
  uint8_t control;

  DEF_SUBFIELD(dpo_fua, 7, 5, wr_protect);
  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
  DEF_SUBBIT(dpo_fua, 1, force_unit_access_nv_cache);
  DEF_SUBFIELD(group_num, 4, 0, group_number);
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
  // group_num (4 downto 0) is 'group_number'
  uint8_t group_num;
  uint8_t control;

  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
  DEF_SUBBIT(dpo_fua, 1, force_unit_access_nv_cache);
  DEF_SUBFIELD(group_num, 4, 0, group_number);
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
  // group_num (4 downto 0) is 'group_number'
  uint8_t group_num;
  uint8_t control;

  DEF_SUBBIT(dpo_fua, 4, disable_page_out);
  DEF_SUBBIT(dpo_fua, 3, force_unit_access);
  DEF_SUBBIT(dpo_fua, 1, force_unit_access_nv_cache);
  DEF_SUBFIELD(group_num, 4, 0, group_number);
} __PACKED;

static_assert(sizeof(Write16CDB) == 16, "Write 16 CDB must be 16 bytes");

struct SynchronizeCache10CDB {
  Opcode opcode;
  // syncnv_immed(2) - SYNC_NV - If SYNC_NV is 1 prefer write to nonvolatile cache.
  // syncnv_immed(1) - IMMED - If IMMED is 1 return after CDB has been
  //                           validated.
  uint8_t syncnv_immed;
  uint32_t logical_block_address;
  // group_num(4 downto 0) is 'group number'
  uint8_t group_num;
  uint16_t num_blocks;
  uint8_t control;

  DEF_SUBBIT(syncnv_immed, 2, sync_nv);
  DEF_SUBBIT(syncnv_immed, 1, immed);
  DEF_SUBFIELD(group_num, 4, 0, group_number);
} __PACKED;

static_assert(sizeof(SynchronizeCache10CDB) == 10, "Synchronize Cache 10 CDB must be 10 bytes");

enum class PowerCondition : uint8_t {
  kStartValid = 0x0,
  kActive = 0x1,
  kIdle = 0x2,
  kStandby = 0x3,
  kObsolete = 0x5,
  kLuControl = 0x7,
  kForceIdle0 = 0xa,
  kForceStandby0 = 0xb,
};

// SBC-3, section 5.25 "START STOP UNIT command".
struct StartStopUnitCDB {
  Opcode opcode;
  // reserved_and_immed(0) is IMMED
  uint8_t reserved_and_immed;
  uint8_t reserved;
  // reserved_and_power_cond_modifier(3 downto 0) is 'power condition modifier'
  uint8_t reserved_and_power_cond_modifier;
  // power_condition_and_bits(7 downto 4) is 'power conditions'
  // power_condition_and_bits(2) is 'no flush'
  // power_condition_and_bits(1) is 'loej (load eject)'
  // power_condition_and_bits(0) is 'start'
  uint8_t power_condition_and_bits;
  uint8_t control;

  DEF_SUBBIT(reserved_and_immed, 0, immed);
  DEF_SUBFIELD(reserved_and_power_cond_modifier, 3, 0, power_condition_modifier);
  DEF_ENUM_SUBFIELD(power_condition_and_bits, PowerCondition, 7, 4, power_condition);
  DEF_SUBBIT(power_condition_and_bits, 2, no_flush);
  DEF_SUBBIT(power_condition_and_bits, 1, load_eject);
  DEF_SUBBIT(power_condition_and_bits, 0, start);
} __PACKED;

static_assert(sizeof(StartStopUnitCDB) == 6, "Start Stop Unit CDB must be 6 bytes");

struct SecurityProtocolInCDB {
  Opcode opcode;
  uint8_t security_protocol;
  uint16_t security_protocol_specific;
  // inc (7) is 'inc 512'
  uint8_t inc;
  uint8_t reserved;
  uint32_t allocation_length;
  uint8_t reserved2;
  uint8_t control;

  DEF_SUBBIT(inc, 7, inc_512);
} __PACKED;

static_assert(sizeof(SecurityProtocolInCDB) == 12, "Security Protocol In CDB must be 12 bytes");

struct SecurityProtocolOutCDB {
  Opcode opcode;
  uint8_t security_protocol;
  uint16_t security_protocol_specific;
  // inc (7) is 'inc 512'
  uint8_t inc;
  uint8_t reserved;
  uint32_t transfer_length;
  uint8_t reserved2;
  uint8_t control;

  DEF_SUBBIT(inc, 7, inc_512);
} __PACKED;

static_assert(sizeof(SecurityProtocolOutCDB) == 12, "Security Protocol Out CDB must be 12 bytes");

struct UnmapCDB {
  Opcode opcode;
  // anch (0) is 'anchor'
  uint8_t anch;
  uint32_t reserved;
  // group_num (4 downto 0) is 'group_number'
  uint8_t group_num;
  uint16_t parameter_list_length;
  uint8_t control;

  DEF_SUBBIT(anch, 0, anchor);
  DEF_SUBFIELD(group_num, 4, 0, group_number);
} __PACKED;

static_assert(sizeof(UnmapCDB) == 10, "Unmap CDB must be 10 bytes");

struct UnmapBlockDescriptor {
  uint64_t logical_block_address;
  uint32_t blocks;
  uint8_t reserved[4];
} __PACKED;

static_assert(sizeof(UnmapBlockDescriptor) == 16, "Unmap block decriptor must be 16 bytes");

struct UnmapParameterListHeader {
  uint16_t data_length;
  uint16_t block_descriptor_data_length;
  uint8_t reserved[4];

  // Unmap block descriptors follow after 8 bytes.
  UnmapBlockDescriptor block_descriptors[];
} __PACKED;

static_assert(sizeof(UnmapParameterListHeader) == 8, "Unmap parameter list header must be 8 bytes");

struct WriteBufferCDB {
  Opcode opcode;
  // mod (4 downto 0) is 'mode'
  uint8_t mod;
  uint8_t buffer_id;
  uint8_t buffer_offset[3];
  uint8_t parameter_list_length[3];
  uint8_t control;

  DEF_SUBFIELD(mod, 4, 0, mode);
} __PACKED;

static_assert(sizeof(WriteBufferCDB) == 10, "Write Buffer CDB must be 10 bytes");

// SBC-3, section 5.3 "FORMAT UNIT command".
struct FormatUnitCDB {
  Opcode opcode;
  uint8_t parameters;
  uint8_t vendor_specific;
  uint16_t obsolete;
  uint8_t control;

  DEF_SUBFIELD(parameters, 7, 6, fmtpinfo);  // Format protection information
  DEF_SUBBIT(parameters, 5, longlist);       // Long list
  DEF_SUBBIT(parameters, 4, fmtdata);        // Format data
  DEF_SUBBIT(parameters, 3, cmplst);         // Complete list
  DEF_SUBFIELD(parameters, 2, 0, defect_list_format);
} __PACKED;

static_assert(sizeof(FormatUnitCDB) == 6, "Format Unit CDB must be 6 bytes");

struct FormatUnitShortParameterListHeader {
  uint8_t reserved_and_protection_field_usage;
  // fov_and_format_option_bits  (7) is 'FOV (Format options valid)'
  // fov_and_format_option_bits  (6) is 'DPRY (Disable primary)'
  // fov_and_format_option_bits  (5) is 'DCRT (Disable certification)'
  // fov_and_format_option_bits  (4) is 'STPF (Stop format)'
  // fov_and_format_option_bits  (3) is 'IP (Initialization pattern)'
  // fov_and_format_option_bits  (2) is 'Obsolete'
  // fov_and_format_option_bits  (1) is 'Immed'
  // fov_and_format_option_bits  (0) is 'Vendor specific'
  uint8_t fov_and_format_option_bits;
  uint16_t defect_list_length;

  DEF_SUBFIELD(reserved_and_protection_field_usage, 2, 0, protection_field_usage);
  DEF_SUBBIT(fov_and_format_option_bits, 7, fov);
  DEF_SUBBIT(fov_and_format_option_bits, 6, dpry);
  DEF_SUBBIT(fov_and_format_option_bits, 5, dcrt);
  DEF_SUBBIT(fov_and_format_option_bits, 4, stpf);
  DEF_SUBBIT(fov_and_format_option_bits, 3, ip);
  DEF_SUBBIT(fov_and_format_option_bits, 2, obsolete);
  DEF_SUBBIT(fov_and_format_option_bits, 1, immed);
  DEF_SUBBIT(fov_and_format_option_bits, 0, vendor_specific);
} __PACKED;

static_assert(sizeof(FormatUnitShortParameterListHeader) == 4,
              "Format Unit Short Parameter List Header must be 4 bytes");

struct FormatUnitLongParameterListHeader {
  uint8_t reserved_and_protection_field_usage;
  // fov_and_format_option_bits  (7) is 'FOV (Format options valid)'
  // fov_and_format_option_bits  (6) is 'DPRY (Disable primary)'
  // fov_and_format_option_bits  (5) is 'DCRT (Disable certification)'
  // fov_and_format_option_bits  (4) is 'STPF (Stop format)'
  // fov_and_format_option_bits  (3) is 'IP (Initialization pattern)'
  // fov_and_format_option_bits  (2) is 'Obsolete'
  // fov_and_format_option_bits  (1) is 'Immed'
  // fov_and_format_option_bits  (0) is 'Vendor specific'
  uint8_t fov_and_format_option_bits;
  uint8_t reserved;
  // information_and_protection_interval_exponent (7 downto 4) is 'P_I_INFORMATION'
  // information_and_protection_interval_exponent (3 downto 0) is 'PROTECTION INTERVAL EXPONENT'
  uint8_t information_and_protection_interval_exponent;
  uint32_t defect_list_length;

  DEF_SUBFIELD(reserved_and_protection_field_usage, 2, 0, protection_field_usage);
  DEF_SUBBIT(fov_and_format_option_bits, 7, fov);
  DEF_SUBBIT(fov_and_format_option_bits, 6, dpry);
  DEF_SUBBIT(fov_and_format_option_bits, 5, dcrt);
  DEF_SUBBIT(fov_and_format_option_bits, 4, stpf);
  DEF_SUBBIT(fov_and_format_option_bits, 3, ip);
  DEF_SUBBIT(fov_and_format_option_bits, 2, obsolete);
  DEF_SUBBIT(fov_and_format_option_bits, 1, immed);
  DEF_SUBBIT(fov_and_format_option_bits, 0, vendor_specific);
  DEF_SUBFIELD(information_and_protection_interval_exponent, 7, 4, p_i_information);
  DEF_SUBFIELD(information_and_protection_interval_exponent, 3, 0, protection_interval_exponent);
} __PACKED;

static_assert(sizeof(FormatUnitLongParameterListHeader) == 8,
              "Format Unit Long Parameter List Header must be 8 bytes");

struct FormatUnitInitializationPatternDescriptor {
  // security_initialize (5) is 'SI (Security initialize)'
  uint8_t security_initialize;
  uint8_t initialization_pattern_type;
  uint16_t initialization_pattern_length;
  uint8_t initialization_pattern[];

  DEF_SUBBIT(security_initialize, 5, si);
} __PACKED;

static_assert(sizeof(FormatUnitInitializationPatternDescriptor) == 4,
              "Format Unit Initialization Pattern Descriptor must be 4 bytes");

enum class SelfTestCode : uint8_t {
  kNone = 0x0,
  kBackgroundShortSelfTest = 0x1,
  kBackgroundExtendedSelfTest = 0x2,
  kReserved1 = 0x3,
  kAbortBackgroundSelfTest = 0x4,
  kForegroundShortSelfTest = 0x5,
  kForegroundExtendedSelfTest = 0x6,
  kReserved2 = 0x7,
};

// SPC-4, section 6.32 "SEND DIAGNOSTIC command".
struct SendDiagnosticCDB {
  Opcode opcode;
  // self_test_code_and_parameters (7 downto 5) is 'SELF-TEST CODE'
  // self_test_code_and_parameters (4) is 'PF (Page format)'
  // self_test_code_and_parameters (2) is 'SELFTEST (Self test)'
  // self_test_code_and_parameters (1) is 'DEVOFFL (SCSI target device offline)'
  // self_test_code_and_parameters (0) is 'UNITOFFL (Unit offline)'
  uint8_t self_test_code_and_parameters;
  uint8_t reserved;
  uint16_t parameter_list_length;
  uint8_t control;

  DEF_ENUM_SUBFIELD(self_test_code_and_parameters, SelfTestCode, 7, 5, self_test_code);
  DEF_SUBBIT(self_test_code_and_parameters, 4, pf);
  DEF_SUBBIT(self_test_code_and_parameters, 2, self_test);
  DEF_SUBBIT(self_test_code_and_parameters, 1, dev_off_l);
  DEF_SUBBIT(self_test_code_and_parameters, 0, unit_off_l);
} __PACKED;

static_assert(sizeof(SendDiagnosticCDB) == 6, "Send Diagnostic CDB must be 6 bytes");

struct DiskOp;
struct DiskOptions;

using LuCallback =
    fit::function<zx::result<>(uint16_t lun, size_t block_size, uint64_t block_count)>;

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
  // The |data| is used when there is an additional data buffer to pass. For example, an operation
  // like TRIM(block_trim_t) does not have a data vmo, but the SCSI UNMAP command requires a data
  // vmo to record the address and length of the block to be trimmed. In this case, the additional
  // buffer is passed through |data| and the device driver creates and manages the data vmo.
  virtual void ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                   uint32_t block_size_bytes, DiskOp* disk_op,
                                   iovec data = {nullptr, 0}) = 0;

  // Test whether the target-lun is ready.
  zx_status_t TestUnitReady(uint8_t target, uint16_t lun);

  // Read Sense data to |data|.
  zx_status_t RequestSense(uint8_t target, uint16_t lun, iovec data);

  // Return InquiryData for the specified lun.
  zx::result<InquiryData> Inquiry(uint8_t target, uint16_t lun);

  // Read Block Limits VPD Page (0xB0), if supported and return the max transfer size
  // (in blocks) supported by the target.
  zx::result<VPDBlockLimits> InquiryBlockLimits(uint8_t target, uint16_t lun);

  // Read Logical Block Provisioning VPD Page (0xB2), check that it supports the UNMAP command.
  zx::result<bool> InquirySupportUnmapCommand(uint8_t target, uint16_t lun);

  // Return ModeSense(6|10)ParameterHeader for the specified lun.
  zx::result<> ModeSense(uint8_t target, uint16_t lun, PageCode page_code, iovec data,
                         bool use_mode_sense_6);

  // Determine if the lun has DPO FUA available and Write protected.
  zx::result<std::tuple<bool, bool>> ModeSenseDpoFuaAndWriteProtectedEnabled(uint8_t target,
                                                                             uint16_t lun,
                                                                             bool use_mode_sense_6);

  // Determine if the lun has write cache enabled.
  zx::result<bool> ModeSenseWriteCacheEnabled(uint8_t target, uint16_t lun, bool use_mode_sense_6);

  // Return the block count and block size (in bytes) for the specified lun.
  zx_status_t ReadCapacity(uint8_t target, uint16_t lun, uint64_t* block_count,
                           uint32_t* block_size_bytes);

  // Count the number of addressable LUNs attached to a target.
  zx::result<uint16_t> ReportLuns(uint8_t target);

  // Change the power condition of the logical unit. If |load_or_unload| is true, load the medium,
  // if false, unload it. If |load_or_unload| is std::nullopt, do not load/unload.
  zx_status_t StartStopUnit(uint8_t target, uint16_t lun, bool immed,
                            PowerCondition power_condition, uint8_t modifier = 0,
                            std::optional<bool> load_or_unload = std::nullopt);

  // Format the selected LU. Currently only supports type 0 protection, FMTDATA=0 (mandatory), and
  // does not send a parameter list.
  zx_status_t FormatUnit(uint8_t target, uint16_t lun);

  // Request diagnostic operation to the device.
  // This function currently only supports the default self-test feature, which is the minimum
  // requirement.
  zx_status_t SendDiagnostic(uint8_t target, uint16_t lun, SelfTestCode code);

  // Check the status of each LU and bind it. This function returns the number of LUs found.
  zx::result<uint32_t> ScanAndBindLogicalUnits(zx_device_t* device, uint8_t target,
                                               uint32_t max_transfer_bytes, uint16_t max_lun,
                                               LuCallback lu_callback, DiskOptions disk_options);
};

}  // namespace scsi

#endif  // SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_CONTROLLER_H_
