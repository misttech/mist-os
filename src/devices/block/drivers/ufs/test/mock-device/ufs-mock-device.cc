// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ufs-mock-device.h"

#include <lib/stdcompat/span.h>

namespace ufs {
namespace ufs_mock_device {

UfsLogicalUnit::UfsLogicalUnit() {
  unit_desc_ = UnitDescriptor{
      .bLength = sizeof(UnitDescriptor),
      .bDescriptorIDN = static_cast<size_t>(DescriptorType::kUnit),
      .bLUEnable = 0x00,
      .bLogicalBlockSize = kMockBlockSizeShift,
  };
}

zx_status_t UfsLogicalUnit::Enable(uint8_t lun, uint64_t block_count) {
  if (unit_desc_.bLUEnable != 0) {
    return ZX_ERR_ALREADY_EXISTS;
  }
  block_count_ = block_count;
  unit_desc_.bUnitIndex = lun;
  unit_desc_.qLogicalBlockCount = htobe64(block_count);
  unit_desc_.bLUEnable = 0x01;
  buffer_.resize(kMockBlockSize * block_count);
  return ZX_OK;
}

zx_status_t UfsLogicalUnit::BufferWrite(const void *buf, size_t block_count, off_t block_offset) {
  if (unit_desc_.bLUEnable == 0) {
    return ZX_ERR_UNAVAILABLE;
  }

  uint64_t offset = block_offset * kMockBlockSize;
  uint64_t count = block_count * kMockBlockSize;
  if (offset + count > buffer_.size() || offset >= buffer_.size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  std::memcpy(buffer_.data() + offset, buf, count);
  return ZX_OK;
}

zx_status_t UfsLogicalUnit::BufferRead(void *buf, size_t block_count, off_t block_offset) {
  if (unit_desc_.bLUEnable == 0) {
    return ZX_ERR_UNAVAILABLE;
  }

  uint64_t offset = block_offset * kMockBlockSize;
  uint64_t count = block_count * kMockBlockSize;
  if (offset + count > buffer_.size() || offset >= buffer_.size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  std::memcpy(buf, buffer_.data() + offset, count);
  return ZX_OK;
}

UfsMockDevice::UfsMockDevice(zx::interrupt irq)
    : irq_(std::move(irq)),
      register_mmio_processor_(*this),
      uiccmd_processor_(*this),
      transfer_request_processor_(*this),
      query_request_processor_(*this),
      scsi_command_processor_(*this) {
  VersionReg::Get()
      .ReadFrom(&registers_)
      .set_major_version_number(kMajorVersion)
      .set_minor_version_number(kMinorVersion)
      .set_version_suffix(kVersionSuffix)
      .WriteTo(&registers_);
  CapabilityReg::Get()
      .ReadFrom(&registers_)
      .set__64_bit_addressing_supported(true)
      .set_number_of_utp_task_management_request_slots(kNutmrs - 1)
      .set_number_of_utp_transfer_request_slots(kNutrs - 1)
      .WriteTo(&registers_);
  HostControllerStatusReg::Get()
      .ReadFrom(&registers_)
      .set_utp_task_management_request_list_ready(true)
      .set_utp_transfer_request_list_ready(true)
      .set_device_present(true)
      .WriteTo(&registers_);
  HostControllerEnableReg::Get()
      .ReadFrom(&registers_)
      .set_host_controller_enable(true)
      .WriteTo(&registers_);

  std::memset(&device_desc_, 0, sizeof(DeviceDescriptor));
  device_desc_.bLength = sizeof(DeviceDescriptor);
  device_desc_.bDescriptorIDN = static_cast<size_t>(DescriptorType::kDevice);
  device_desc_.bDeviceSubClass = 0x01;
  device_desc_.bNumberWLU = 0x04;
  device_desc_.bInitPowerMode = 0x01;
  device_desc_.bHighPriorityLUN = 0x7F;
  device_desc_.wSpecVersion = htobe16(0x0310);
  device_desc_.bUD0BaseOffset = 0x16;
  device_desc_.bUDConfigPLength = 0x1A;

  std::memset(&geometry_desc_, 0, sizeof(GeometryDescriptor));
  geometry_desc_.bLength = sizeof(GeometryDescriptor);
  geometry_desc_.bDescriptorIDN = static_cast<size_t>(DescriptorType::kGeometry);
  geometry_desc_.qTotalRawDeviceCapacity = htobe64(kMockTotalDeviceCapacity >> 9);  // 16MB
  static_assert(kMaxLun == 8 || kMaxLun == 32, "Max Number of Logical Unit should be 8 or 32.");
  if constexpr (kMaxLun == 8) {
    geometry_desc_.bMaxNumberLU = 0x00;
  } else if (kMaxLun == 32) {
    geometry_desc_.bMaxNumberLU = 0x01;
  }
}

zx_status_t UfsMockDevice::AddLun(uint8_t lun) {
  if (lun >= logical_units_.size()) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (auto err = logical_units_[lun].Enable(lun, kMockTotalDeviceCapacity / kMockBlockSize);
      err != ZX_OK) {
    return err;
  }
  ++device_desc_.bNumberLU;
  return ZX_OK;
}

zx_status_t UfsMockDevice::BufferWrite(uint8_t lun, const void *buf, size_t block_count,
                                       off_t block_offset) {
  if (lun >= logical_units_.size()) {
    return ZX_ERR_INVALID_ARGS;
  }

  return logical_units_[lun].BufferWrite(buf, block_count, block_offset);
}

zx_status_t UfsMockDevice::BufferRead(uint8_t lun, void *buf, size_t block_count,
                                      off_t block_offset) {
  if (lun >= logical_units_.size()) {
    return ZX_ERR_INVALID_ARGS;
  }

  return logical_units_[lun].BufferRead(buf, block_count, block_offset);
}
}  // namespace ufs_mock_device
}  // namespace ufs
