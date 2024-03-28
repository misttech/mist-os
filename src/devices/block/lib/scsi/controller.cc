// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <endian.h>
#include <lib/scsi/controller.h>
#include <lib/scsi/disk.h>
#include <zircon/status.h>

#include <tuple>

#include <safemath/safe_math.h>

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

zx_status_t Controller::RequestSense(uint8_t target, uint16_t lun, iovec data) {
  RequestSenseCDB cdb = {};
  cdb.opcode = Opcode::REQUEST_SENSE;
  cdb.allocation_length = static_cast<uint8_t>(data.iov_len);
  zx_status_t status =
      ExecuteCommandSync(target, lun, {&cdb, sizeof(cdb)}, /*is_write=*/false, data);
  if (status != ZX_OK) {
    zxlogf(DEBUG, "REQUEST_SENSE failed for target %u, lun %u: %s", target, lun,
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
    zxlogf(DEBUG, "INQUIRY failed for target %u, lun %u: %s", target, lun,
           zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(data);
}

zx::result<VPDBlockLimits> Controller::InquiryBlockLimits(uint8_t target, uint16_t lun) {
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
  for (i = 0; i < vpd_pagelist.page_length; ++i) {
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

  return zx::ok(block_limits);
}

zx::result<bool> Controller::InquirySupportUnmapCommand(uint8_t target, uint16_t lun) {
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
  for (i = 0; i < vpd_pagelist.page_length; ++i) {
    if (vpd_pagelist.pages[i] == InquiryCDB::kLogicalBlockProvisioningVpdPageCode) {
      break;
    }
  }
  if (i == vpd_pagelist.page_length) {
    zxlogf(ERROR, "The Logical Block Provisioning VPD page is not supported for target %u, lun %u.",
           target, lun);
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  // The Block Limits VPD page is supported, fetch it.
  cdb.page_code = InquiryCDB::kLogicalBlockProvisioningVpdPageCode;
  VPDLogicalBlockProvisioning provisioning = {};
  cdb.allocation_length = htobe16(sizeof(provisioning));
  status = ExecuteCommandSync(target, lun, {&cdb, sizeof(cdb)}, /*is_write=*/false,
                              {&provisioning, sizeof(provisioning)});
  if (status != ZX_OK) {
    zxlogf(ERROR, "INQUIRY failed for target %u, lun %u: %s", target, lun,
           zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok(provisioning.lbpu());
}

zx::result<> Controller::ModeSense(uint8_t target, uint16_t lun, PageCode page_code, iovec data,
                                   bool use_mode_sense_6) {
  // Allocate as much as the largest mode sense CDB.
  uint8_t cdb[sizeof(ModeSense10CDB)] = {};
  size_t cdb_size = 0;

  if (use_mode_sense_6 && data.iov_len <= UINT8_MAX) {  // MODE SENSE 6
    if (data.iov_len < sizeof(ModeSense6ParameterHeader)) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    ModeSense6CDB* cdb_6 = reinterpret_cast<ModeSense6CDB*>(cdb);
    cdb_6->opcode = Opcode::MODE_SENSE_6;
    cdb_6->set_page_code(page_code);
    cdb_6->allocation_length = safemath::checked_cast<uint8_t>(data.iov_len);
    // Do not return any block descriptors.
    cdb_6->set_disable_block_descriptors(true);

    cdb_size = sizeof(ModeSense6CDB);
  } else {  // MODE SENSE 10
    if (data.iov_len < sizeof(ModeSense10ParameterHeader) || data.iov_len > UINT16_MAX) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    ModeSense10CDB* cdb_10 = reinterpret_cast<ModeSense10CDB*>(cdb);
    cdb_10->opcode = Opcode::MODE_SENSE_10;
    cdb_10->set_page_code(page_code);
    cdb_10->allocation_length = htobe16(safemath::checked_cast<uint16_t>(data.iov_len));
    // Do not return any block descriptors.
    cdb_10->set_disable_block_descriptors(true);

    cdb_size = sizeof(ModeSense10CDB);
  }

  zx_status_t status = ExecuteCommandSync(target, lun, {&cdb, cdb_size},
                                          /*is_write=*/false, data);
  if (status != ZX_OK) {
    zxlogf(ERROR, "MODE_SENSE_%zu failed for target %u, lun %u: %s", cdb_size, target, lun,
           zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<std::tuple<bool, bool>> Controller::ModeSenseDpoFuaAndWriteProtectedEnabled(
    uint8_t target, uint16_t lun, bool use_mode_sense_6) {
  constexpr uint8_t header_size =
      std::max(sizeof(ModeSense6ParameterHeader), sizeof(ModeSense10ParameterHeader));
  uint8_t data[header_size];

  zx::result result =
      ModeSense(target, lun, PageCode::kAllPageCode, {data, sizeof(data)}, use_mode_sense_6);
  if (result.is_error()) {
    zxlogf(ERROR, "MODE_SENSE failed for target %u, lun %u: %s", target, lun,
           result.status_string());
    return result.take_error();
  }

  bool dpo_fua_available, write_protected;
  if (use_mode_sense_6) {
    ModeSense6ParameterHeader* parameter_header =
        reinterpret_cast<ModeSense6ParameterHeader*>(data);
    dpo_fua_available = parameter_header->dpo_fua_available();
    write_protected = parameter_header->write_protected();
  } else {
    ModeSense10ParameterHeader* parameter_header =
        reinterpret_cast<ModeSense10ParameterHeader*>(data);
    dpo_fua_available = parameter_header->dpo_fua_available();
    write_protected = parameter_header->write_protected();
  }

  return zx::ok(std::make_tuple(dpo_fua_available, write_protected));
}

zx::result<bool> Controller::ModeSenseWriteCacheEnabled(uint8_t target, uint16_t lun,
                                                        bool use_mode_sense_6) {
  constexpr uint8_t header_size =
      std::max(sizeof(ModeSense6ParameterHeader), sizeof(ModeSense10ParameterHeader));
  uint8_t data[header_size + sizeof(CachingModePage)];

  zx::result result =
      ModeSense(target, lun, PageCode::kCachingPageCode, {data, sizeof(data)}, use_mode_sense_6);
  if (result.is_error()) {
    zxlogf(ERROR, "MODE_SENSE failed for target %u, lun %u: %s", target, lun,
           result.status_string());
    return result.take_error();
  }

  uint32_t mode_page_offset =
      use_mode_sense_6 ? sizeof(ModeSense6ParameterHeader) : sizeof(ModeSense10ParameterHeader);
  CachingModePage* mode_page = reinterpret_cast<CachingModePage*>(data + mode_page_offset);
  if (mode_page->page_code() != static_cast<uint8_t>(PageCode::kCachingPageCode)) {
    zxlogf(ERROR, "failed for target %u, lun %u to retrieve caching mode page", target, lun);
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok(mode_page->write_cache_enabled());
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

zx::result<uint16_t> Controller::ReportLuns(uint8_t target) {
  ReportLunsCDB cdb = {};
  cdb.opcode = Opcode::REPORT_LUNS;
  ReportLunsParameterDataHeader data = {};
  cdb.allocation_length = htobe32(sizeof(data));
  zx_status_t status =
      ExecuteCommandSync(target, 0, {&cdb, sizeof(cdb)}, /*is_write=*/false, {&data, sizeof(data)});
  if (status != ZX_OK) {
    // Do not log the error, as it generates too many messages. Instead, log on success.
    return zx::error(status);
  } else {
    zxlogf(DEBUG, "REPORT_LUNS succeeded for target %u.", target);
  }

  // data.lun_list_length is the number of bytes of LUN structures.
  const uint32_t lun_count = betoh32(data.lun_list_length) / 8;
  if (lun_count > UINT16_MAX) {
    zxlogf(ERROR, "REPORT_LUNS returned unexpectedly large LUN count: %u", lun_count);
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  return zx::ok(static_cast<uint16_t>(lun_count));
}

zx_status_t Controller::StartStopUnit(uint8_t target, uint16_t lun, bool immed,
                                      PowerCondition power_condition, uint8_t modifier,
                                      std::optional<bool> load_or_unload) {
  StartStopUnitCDB cdb = {};
  cdb.opcode = Opcode::START_STOP_UNIT;
  cdb.set_immed(immed);
  cdb.set_power_condition(power_condition);
  cdb.set_power_condition_modifier(modifier);
  cdb.set_no_flush(false);  // Currently, we only support flush.
  if (load_or_unload.has_value()) {
    if (power_condition != PowerCondition::kStartValid) {
      zxlogf(ERROR,
             "Power condition must be START_VALID to perform load/unload, power_condition=0x%x",
             static_cast<uint8_t>(power_condition));
      return ZX_ERR_INVALID_ARGS;
    }
    cdb.set_load_eject(true);
    cdb.set_start(load_or_unload.value());  // Load = true, unload = false
  }
  cdb.set_start(0);
  zx_status_t status =
      ExecuteCommandSync(target, lun, {&cdb, sizeof(cdb)}, /*is_write=*/false, {nullptr, 0});
  if (status != ZX_OK) {
    zxlogf(ERROR, "START STOP UNIT failed for target %u, lun %u: %s", target, lun,
           zx_status_get_string(status));
  }
  return status;
}

zx_status_t Controller::FormatUnit(uint8_t target, uint16_t lun) {
  FormatUnitCDB cdb = {};
  cdb.opcode = Opcode::FORMAT_UNIT;
  cdb.set_fmtpinfo(0);      // Currently, only supports type 0 protection.
  cdb.set_fmtdata(false);   // Currently, we do not send the parameter list.
  cdb.set_longlist(false);  // If the FMTDATA is set to zero, then the LONGLIST shall be ignored.
  cdb.set_cmplst(false);    // If the FMTDATA is set to zero, then the CMPLST shall be ignored.
  // If the FMTDATA is set to zero, then the DEFECT_LIST_FORMAT is not available.
  cdb.set_defect_list_format(0);

  zx_status_t status =
      ExecuteCommandSync(target, lun, {&cdb, sizeof(cdb)}, /*is_write=*/false, {nullptr, 0});
  if (status != ZX_OK) {
    zxlogf(ERROR, "FORMAT UNIT failed for target %u, lun %u: %s", target, lun,
           zx_status_get_string(status));
  }
  return status;
}

zx::result<uint32_t> Controller::ScanAndBindLogicalUnits(zx_device_t* device, uint8_t target,
                                                         uint32_t max_transfer_bytes,
                                                         uint16_t max_lun, LuCallback lu_callback,
                                                         DiskOptions disk_options) {
  zx::result<uint16_t> lun_count = ReportLuns(target);
  if (lun_count.is_error()) {
    return lun_count.take_error();
  }

  // TODO(b/317838849): We should only attempt to bind to the luns obtained by ReportLuns().
  uint16_t luns_found = 0;
  for (uint16_t lun = 0; lun < max_lun; ++lun) {
    zx::result disk = Disk::Bind(device, this, target, lun, max_transfer_bytes, disk_options);
    if (disk.is_ok()) {
      if (lu_callback) {
        zx::result result = lu_callback(lun, disk->block_size_bytes(), disk->block_count());
        if (result.is_error()) {
          zxlogf(ERROR, "SCSI: lu_callback for block device failed: %s", disk.status_string());
          return result.take_error();
        }
      }
      ++luns_found;
    }

    if (luns_found == lun_count.value()) {
      break;
    }
  }

  if (luns_found != lun_count.value()) {
    zxlogf(ERROR, "SCSI: Lun count(%d) and the number of luns found(%d) are different.",
           lun_count.value(), luns_found);
    return zx::error(ZX_ERR_BAD_STATE);
  }

  return zx::ok(lun_count.value());
}

}  // namespace scsi
