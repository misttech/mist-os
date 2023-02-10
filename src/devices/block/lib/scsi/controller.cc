// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <endian.h>
#include <lib/scsi/controller.h>
#include <zircon/status.h>

namespace scsi {

zx_status_t Controller::TestUnitReady(uint8_t target, uint16_t lun) {
  scsi::TestUnitReadyCDB cdb = {};
  cdb.opcode = scsi::Opcode::TEST_UNIT_READY;
  zx_status_t status =
      ExecuteCommandSync(target, lun, {&cdb, sizeof(cdb)}, /*is_write=*/false, {nullptr, 0});
  if (status != ZX_OK) {
    zxlogf(DEBUG, "TEST_UNIT_READY failed for target %u, lun %u: %s", target, lun,
           zx_status_get_string(status));
  }
  return status;
}

zx::result<InquiryData> Controller::Inquiry(uint8_t target, uint16_t lun) {
  InquiryCDB cdb = {};
  cdb.opcode = Opcode::INQUIRY;
  InquiryData data = {};
  cdb.allocation_length = htobe16(sizeof(data));
  zx_status_t status = ExecuteCommandSync(target, lun, {&cdb, sizeof(cdb)}, /*is_write=*/false,
                                          {&data, sizeof(data)});
  if (status != ZX_OK) {
    zxlogf(ERROR, "INQUIRY failed for target %u, lun %u: %s", target, lun,
           zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(data);
}

zx::result<uint32_t> Controller::InquiryMaxTransferBlocks(uint8_t target, uint16_t lun) {
  InquiryCDB cdb = {};
  cdb.opcode = Opcode::INQUIRY;
  // Query for all supported VPD pages.
  cdb.reserved_and_evpd = 0x1;
  cdb.page_code = 0x00;
  VPDPageList vpd_pagelist = {};
  cdb.allocation_length = htobe16(sizeof(vpd_pagelist));
  zx_status_t status = ExecuteCommandSync(target, lun, {&cdb, sizeof(cdb)}, /*is_write=*/false,
                                          {&vpd_pagelist, sizeof(vpd_pagelist)});
  if (status != ZX_OK) {
    zxlogf(ERROR, "INQUIRY failed for target %u, lun %u: %s", target, lun,
           zx_status_get_string(status));
    return zx::error(status);
  }

  uint8_t i;
  for (i = 0; i < vpd_pagelist.page_length; i++) {
    if (vpd_pagelist.pages[i] == InquiryCDB::kBlockLimitsVpdPageCode) {
      break;
    }
  }
  if (i == vpd_pagelist.page_length) {
    zxlogf(ERROR, "The Block Limits VPD page is not supported for target %u, lun %u.", target, lun);
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  // The Block Limits VPD page is supported, fetch it.
  cdb.page_code = InquiryCDB::kBlockLimitsVpdPageCode;
  VPDBlockLimits block_limits = {};
  cdb.allocation_length = htobe16(sizeof(block_limits));
  status = ExecuteCommandSync(target, lun, {&cdb, sizeof(cdb)}, /*is_write=*/false,
                              {&block_limits, sizeof(block_limits)});
  if (status != ZX_OK) {
    zxlogf(ERROR, "INQUIRY failed for target %u, lun %u: %s", target, lun,
           zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok(betoh32(block_limits.max_xfer_length_blocks));
}

zx::result<ModeSense6ParameterHeader> Controller::ModeSense(uint8_t target, uint16_t lun) {
  ModeSense6CDB cdb = {};
  cdb.opcode = Opcode::MODE_SENSE_6;
  cdb.page_code = ModeSense6CDB::kAllPageCode;
  ModeSense6ParameterHeader data = {};
  cdb.allocation_length = sizeof(data);
  zx_status_t status = ExecuteCommandSync(target, lun, {&cdb, sizeof(cdb)},
                                          /*is_write=*/false, {&data, sizeof(data)});
  if (status != ZX_OK) {
    zxlogf(ERROR, "MODE_SENSE_6 failed for target %u, lun %u: %s", target, lun,
           zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok(data);
}

zx::result<bool> Controller::ModeSenseWriteCacheEnabled(uint8_t target, uint16_t lun) {
  ModeSense6CDB cdb = {};
  cdb.opcode = Opcode::MODE_SENSE_6;
  // Only fetch the caching mode page and get the current values.
  cdb.page_code = ModeSense6CDB::kCachingPageCode;
  CachingModePage data = {};
  cdb.allocation_length = sizeof(data);
  // Do not return any block descriptors.
  cdb.disable_block_descriptors = 0b1000;
  zx_status_t status = ExecuteCommandSync(target, lun, {&cdb, sizeof(cdb)},
                                          /*is_write=*/false, {&data, sizeof(data)});
  if (status != ZX_OK) {
    zxlogf(ERROR, "MODE_SENSE_6 failed for target %u, lun %u: %s", target, lun,
           zx_status_get_string(status));
    return zx::error(status);
  }

  if (data.page_code != ModeSense6CDB::kCachingPageCode) {
    zxlogf(ERROR, "failed for target %u, lun %u to retrieve caching mode page", target, lun);
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok(data.write_cache_enabled());
}

zx_status_t Controller::ReadCapacity(uint8_t target, uint16_t lun, uint64_t* block_count,
                                     uint32_t* block_size_bytes) {
  ReadCapacity10CDB cdb10 = {};
  cdb10.opcode = Opcode::READ_CAPACITY_10;
  ReadCapacity10ParameterData data10 = {};
  zx_status_t status = ExecuteCommandSync(target, lun, {&cdb10, sizeof(cdb10)}, /*is_write=*/false,
                                          {&data10, sizeof(data10)});
  if (status != ZX_OK) {
    zxlogf(ERROR, "READ_CAPACITY_10 failed for target %u, lun %u: %s", target, lun,
           zx_status_get_string(status));
    return status;
  }

  *block_count = betoh32(data10.returned_logical_block_address);
  *block_size_bytes = betoh32(data10.block_length_in_bytes);

  if (*block_count == UINT32_MAX) {
    ReadCapacity16CDB cdb16 = {};
    cdb16.opcode = Opcode::READ_CAPACITY_16;
    cdb16.service_action = 0x10;
    ReadCapacity16ParameterData data16 = {};
    cdb16.allocation_length = htobe32(sizeof(data16));
    status = ExecuteCommandSync(target, lun, {&cdb16, sizeof(cdb16)}, /*is_write=*/false,
                                {&data16, sizeof(data16)});
    if (status != ZX_OK) {
      zxlogf(ERROR, "READ_CAPACITY_16 failed for target %u, lun %u: %s", target, lun,
             zx_status_get_string(status));
      return status;
    }

    *block_count = betoh64(data16.returned_logical_block_address);
    *block_size_bytes = betoh32(data16.block_length_in_bytes);
  }

  // +1 because data.returned_logical_block_address returns the address of the final block, and
  // blocks are zero indexed.
  *block_count = *block_count + 1;
  return ZX_OK;
}

zx::result<uint32_t> Controller::ReportLuns(uint8_t target) {
  ReportLunsCDB cdb = {};
  cdb.opcode = Opcode::REPORT_LUNS;
  ReportLunsParameterDataHeader data = {};
  cdb.allocation_length = htobe32(sizeof(data));
  zx_status_t status =
      ExecuteCommandSync(target, 0, {&cdb, sizeof(cdb)}, /*is_write=*/false, {&data, sizeof(data)});
  if (status != ZX_OK) {
    zxlogf(WARNING, "REPORT_LUNS failed for target %u: %s", target, zx_status_get_string(status));
    return zx::error(status);
  }

  // data.lun_list_length is the number of bytes of LUN structures.
  return zx::ok(betoh32(data.lun_list_length) / 8);
}

}  // namespace scsi
