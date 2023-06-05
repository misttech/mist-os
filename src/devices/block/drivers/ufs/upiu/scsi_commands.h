// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_SCSI_COMMANDS_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_SCSI_COMMANDS_H_

#include <endian.h>
#include <lib/sync/completion.h>

#include <fbl/intrusive_double_list.h>

#include "upiu_transactions.h"

namespace ufs {

// UFS Specification Version 3.1, section 11.3 "Universal Flash Storage SCSI Commands".
enum class ScsiOpcode {
  kFormatUnit = 0x04,
  kInquiry = 0x12,
  kModeSelect10 = 0x55,
  kModeSense10 = 0x5a,
  kPreFetch10 = 0x34,
  kPreFetch16 = 0x90,  // Optional
  kRead6 = 0x08,
  kRead10 = 0x28,
  kRead16 = 0x88,
  kReadBuffer = 0x3c,
  kReadCapacity10 = 0x25,
  kReadCapacity16 = 0x9e,
  kReportLuns = 0xa0,
  kRequestSense = 0x03,
  kSecurityProtocolIn = 0xa2,
  kSecurityProtocolOut = 0xb5,
  kSendDiagnostic = 0x1d,
  kStartStopUnit = 0x1b,
  kSynchronizeCache10 = 0x35,
  kSynchronizeCache16 = 0x91,  // Optional
  kTestUnitReady = 0x00,
  kUnmap = 0x42,
  kVerify10 = 0x2f,
  kWrite6 = 0x0a,
  kWrite10 = 0x2a,
  kWrite16 = 0x8a,  // Optional
  kWriteBuffer = 0x3b,
};

enum ScsiCommandSetStatus {
  kGood = 0x00,
  kCheckCondition = 0x2,
  kConditionMet = 0x4,
  kBusy = 0x8,
  kReservationConflict = 0x18,
  kTaskSetFull = 0x28,
  kAcaActive = 0x30,
  kTaskAborted = 0x40,
};

enum ScsiCommandSenseKey {
  kNoSense = 0x00,
  kRecoveredError = 0x01,
  kNotReady = 0x02,
  kMediumError = 0x03,
  kHardwareError = 0x04,
  kIllegalRequest = 0x05,
  kUnitAttention = 0x06,
  kDataProtect = 0x07,
  kBlankCheck = 0x08,
  kVendorSpecific = 0x09,
  kCopyAborted = 0x0a,
  kAbortedCommand = 0x0b,
  kReserved_1 = 0x0c,
  kVolumeOverflow = 0x0d,
  kMiscompare = 0x0E,
  kReserved_2 = 0x0F,
};

struct ScsiStartStopData {
  uint8_t pwr_cond;
  uint8_t start;
};

struct ScsiUnmapBlockDesc {
  uint64_t lba;
  uint32_t blocks;
  uint8_t rsrvd[4];
};

struct ScsiUnmapParamList {
  uint16_t data_len;
  uint16_t blk_desc_len;
  uint8_t rsrvd[4];
  ScsiUnmapBlockDesc block_desc[];
};

struct ScsiSenseData {
  uint8_t resp_code : 7;
  uint8_t valid : 1;
  uint8_t obsolete;
  uint8_t sense_key : 4;
  uint8_t : 1;
  uint8_t ili : 1;
  uint8_t eom : 1;
  uint8_t filemark : 1;
  uint8_t information[4];
  uint8_t add_sense_len;
  uint8_t cmd_specific_info[4];
  uint8_t asc;
  uint8_t ascq;
  uint8_t fruc;
  uint8_t sense_key_specific : 7;
  uint8_t sksv : 1;
  uint8_t sense_key_specific2[2];
};

class ScsiCommandUpiu;

// TODO(fxbug.dev/124835): Currently, |scsi_xfer| is used internally as an entry in the I/O command
// queue. This will be replaced by an |IoCommand| struct containing a |block_op_t|.
struct scsi_xfer
    : public fbl::DoublyLinkedListable<std::unique_ptr<scsi_xfer>, fbl::NodeOptions::None> {
  std::unique_ptr<ScsiCommandUpiu> upiu;
  uint8_t lun;
  ScsiOpcode op;
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
 public:
  DEF_SUBFIELD(data_.cdb[0], 7, 0, opcode);

  ~ScsiCommandUpiu() override = default;

  explicit ScsiCommandUpiu(ScsiOpcode opcode) : CommandUpiu(UpiuCommandSetType::kScsi) {
    set_opcode(static_cast<uint8_t>(opcode));
  }

  ScsiOpcode GetOpcode() const { return static_cast<ScsiOpcode>(opcode()); }

  // Get the address of first block. Returns std::nullopt if not applicable.
  virtual std::optional<uint32_t> GetStartLba() const { return std::nullopt; }
  // Get the byte size of the PRDT data buffer to send or receive. Returns 0 if there is no buffer
  // to transfer.
  virtual uint32_t GetTransferBytes() const { return 0; }

  // for test
  ScsiCommandUpiu() = default;
  // Because |CopyFrom()| does not copy member variables, classes that inherit from
  // |ScsiCommandUpiu| must copy member variables separately if they are declared.
  template <class T>
  static std::unique_ptr<const T> CopyFrom(const CommandUpiu::Data *data) {
    static_assert(std::is_base_of<CommandUpiu, T>::value);
    T *upiu;
    // |ScsiRead10Upiu| and |ScsiWrite10Upiu| have block size information as a member variable, so
    // we must pass the block size information to the constructor.
    if constexpr (std::is_same_v<ScsiRead10Upiu, T> || std::is_same_v<ScsiWrite10Upiu, T>) {
      // TODO(fxbug.dev/124835): We should pass kMockBlockSize to the constructor
      upiu = new T(4096);
    } else {
      upiu = new T();
    }
    std::memcpy(upiu->GetData(), data, sizeof(CommandUpiu::Data));
    return std::unique_ptr<const T>(upiu);
  }
};

// UFS Specification Version 3.1, section 11.3.6 "READ (10) Command".
class ScsiRead10Upiu : public ScsiCommandUpiu {
 public:
  DEF_SUBFIELD(data_.cdb[1], 7, 5, rd_protect);
  DEF_SUBBIT(data_.cdb[1], 4, dpo);
  DEF_SUBBIT(data_.cdb[1], 3, fua);
  DEF_SUBBIT(data_.cdb[1], 1, fua_nv);
  // uint32_t lba; // cdb 2,3,4,5 (Big-endian)
  DEF_SUBFIELD(data_.cdb[6], 4, 0, group_num);
  // uint16_t transfer_len;  // cdb 7,8 (Big-endian)
  DEF_SUBFIELD(data_.cdb[9], 7, 0, control);

  explicit ScsiRead10Upiu(uint32_t start, uint16_t length, uint32_t block_size, bool fua,
                          uint8_t group_num)
      : ScsiCommandUpiu(ScsiOpcode::kRead10), block_size_(block_size) {
    set_header_flags_r(1);

    uint32_t *lba = reinterpret_cast<uint32_t *>(&data_.cdb[2]);  // cdb 2,3,4,5
    UnalignedStore32(lba, htobe32(start));

    uint16_t *transfer_len = reinterpret_cast<uint16_t *>(&data_.cdb[7]);  // cdb 7,8
    UnalignedStore16(transfer_len, htobe16(length));

    set_fua(fua);
    set_fua_nv(0);

    if (group_num) {
      set_group_num(group_num);
    }
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kDeviceToHost;
  }

  std::optional<uint32_t> GetStartLba() const override {
    const uint32_t *lba = reinterpret_cast<const uint32_t *>(&data_.cdb[2]);  // cdb 2, 3, 4, 5
    return betoh32(UnalignedLoad32(lba));
  }
  uint32_t GetTransferBytes() const override {
    const uint16_t *transfer_len = reinterpret_cast<const uint16_t *>(&data_.cdb[7]);  // cdb 7, 8
    return betoh16(UnalignedLoad16(transfer_len)) * block_size_;
  }

 private:
  const uint32_t block_size_;
  // for test
  friend class ScsiCommandUpiu;
  explicit ScsiRead10Upiu(uint32_t block_size) : block_size_(block_size) {}
};

// UFS Specification Version 3.1, section 11.3.10 "START STOP UNIT Command".
class ScsiStartStopUnitUpiu : public ScsiCommandUpiu {
 public:
  DEF_SUBBIT(data_.cdb[1], 0, immed);
  DEF_SUBFIELD(data_.cdb[3], 3, 0, power_condition_modifier);
  DEF_SUBFIELD(data_.cdb[4], 7, 4, power_conditions);
  DEF_SUBBIT(data_.cdb[4], 2, no_flush);
  DEF_SUBBIT(data_.cdb[4], 1, loej);
  DEF_SUBBIT(data_.cdb[4], 0, start);
  DEF_SUBFIELD(data_.cdb[9], 7, 0, control);

  explicit ScsiStartStopUnitUpiu(ScsiStartStopData *data)
      : ScsiCommandUpiu(ScsiOpcode::kStartStopUnit) {
    set_power_conditions(data->pwr_cond);
    set_start(data->start);
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kNone;
  }

 private:
  // for test
  friend class ScsiCommandUpiu;
  ScsiStartStopUnitUpiu() = default;
};

// UFS Specification Version 3.1, section 11.3.11 "TEST UNIT READY Command".
class ScsiTestUnitReadyUpiu : public ScsiCommandUpiu {
 public:
  DEF_SUBFIELD(data_.cdb[5], 7, 0, control);

  explicit ScsiTestUnitReadyUpiu() : ScsiCommandUpiu(ScsiOpcode::kTestUnitReady) { set_control(0); }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kNone;
  }

 private:
  // for test
  friend class ScsiCommandUpiu;
};

// UFS Specification Version 3.1, section 11.3.15 "WRITE (10) Command".
class ScsiWrite10Upiu : public ScsiCommandUpiu {
 public:
  DEF_SUBFIELD(data_.cdb[1], 7, 5, wr_protect);
  DEF_SUBBIT(data_.cdb[1], 4, dpo);
  DEF_SUBBIT(data_.cdb[1], 3, fua);
  DEF_SUBBIT(data_.cdb[1], 1, fua_nv);
  // uint32_t lba;  // cdb 2,3,4,5 (Big-endian)
  DEF_SUBFIELD(data_.cdb[6], 4, 0, group_num);
  // uint16_t transfer_len;  // cdb 7,8 (Big-endian)
  DEF_SUBFIELD(data_.cdb[9], 7, 0, control);

  explicit ScsiWrite10Upiu(uint32_t start, uint16_t length, uint32_t block_size, bool fua,
                           uint8_t group_num)
      : ScsiCommandUpiu(ScsiOpcode::kWrite10), block_size_(block_size) {
    set_header_flags_w(1);

    uint32_t *lba = reinterpret_cast<uint32_t *>(&data_.cdb[2]);  // cdb 2, 3, 4, 5
    UnalignedStore32(lba, htobe32(start));

    uint16_t *transfer_len = reinterpret_cast<uint16_t *>(&data_.cdb[7]);  // cdb 7, 8
    UnalignedStore16(transfer_len, htobe16(length));

    set_fua(fua);
    set_fua_nv(0);

    if (group_num) {
      set_group_num(group_num);
    }
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kHostToDevice;
  }

  std::optional<uint32_t> GetStartLba() const override {
    const uint32_t *lba = reinterpret_cast<const uint32_t *>(&data_.cdb[2]);  // cdb 2, 3, 4, 5
    return betoh32(UnalignedLoad32(lba));
  }
  uint32_t GetTransferBytes() const override {
    const uint16_t *transfer_len = reinterpret_cast<const uint16_t *>(&data_.cdb[7]);  // cdb 7, 8
    return betoh16(UnalignedLoad16(transfer_len)) * block_size_;
  }

 private:
  const uint32_t block_size_;
  // for test
  friend class ScsiCommandUpiu;
  explicit ScsiWrite10Upiu(uint32_t block_size) : block_size_(block_size) {}
};

// UFS Specification Version 3.1, section 11.3.17 "REQUEST SENSE Command".
class ScsiRequestSenseUpiu : public ScsiCommandUpiu {
 public:
  DEF_SUBBIT(data_.cdb[1], 0, desc);
  DEF_SUBFIELD(data_.cdb[4], 7, 0, allocation_length);
  DEF_SUBFIELD(data_.cdb[5], 7, 0, control);

  explicit ScsiRequestSenseUpiu() : ScsiCommandUpiu(ScsiOpcode::kRequestSense) {
    set_desc(0);
    set_allocation_length(sizeof(ScsiSenseData));
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kDeviceToHost;
  }

  uint32_t GetTransferBytes() const override { return allocation_length(); }
};

// UFS Specification Version 3.1, section 11.3.21 "SECURITY PROTOCOL IN Command".
class ScsiSecurityProtocolInUpiu : public ScsiCommandUpiu {
 public:
  DEF_SUBFIELD(data_.cdb[1], 7, 0, security_protocol);
  // uint16_t security_protocol_specific; // cdb 2,3 (Big-endian)
  DEF_SUBBIT(data_.cdb[4], 7, inc_512);
  // uint32_t allocation_length; // cdb 6,7,8,9 (Big-endian)
  DEF_SUBFIELD(data_.cdb[11], 7, 0, control);

  explicit ScsiSecurityProtocolInUpiu(uint16_t length)
      : ScsiCommandUpiu(ScsiOpcode::kSecurityProtocolIn) {
    set_security_protocol(0xec);

    uint16_t *security_protocol_specific = reinterpret_cast<uint16_t *>(&data_.cdb[2]);  // cdb 2,3
    UnalignedStore16(security_protocol_specific, htobe16(0x01));

    uint32_t *allocation_length = reinterpret_cast<uint32_t *>(&data_.cdb[6]);  // cdb 6,7,8,9
    UnalignedStore32(allocation_length, htobe32(length));
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kDeviceToHost;
  }

  uint32_t GetTransferBytes() const override {
    const uint32_t *allocation_length =
        reinterpret_cast<const uint32_t *>(&data_.cdb[6]);  // cdb 6,7,8,9
    return betoh32(UnalignedLoad32(allocation_length)) * (inc_512() ? 512 : 1);
  }

 private:
  // for test
  friend class ScsiCommandUpiu;
  ScsiSecurityProtocolInUpiu() = default;
};

// UFS Specification Version 3.1, section 11.3.22 "SECURITY PROTOCOL OUT Command".
class ScsiSecurityProtocolOutUpiu : public ScsiCommandUpiu {
 public:
  DEF_SUBFIELD(data_.cdb[1], 7, 0, security_protocol);
  // uint16_t security_protocol_specific; // cdb 2,3 (Big-endian)
  DEF_SUBBIT(data_.cdb[4], 7, inc_512);
  // uint32_t allocation_length; // cdb 6,7,8,9 (Big-endian)
  DEF_SUBFIELD(data_.cdb[11], 7, 0, control);

  explicit ScsiSecurityProtocolOutUpiu(uint16_t length)
      : ScsiCommandUpiu(ScsiOpcode::kSecurityProtocolOut) {
    // |security_protocol| is a field that indicates which security protocol is used. For UFS, use
    // 0xec, which is the JEDEC UFS application.
    set_security_protocol(0xec);

    uint16_t *security_protocol_specific = reinterpret_cast<uint16_t *>(&data_.cdb[2]);  // cdb 2,3
    // |security_protocol_specific| field specifies the RPMB Protocol ID. 0x01 means RPMB Region 0.
    // See the UFS Specification Version 3.1, section 12.4.5.1 CDB format of SECURITY PROTOCOL 4867
    // IN/OUT commands
    UnalignedStore16(security_protocol_specific, htobe16(0x01));

    uint32_t *allocation_length = reinterpret_cast<uint32_t *>(&data_.cdb[6]);  // cdb 6,7,8,9
    UnalignedStore32(allocation_length, htobe32(length));
  }
  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kHostToDevice;
  }

  uint32_t GetTransferBytes() const override {
    const uint32_t *allocation_length =
        reinterpret_cast<const uint32_t *>(&data_.cdb[6]);  // cdb 6,7,8,9
    return betoh32(UnalignedLoad32(allocation_length) * (inc_512() ? 512 : 1));
  }

 private:
  // for test
  friend class ScsiCommandUpiu;
  ScsiSecurityProtocolOutUpiu() = default;
};

// UFS Specification Version 3.1, section 11.3.24 "SYNCHRONIZE CACHE (10) Command".
class ScsiSynchronizeCache10Upiu : public ScsiCommandUpiu {
 public:
  DEF_SUBBIT(data_.cdb[1], 1, immed);
  DEF_SUBBIT(data_.cdb[1], 2, sync_nv);
  // uint32_t lba;  // cdb 2,3,4,5 (Big-endian)
  DEF_SUBFIELD(data_.cdb[6], 4, 0, group_number);
  // uint16_t number_of_logical_blocks;  // cdb 7,8 (Big-endian)
  DEF_SUBFIELD(data_.cdb[9], 7, 0, control);

  explicit ScsiSynchronizeCache10Upiu(uint32_t start, uint16_t length)
      : ScsiCommandUpiu(ScsiOpcode::kSynchronizeCache10) {
    uint32_t *lba = reinterpret_cast<uint32_t *>(&data_.cdb[2]);  // cdb 2,3,4,5
    UnalignedStore32(lba, htobe32(start));

    uint16_t *number_of_logical_blocks = reinterpret_cast<uint16_t *>(&data_.cdb[7]);  // cdb 7, 8
    UnalignedStore16(number_of_logical_blocks, htobe16(length));
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kNone;
  }

 private:
  // for test
  friend class ScsiCommandUpiu;
  ScsiSynchronizeCache10Upiu() = default;
};

// UFS Specification Version 3.1, section 11.3.26 "UNMAP Command".
class ScsiUnmapUpiu : public ScsiCommandUpiu {
 public:
  DEF_SUBBIT(data_.cdb[1], 0, anchor);
  DEF_SUBFIELD(data_.cdb[6], 4, 0, group_num);
  // uint16_t parameter_list_length; // cdb 7, 8 (Big-endian)
  DEF_SUBFIELD(data_.cdb[9], 7, 0, control);

  explicit ScsiUnmapUpiu(uint16_t param_len) : ScsiCommandUpiu(ScsiOpcode::kUnmap) {
    set_anchor(0);
    set_group_num(0);

    uint16_t *parameter_list_length = reinterpret_cast<uint16_t *>(&data_.cdb[7]);  // cdb 7, 8
    UnalignedStore16(parameter_list_length, htobe16(param_len));
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kHostToDevice;
  }

  uint32_t GetTransferBytes() const override {
    const uint16_t *parameter_list_length =
        reinterpret_cast<const uint16_t *>(&data_.cdb[7]);  // cdb 7, 8
    return betoh16(UnalignedLoad16(parameter_list_length));
  }

 private:
  // for test
  friend class ScsiCommandUpiu;
  ScsiUnmapUpiu() = default;
};

// UFS Specification Version 3.1, section 11.3.28 "WRITE BUFFER Command".
class ScsiWriteBufferUpiu : public ScsiCommandUpiu {
 public:
  enum Mode {
    kVendorSpecific = 0x01,
    kData = 0x02,
    kDownloadMicrocode = 0x0e,
  };

  DEF_SUBFIELD(data_.cdb[1], 4, 0, mode);
  DEF_SUBFIELD(data_.cdb[2], 4, 0, buffer_id);
  // uint32_t buffer_offset : 24; // cdb 3,4,5 (Big-endian)
  // uint32_t parameter_list_length : 24;  // cdb 6,7,8 (Big-endian)
  DEF_SUBFIELD(data_.cdb[9], 7, 0, control);

  explicit ScsiWriteBufferUpiu(uint32_t length) : ScsiCommandUpiu(ScsiOpcode::kWriteBuffer) {
    // For now, the WriteBuffer command will only be used for FW downloads.
    set_mode(Mode::kDownloadMicrocode);
    set_buffer_id(0);

    uint24_t *buffer_offset = reinterpret_cast<uint24_t *>(&data_.cdb[3]);  // cdb 3,4,5
    UnalignedStore24(buffer_offset, htobe24(0));

    uint24_t *parameter_list_length = reinterpret_cast<uint24_t *>(&data_.cdb[6]);  // cdb 6,7,8
    UnalignedStore24(parameter_list_length, htobe24(length));                       // in bytes
  }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kHostToDevice;
  }

  uint32_t GetTransferBytes() const override {
    const uint24_t *parameter_list_length =
        reinterpret_cast<const uint24_t *>(&data_.cdb[6]);  // cdb 6,7,8
    return betoh24(UnalignedLoad24(parameter_list_length));
  }

 private:
  // for test
  friend class ScsiCommandUpiu;
  ScsiWriteBufferUpiu() = default;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_SCSI_COMMANDS_H_
