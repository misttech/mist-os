// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_UFS_MOCK_DEVICE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_UFS_MOCK_DEVICE_H_

#include <endian.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>

#include <bitset>

#include "fake-dma-handler.h"
#include "query-request-processor.h"
#include "register-mmio-processor.h"
#include "scsi-command-processor.h"
#include "src/devices/block/drivers/ufs/ufs.h"
#include "transfer-request-processor.h"
#include "uiccmd-processor.h"

namespace ufs {
namespace ufs_mock_device {

constexpr uint64_t kMockBlockSizeShift = 12;
constexpr uint64_t kMockBlockSize = (1 << kMockBlockSizeShift);
constexpr uint64_t kMockTotalDeviceCapacity = (1 << 24);  // 16MB

class FakeRegisters final {
 public:
  FakeRegisters() { std::memset(registers_, 0, RegisterMap::kRegisterSize); }

  template <typename T>
  T Read(zx_off_t offs) const {
    ZX_ASSERT(offs + sizeof(T) <= RegisterMap::kRegisterSize);
    return *reinterpret_cast<const T *>(registers_ + offs);
  }

  template <typename T>
  void Write(T val, zx_off_t offs) {
    ZX_ASSERT(offs + sizeof(T) <= RegisterMap::kRegisterSize);
    *reinterpret_cast<T *>(registers_ + offs) = val;
  }

 private:
  uint8_t registers_[RegisterMap::kRegisterSize];
};

// Simulates a logical unit and its contents.
class UfsLogicalUnit {
 public:
  UfsLogicalUnit();

  zx_status_t Enable(uint8_t lun, uint64_t block_count);

  zx_status_t BufferWrite(const void *buf, size_t block_count, off_t block_offset);
  zx_status_t BufferRead(void *buf, size_t block_count, off_t block_offset);
  UnitDescriptor &GetUnitDesc() { return unit_desc_; }

 private:
  uint64_t block_count_;
  std::vector<uint8_t> buffer_;
  UnitDescriptor unit_desc_;
};

class UfsMockDevice {
 public:
  static constexpr uint32_t kNutrs = 32;
  static constexpr uint32_t kNutmrs = 8;

  explicit UfsMockDevice(zx::interrupt irq);
  UfsMockDevice(const UfsMockDevice &) = delete;
  UfsMockDevice &operator=(const UfsMockDevice &) = delete;
  UfsMockDevice(const UfsMockDevice &&) = delete;
  UfsMockDevice &operator=(const UfsMockDevice &&) = delete;
  ~UfsMockDevice() = default;

  fdf::MmioBuffer GetMmioBuffer() {
    return fdf::MmioBuffer{
        mmio_buffer_t{
            .vaddr = FakeMmioPtr(&registers_),
            .offset = 0,
            .size = RegisterMap::kRegisterSize,
            .vmo = ZX_HANDLE_INVALID,
        },
        &RegisterMmioProcessor::GetMmioOps(),
        this,
    };
  }

  zx_status_t AddLun(uint8_t lun);

  zx::bti GetFakeBti() { return dma_handler_.DuplicateFakeBti(); }
  zx::result<zx_vaddr_t> MapDmaPaddr(zx_paddr_t paddr) { return dma_handler_.PhysToVirt(paddr); }

  void TriggerInterrupt() { irq_.trigger(0, zx::clock::get_monotonic()); }

  zx_status_t BufferWrite(uint8_t lun, const void *buf, size_t block_count, off_t block_offset);
  zx_status_t BufferRead(uint8_t lun, void *buf, size_t block_count, off_t block_offset);

  FakeRegisters *GetRegisters() { return &registers_; }
  DeviceDescriptor &GetDeviceDesc() { return device_desc_; }
  GeometryDescriptor &GetGeometryDesc() { return geometry_desc_; }
  void SetAttribute(Attributes idn, uint32_t value) {
    attributes_[static_cast<size_t>(idn)] = value;
  }
  void SetFlag(Flags idn, bool value) { flags_[static_cast<size_t>(idn)] = value; }
  uint32_t GetAttribute(Attributes idn) const { return attributes_[static_cast<size_t>(idn)]; }
  bool GetFlag(Flags idn) const { return flags_[static_cast<size_t>(idn)]; }

  UfsLogicalUnit &GetLogicalUnit(uint8_t lun) { return logical_units_[lun]; }
  RegisterMmioProcessor &GetRegisterMmioProcessor() { return register_mmio_processor_; }
  UicCmdProcessor &GetUicCmdProcessor() { return uiccmd_processor_; }
  TransferRequestProcessor &GetTransferRequestProcessor() { return transfer_request_processor_; }
  QueryRequestProcessor &GetQueryRequestProcessor() { return query_request_processor_; }
  ScsiCommandProcessor &GetScsiCommandProcessor() { return scsi_command_processor_; }

 private:
  std::array<UfsLogicalUnit, kMaxLun> logical_units_;
  DeviceDescriptor device_desc_;
  GeometryDescriptor geometry_desc_;
  std::array<uint32_t, static_cast<size_t>(Attributes::kAttributeCount)> attributes_;
  std::array<bool, static_cast<size_t>(Flags::kFlagCount)> flags_;

  zx::interrupt irq_;
  FakeDmaHandler dma_handler_;
  FakeRegisters registers_;
  RegisterMmioProcessor register_mmio_processor_;
  UicCmdProcessor uiccmd_processor_;
  TransferRequestProcessor transfer_request_processor_;
  QueryRequestProcessor query_request_processor_;
  ScsiCommandProcessor scsi_command_processor_;
};

}  // namespace ufs_mock_device
}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_UFS_MOCK_DEVICE_H_
