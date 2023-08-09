// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_SCSI_COMMANDS_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_SCSI_COMMANDS_H_

#include <endian.h>
#include <lib/scsi/controller.h>
#include <lib/sync/completion.h>

#include <fbl/intrusive_double_list.h>

#include "upiu_transactions.h"

namespace ufs {

class ScsiCommandUpiu;

// TODO(fxbug.dev/124835): Currently, |scsi_xfer| is used internally as an entry in the I/O command
// queue. This will be replaced by an |IoCommand| struct containing a |block_op_t|.
struct scsi_xfer
    : public fbl::DoublyLinkedListable<std::unique_ptr<scsi_xfer>, fbl::NodeOptions::None> {
  std::unique_ptr<ScsiCommandUpiu> upiu;
  uint8_t lun;
  scsi::Opcode op;
  uint64_t start_lba;
  uint64_t block_count;
  std::array<zx_paddr_t, 2> buffer_phys;
  sync_completion_t *done;
  sync_completion_t local_event;
  zx_status_t status;
  uint32_t block_size;
};

using uint24_t = struct {
  uint8_t byte[3];
};

inline uint24_t htobe24(uint32_t unsigned_int_32) {
  ZX_ASSERT_MSG(unsigned_int_32 <= 0xffffff, "Input %08xh is greater than 24 bits.",
                unsigned_int_32);
  uint24_t big_24{
      static_cast<uint8_t>((unsigned_int_32 >> 16) & 0xff),
      static_cast<uint8_t>((unsigned_int_32 >> 8) & 0xff),
      static_cast<uint8_t>(unsigned_int_32 & 0xff),
  };
  return big_24;
}

inline uint32_t betoh24(uint24_t big_24) {
  return big_24.byte[0] << 16 | big_24.byte[1] << 8 | big_24.byte[2];
}

inline uint16_t UnalignedLoad16(const uint16_t *ptr) {
  uint16_t value;
  memcpy(&value, ptr, sizeof(uint16_t));
  return value;
}
inline uint24_t UnalignedLoad24(const uint24_t *ptr) {
  uint24_t value;
  memcpy(&value, &ptr, sizeof(uint24_t));
  return value;
}
inline uint32_t UnalignedLoad32(const uint32_t *ptr) {
  uint32_t value;
  memcpy(&value, ptr, sizeof(uint32_t));
  return value;
}

inline void UnalignedStore16(uint16_t *ptr, uint16_t value) {
  memcpy(ptr, &value, sizeof(uint16_t));
}
inline void UnalignedStore24(uint24_t *ptr, uint24_t value) {
  memcpy(ptr, &value, sizeof(uint24_t));
}
inline void UnalignedStore32(uint32_t *ptr, uint32_t value) {
  memcpy(ptr, &value, sizeof(uint32_t));
}

class ScsiRead10Upiu;
class ScsiWrite10Upiu;

// UFS Specification Version 3.1, section 11.3 "Universal Flash Storage SCSI Commands".
class ScsiCommandUpiu : public CommandUpiu {
 protected:
  struct ScsiCommonCDB {
    scsi::Opcode opcode;
  } __PACKED;

 public:
  explicit ScsiCommandUpiu(scsi::Opcode opcode) : CommandUpiu(UpiuCommandSetType::kScsi) {
    scsi_cdb_->opcode = opcode;
  }

  ~ScsiCommandUpiu() override = default;

  scsi::Opcode GetOpcode() const { return scsi_cdb_->opcode; }

  // Get the address of first block. Returns std::nullopt if not applicable.
  virtual std::optional<uint32_t> GetStartLba() const { return std::nullopt; }
  // Get the byte size of the PRDT data buffer to send or receive. Returns 0 if there is no buffer
  // to transfer.
  virtual uint32_t GetTransferBytes() const { return 0; }

  // for test
  explicit ScsiCommandUpiu(const CommandUpiuData &data) {
    std::memcpy(GetData(), &data, sizeof(CommandUpiuData));
  }

 private:
  ScsiCommonCDB *scsi_cdb_ = reinterpret_cast<ScsiCommonCDB *>(GetData<CommandUpiuData>()->cdb);
};

// UFS Specification Version 3.1, section 11.3.6 "READ (10) Command".
class ScsiRead10Upiu : public ScsiCommandUpiu {
 public:
  explicit ScsiRead10Upiu(uint32_t start, uint16_t length, uint32_t block_size, bool fua,
                          uint8_t group_num)
      : ScsiCommandUpiu(scsi::Opcode::READ_10), block_size_(block_size) {
    GetData<CommandUpiuData>()->set_header_flags_r(1);

    UnalignedStore32(&scsi_cdb_->logical_block_address, htobe32(start));
    UnalignedStore16(&scsi_cdb_->transfer_length, htobe16(length));

    scsi_cdb_->set_force_unit_access(fua);
    scsi_cdb_->set_force_unit_access_nv_cache(0);

    if (group_num) {
      scsi_cdb_->set_group_number(group_num);
    }
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kDeviceToHost;
  }

  std::optional<uint32_t> GetStartLba() const override {
    return betoh32(UnalignedLoad32(&scsi_cdb_->logical_block_address));
  }
  uint32_t GetTransferBytes() const override {
    return betoh16(UnalignedLoad16(&scsi_cdb_->transfer_length)) * block_size_;
  }

  // for test
  explicit ScsiRead10Upiu(const CommandUpiuData &data, uint32_t block_size)
      : ScsiCommandUpiu(data), block_size_(block_size) {}

 private:
  scsi::Read10CDB *scsi_cdb_ = reinterpret_cast<scsi::Read10CDB *>(GetData<CommandUpiuData>()->cdb);
  const uint32_t block_size_;
};

// UFS Specification Version 3.1, section 11.3.10 "START STOP UNIT Command".
class ScsiStartStopUnitUpiu : public ScsiCommandUpiu {
 public:
  explicit ScsiStartStopUnitUpiu(uint8_t power_condition, uint8_t start)
      : ScsiCommandUpiu(scsi::Opcode::START_STOP_UNIT) {
    scsi_cdb_->set_power_conditions(power_condition);
    scsi_cdb_->set_start(start);
  }

 private:
  scsi::StartStopCDB *scsi_cdb_ =
      reinterpret_cast<scsi::StartStopCDB *>(GetData<CommandUpiuData>()->cdb);
};

// UFS Specification Version 3.1, section 11.3.11 "TEST UNIT READY Command".
class ScsiTestUnitReadyUpiu : public ScsiCommandUpiu {
 public:
  explicit ScsiTestUnitReadyUpiu() : ScsiCommandUpiu(scsi::Opcode::TEST_UNIT_READY) {
    scsi_cdb_->control = 0;
  }

 private:
  scsi::TestUnitReadyCDB *scsi_cdb_ =
      reinterpret_cast<scsi::TestUnitReadyCDB *>(GetData<CommandUpiuData>()->cdb);
};

// UFS Specification Version 3.1, section 11.3.15 "WRITE (10) Command".
class ScsiWrite10Upiu : public ScsiCommandUpiu {
 public:
  explicit ScsiWrite10Upiu(uint32_t start, uint16_t length, uint32_t block_size, bool fua,
                           uint8_t group_num)
      : ScsiCommandUpiu(scsi::Opcode::WRITE_10), block_size_(block_size) {
    GetData<CommandUpiuData>()->set_header_flags_w(1);

    UnalignedStore32(&scsi_cdb_->logical_block_address, htobe32(start));
    UnalignedStore16(&scsi_cdb_->transfer_length, htobe16(length));

    scsi_cdb_->set_force_unit_access(fua);
    scsi_cdb_->set_force_unit_access_nv_cache(0);

    if (group_num) {
      scsi_cdb_->set_group_number(group_num);
    }
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kHostToDevice;
  }

  std::optional<uint32_t> GetStartLba() const override {
    return betoh32(UnalignedLoad32(&scsi_cdb_->logical_block_address));
  }
  uint32_t GetTransferBytes() const override {
    return betoh16(UnalignedLoad16(&scsi_cdb_->transfer_length)) * block_size_;
  }

  // for test
  explicit ScsiWrite10Upiu(const CommandUpiuData &data, uint32_t block_size)
      : ScsiCommandUpiu(data), block_size_(block_size) {}

 private:
  scsi::Write10CDB *scsi_cdb_ =
      reinterpret_cast<scsi::Write10CDB *>(GetData<CommandUpiuData>()->cdb);
  const uint32_t block_size_;
};

// UFS Specification Version 3.1, section 11.3.17 "REQUEST SENSE Command".
class ScsiRequestSenseUpiu : public ScsiCommandUpiu {
 public:
  explicit ScsiRequestSenseUpiu() : ScsiCommandUpiu(scsi::Opcode::REQUEST_SENSE) {
    scsi_cdb_->desc = 0;
    scsi_cdb_->allocation_length = sizeof(scsi::FixedFormatSenseDataHeader);
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kDeviceToHost;
  }

  uint32_t GetTransferBytes() const override { return scsi_cdb_->allocation_length; }

  // for test
  uint8_t desc() const { return scsi_cdb_->desc; }
  uint8_t allocation_length() const { return scsi_cdb_->allocation_length; }

  // for test
  explicit ScsiRequestSenseUpiu(const CommandUpiuData &data) : ScsiCommandUpiu(data) {}

 private:
  scsi::RequestSenseCDB *scsi_cdb_ =
      reinterpret_cast<scsi::RequestSenseCDB *>(GetData<CommandUpiuData>()->cdb);
};

// UFS Specification Version 3.1, section 11.3.21 "SECURITY PROTOCOL IN Command".
class ScsiSecurityProtocolInUpiu : public ScsiCommandUpiu {
 public:
  explicit ScsiSecurityProtocolInUpiu(uint16_t length)
      : ScsiCommandUpiu(scsi::Opcode::SECURITY_PROTOCOL_IN) {
    // 0xec: JEDEC UFS application
    scsi_cdb_->security_protocol = 0xec;

    UnalignedStore16(&scsi_cdb_->security_protocol_specific, htobe16(0x01));
    UnalignedStore32(&scsi_cdb_->allocation_length, htobe32(length));
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kDeviceToHost;
  }

  uint32_t GetTransferBytes() const override {
    return betoh32(UnalignedLoad32(&scsi_cdb_->allocation_length)) *
           (scsi_cdb_->inc_512() ? 512 : 1);
  }

 private:
  scsi::SecurityProtocolInCDB *scsi_cdb_ =
      reinterpret_cast<scsi::SecurityProtocolInCDB *>(GetData<CommandUpiuData>()->cdb);
};

// UFS Specification Version 3.1, section 11.3.22 "SECURITY PROTOCOL OUT Command".
class ScsiSecurityProtocolOutUpiu : public ScsiCommandUpiu {
 public:
  explicit ScsiSecurityProtocolOutUpiu(uint16_t length)
      : ScsiCommandUpiu(scsi::Opcode::SECURITY_PROTOCOL_OUT) {
    // |security_protocol| is a field that indicates which security protocol is used. For UFS, use
    // 0xec, which is the JEDEC UFS application.
    scsi_cdb_->security_protocol = 0xec;

    // |security_protocol_specific| field specifies the RPMB Protocol ID. 0x01 means RPMB Region 0.
    // See the UFS Specification Version 3.1, section 12.4.5.1 CDB format of SECURITY PROTOCOL 4867
    // IN/OUT commands
    UnalignedStore16(&scsi_cdb_->security_protocol_specific, htobe16(0x01));
    UnalignedStore32(&scsi_cdb_->transfer_length, htobe32(length));
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kHostToDevice;
  }

  uint32_t GetTransferBytes() const override {
    return betoh32(UnalignedLoad32(&scsi_cdb_->transfer_length)) * (scsi_cdb_->inc_512() ? 512 : 1);
  }

 private:
  scsi::SecurityProtocolOutCDB *scsi_cdb_ =
      reinterpret_cast<scsi::SecurityProtocolOutCDB *>(GetData<CommandUpiuData>()->cdb);
};

// UFS Specification Version 3.1, section 11.3.24 "SYNCHRONIZE CACHE (10) Command".
class ScsiSynchronizeCache10Upiu : public ScsiCommandUpiu {
 public:
  explicit ScsiSynchronizeCache10Upiu(uint32_t start, uint16_t length)
      : ScsiCommandUpiu(scsi::Opcode::SYNCHRONIZE_CACHE_10) {
    UnalignedStore32(&scsi_cdb_->logical_block_address, htobe32(start));
    UnalignedStore16(&scsi_cdb_->num_blocks, htobe16(length));
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kNone;
  }

 private:
  scsi::SynchronizeCache10CDB *scsi_cdb_ =
      reinterpret_cast<scsi::SynchronizeCache10CDB *>(GetData<CommandUpiuData>()->cdb);
};

// UFS Specification Version 3.1, section 11.3.26 "UNMAP Command".
class ScsiUnmapUpiu : public ScsiCommandUpiu {
 public:
  explicit ScsiUnmapUpiu(uint16_t param_len) : ScsiCommandUpiu(scsi::Opcode::UNMAP) {
    scsi_cdb_->set_anchor(0);
    scsi_cdb_->set_group_number(0);

    UnalignedStore16(&scsi_cdb_->parameter_list_length, htobe16(param_len));
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kHostToDevice;
  }

  uint32_t GetTransferBytes() const override {
    return betoh16(UnalignedLoad16(&scsi_cdb_->parameter_list_length));
  }

 private:
  scsi::UnmapCDB *scsi_cdb_ = reinterpret_cast<scsi::UnmapCDB *>(GetData<CommandUpiuData>()->cdb);
};

// UFS Specification Version 3.1, section 11.3.28 "WRITE BUFFER Command".
class ScsiWriteBufferUpiu : public ScsiCommandUpiu {
 public:
  enum Mode {
    kVendorSpecific = 0x01,
    kData = 0x02,
    kDownloadMicrocode = 0x0e,
  };

  explicit ScsiWriteBufferUpiu(uint32_t length) : ScsiCommandUpiu(scsi::Opcode::WRITE_BUFFER) {
    // For now, the WriteBuffer command will only be used for FW downloads.
    scsi_cdb_->set_mode(Mode::kDownloadMicrocode);
    scsi_cdb_->buffer_id = 0;

    uint24_t *buffer_offset = reinterpret_cast<uint24_t *>(scsi_cdb_->buffer_offset);  // cdb 3,4,5
    UnalignedStore24(buffer_offset, htobe24(0));

    uint24_t *parameter_list_length =
        reinterpret_cast<uint24_t *>(scsi_cdb_->parameter_list_length);  // cdb 6,7,8
    UnalignedStore24(parameter_list_length, htobe24(length));            // in bytes
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kHostToDevice;
  }

  uint32_t GetTransferBytes() const override {
    const uint24_t *parameter_list_length =
        reinterpret_cast<uint24_t *>(scsi_cdb_->parameter_list_length);  // cdb 6,7,8
    return betoh24(UnalignedLoad24(parameter_list_length));
  }

 private:
  scsi::WriteBufferCDB *scsi_cdb_ =
      reinterpret_cast<scsi::WriteBufferCDB *>(GetData<CommandUpiuData>()->cdb);
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_SCSI_COMMANDS_H_
