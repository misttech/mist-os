// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_UPIU_TRANSACTIONS_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_UPIU_TRANSACTIONS_H_

#include <endian.h>

#include <optional>

#include <fbl/algorithm.h>
#include <hwreg/bitfields.h>

#include "src/devices/block/drivers/ufs/transfer_request_descriptor.h"

namespace ufs {

// UPIU requires 64-bit alignment.
constexpr uint16_t kUpiuAlignment = 8;

// for test
namespace ufs_mock_device {
class TransferRequestProcessor;
class QueryRequestProcessor;
class ScsiCommandProcessor;
class UfsMockDevice;
}  // namespace ufs_mock_device

// UFS Specification Version 3.1, section 10.5 "UPIU Transactions".
enum UpiuTransactionCodes {
  kNopOut = 0x00,
  kCommand = 0x01,
  kDataOut = 0x02,
  kTaskManagementRequest = 0x04,
  kQueryRequest = 0x16,
  kNopIn = 0x20,
  kResponse = 0x21,
  kDataIn = 0x22,
  kTaskManagementResponse = 0x24,
  kReadyToTransfer = 0x31,
  kQueryResponse = 0x36,
  kRejectUpiu = 0x3f,
};

enum UpiuCommandSetType {
  kScsi = 0x00,
  kUfsSpecificCommandSet = 0x01,
  kVendorSpecificSet = 0x08,
};

enum UpiuHeaderResponse {
  kTargetSuccess = 0x00,
  kTargetFailure = 0x01,
};

// UFS Specification Version 3.1, section 10.6.2 "Basic Header Format".
struct UpiuHeader {
  // dword 0
  uint8_t trans_type = 0;
  uint8_t flags = 0;
  uint8_t lun = 0;
  uint8_t task_tag = 0;

  // dword 1
  uint8_t cmd_set_type_and_initiator_id = 0;
  uint8_t function = 0;
  uint8_t response = UpiuHeaderResponse::kTargetSuccess;
  uint8_t status = 0;

  // dword 2
  uint8_t ehs_length = 0;
  uint8_t device_infomation = 0;
  uint16_t data_segment_length = 0;  // (Big-endian)

  DEF_SUBFIELD(trans_type, 5, 0, trans_code);
  DEF_SUBBIT(trans_type, 6, dd);
  DEF_SUBBIT(trans_type, 7, hd);

  DEF_SUBBIT(flags, 2, cp);
  DEF_SUBFIELD(flags, 1, 0, task_attribute);

  DEF_SUBFIELD(cmd_set_type_and_initiator_id, 3, 0, command_set_type);
  DEF_SUBFIELD(cmd_set_type_and_initiator_id, 7, 4, initiator_id);
} __PACKED;
static_assert(sizeof(UpiuHeader) == 12, "UpiuHeader struct must be 12 bytes");

class RequestUpiu {
 public:
  virtual ~RequestUpiu() = default;

  // Used to read or write the request descriptor in the UPIU.
  virtual void *GetData() = 0;
  virtual UpiuHeader &GetHeader() = 0;
  // Get the direction of the data transfer to be written to the request descriptor. The
  // TransferRequestDescriptorDataDirection determines whether the target device will read or write
  // the system memory area pointed to by the PRDT.
  virtual TransferRequestDescriptorDataDirection GetDataDirection() const = 0;

  // Get the offset that ResponseUpiu will be written to.
  virtual uint16_t GetResponseOffset() const = 0;
  // Get the length of the ResponseUpiu.
  virtual uint16_t GetResponseLength() const = 0;
};

// UFS Specification Version 3.1, section 10.7.2 "RESPONSE UPIU".
class ResponseUpiu {
 protected:
  struct Data {
    // dword 0 ~ 2
    UpiuHeader header;
    // dword 3
    uint32_t residual_transfer_count = 0;  // (Big-endian)

    // dword 4 ~ 6
    uint8_t reserved[16] = {0};

    // Sense Data
    uint16_t sense_data_len = 0;  // (Big-endian)
    uint8_t sense_data[18] = {0};

    // Add padding to align the kUpiuAlignment.
    uint8_t padding[4] = {0};
  } data_ __PACKED;
  static_assert(sizeof(ResponseUpiu::Data) == 56, "ResponseUpiu struct, must be 56 bytes");
  static_assert(sizeof(ResponseUpiu::Data) % kUpiuAlignment == 0, "UPIU requires 64-bit alignment");

 public:
  DEF_SUBBIT(data_.header.flags, 6, header_flags_o);
  DEF_SUBBIT(data_.header.flags, 5, header_flags_u);
  DEF_SUBBIT(data_.header.flags, 4, header_flags_d);

  explicit ResponseUpiu() { data_.header.set_trans_code(UpiuTransactionCodes::kResponse); }

  UpiuHeader &GetHeader() { return data_.header; }
  uint8_t *GetSenseData() { return data_.sense_data; }

  static uint16_t GetDataSize() { return sizeof(ResponseUpiu::Data); }

 private:
  // for test
  friend class ufs_mock_device::TransferRequestProcessor;
  friend class ufs_mock_device::ScsiCommandProcessor;
  friend class UfsTest;
};

// UFS Specification Version 3.1, section 10.7.1 "COMMAND UPIU".
class CommandUpiu : public RequestUpiu {
 protected:
  struct Data {
    // dword 0 ~ 2
    UpiuHeader header;
    // dword 3
    uint32_t expected_data_transfer_length = 0;  // (Big-endian)

    // dword 4 ~ 6
    uint8_t cdb[16] = {0};
  } data_ __PACKED;
  static_assert(sizeof(CommandUpiu::Data) == 32, "CommandUpiu struct must be 32 bytes");
  static_assert(sizeof(CommandUpiu::Data) % kUpiuAlignment == 0, "UPIU requires 64-bit alignment");

 public:
  DEF_SUBBIT(data_.header.flags, 6, header_flags_r);
  DEF_SUBBIT(data_.header.flags, 5, header_flags_w);
  DEF_SUBBIT(data_.header.flags, 2, header_flags_cp);
  DEF_SUBFIELD(data_.header.flags, 1, 0, header_flags_attr);

  explicit CommandUpiu(UpiuCommandSetType command_set_type) {
    data_.header.set_trans_code(UpiuTransactionCodes::kCommand);
    data_.header.set_command_set_type(command_set_type);
  }
  ~CommandUpiu() override = default;

  void *GetData() override { return static_cast<void *>(&data_); }
  UpiuHeader &GetHeader() override { return data_.header; }

  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kNone;
  }

  uint16_t GetResponseOffset() const override { return GetDataSize(); }
  uint16_t GetResponseLength() const override { return ResponseUpiu::GetDataSize(); }

  void SetExpectedDataTransferLength(uint32_t length) {
    data_.expected_data_transfer_length = htobe32(length);
  }

  static uint16_t GetDataSize() { return sizeof(CommandUpiu::Data); }

  // for test
  CommandUpiu() = default;

 private:
  // for test
  friend class ufs_mock_device::TransferRequestProcessor;
  friend class ufs_mock_device::ScsiCommandProcessor;
  friend class UfsTest;
};

// UFS Specification Version 3.1, section 10.7.7 "TASK MANAGEMENT RESPONSE UPIU".
class TaskManagementResponseUpiu {
 protected:
  struct Data {
    // dword 0 ~ 2
    UpiuHeader header;
    // dword 3
    uint32_t param1 = 0;  // (Big-endian)
    // dword 4
    uint32_t param2 = 0;  // (Big-endian)
    // dword 5 ~ 7
    uint8_t reserved[12] = {0};
  } data_ __PACKED;
  static_assert(sizeof(TaskManagementResponseUpiu::Data) == 32,
                "TaskManagementResponseUpiu struct must be 32 bytes");
  static_assert(sizeof(TaskManagementResponseUpiu::Data) % kUpiuAlignment == 0,
                "UPIU requires 64-bit alignment");

 public:
  explicit TaskManagementResponseUpiu() {
    data_.header.set_trans_code(UpiuTransactionCodes::kTaskManagementResponse);
  }
  static uint16_t GetDataSize() { return sizeof(TaskManagementResponseUpiu::Data); }
};

// UFS Specification Version 3.1, section 10.7.6 "TASK MANAGEMENT REQUEST UPIU".
class TaskManagementRequestUpiu : public RequestUpiu {
 protected:
  struct Data {
    // dword 0 ~ 2
    UpiuHeader header;
    // dword 3
    uint32_t param1 = 0;  // (Big-endian)
    // dword 4
    uint32_t param2 = 0;  // (Big-endian)
    // dword 5
    uint32_t param3 = 0;  // (Big-endian)
    // dword 6 ~ 7
    uint8_t reserved[8] = {0};
  } data_ __PACKED;
  static_assert(sizeof(TaskManagementRequestUpiu::Data) == 32,
                "TaskManagementRequestUpiu struct must be 32 bytes");
  static_assert(sizeof(TaskManagementRequestUpiu::Data) % kUpiuAlignment == 0,
                "UPIU requires 64-bit alignment");

 public:
  explicit TaskManagementRequestUpiu() {
    data_.header.set_trans_code(UpiuTransactionCodes::kTaskManagementRequest);
  }
  ~TaskManagementRequestUpiu() override = default;

  void *GetData() override { return static_cast<void *>(&data_); }
  UpiuHeader &GetHeader() override { return data_.header; }
  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kNone;
  }

  uint16_t GetResponseOffset() const override { return GetDataSize(); }
  uint16_t GetResponseLength() const override { return TaskManagementResponseUpiu::GetDataSize(); }

  static uint16_t GetDataSize() { return sizeof(TaskManagementRequestUpiu::Data); }
};

enum class QueryFunction {
  kStandardReadRequest = 0x01,
  kStandardWriteRequest = 0x81,
};

enum class QueryOpcode {
  kNop = 0,
  kReadDescriptor,
  kWriteDescriptor,
  kReadAttribute,
  kWriteAttribute,
  kReadFlag,
  kSetFlag,
  kClearFlag,
  kToggleFlag,
};

// UFS Specification Version 3.1, section 10.7.9 "QUERY RESPONSE UPIU".
class QueryResponseUpiu {
 protected:
  struct Data {
    // dword 0 ~ 2
    UpiuHeader header;
    // dword 3
    uint8_t opcode = 0;
    uint8_t idn = 0;
    uint8_t index = 0;
    uint8_t selector = 0;

    // dword 4
    uint8_t reserved1[2] = {0};
    uint16_t length = 0;  // (Big-endian)

    // dword 5
    union {
      uint32_t value = 0;  // (Big-endian)
      struct {
        uint8_t reserved2[3];
        uint8_t flag_value;
      };
    };

    // dword 6
    uint8_t reserved3[4] = {0};

    // dword 7
    uint8_t reserved4[4] = {0};
    std::array<uint8_t, 256> command_data = {0};
  } data_ __PACKED;
  static_assert(sizeof(QueryResponseUpiu::Data) == 288,
                "QueryResponseUpiu struct must be 288 bytes");
  static_assert(sizeof(QueryResponseUpiu::Data) % kUpiuAlignment == 0,
                "UPIU requires 64-bit alignment");

 public:
  explicit QueryResponseUpiu(uint8_t query_function, uint8_t query_opcode, uint8_t type) {
    data_.header.set_trans_code(UpiuTransactionCodes::kQueryResponse);
    data_.header.function = query_function;

    data_.opcode = query_opcode;
    data_.idn = type;
  }

  UpiuHeader &GetHeader() { return data_.header; }

  template <typename U>
  constexpr U &GetResponse() {
    static_assert(std::is_base_of<QueryResponseUpiu, U>::value);
    static_assert(sizeof(U) == sizeof(QueryResponseUpiu));
    return *reinterpret_cast<U *>(this);
  }

  static uint16_t GetDataSize() { return sizeof(QueryResponseUpiu::Data); }

  // for test
  uint8_t GetOpcode() const { return data_.opcode; }
  uint8_t GetIdn() const { return data_.idn; }
  uint8_t GetIndex() const { return data_.index; }

 private:
  // for test
  friend class ufs_mock_device::TransferRequestProcessor;
  friend class ufs_mock_device::QueryRequestProcessor;
  friend class UfsTest;
};

// UFS Specification Version 3.1, section 10.7.8 "QUERY REQUEST UPIU".
class QueryRequestUpiu : public RequestUpiu {
 protected:
  struct Data {
    // dword 0 ~ 2
    UpiuHeader header;
    // dword 3
    uint8_t opcode = 0;
    uint8_t idn = 0;
    uint8_t index = 0;
    uint8_t selector = 0;

    // dword 4
    uint8_t reserved1[2] = {0};
    uint16_t length = 0;  // (Big-endian)

    // dword 5
    uint32_t value = 0;  // (Big-endian)

    // dword 6
    uint8_t reserved2[4] = {0};

    // dword 7
    uint8_t reserved3[4] = {0};
    std::array<uint8_t, 256> command_data = {0};
  } data_ __PACKED;
  static_assert(sizeof(QueryRequestUpiu::Data) == 288, "QueryRequestUpiu struct must be 288 bytes");
  static_assert(sizeof(QueryRequestUpiu::Data) % kUpiuAlignment == 0,
                "UPIU requires 64-bit alignment");

 public:
  explicit QueryRequestUpiu(QueryFunction query_function, QueryOpcode query_opcode, uint8_t type,
                            uint8_t index = 0) {
    data_.header.set_trans_code(UpiuTransactionCodes::kQueryRequest);
    data_.header.function = static_cast<uint8_t>(query_function);

    data_.opcode = static_cast<uint8_t>(query_opcode);
    data_.idn = type;
    data_.index = index;
  }
  ~QueryRequestUpiu() override = default;

  void *GetData() override { return static_cast<void *>(&data_); }
  UpiuHeader &GetHeader() override { return data_.header; }
  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kNone;
  }

  uint16_t GetResponseOffset() const override { return GetDataSize(); }
  uint16_t GetResponseLength() const override { return QueryResponseUpiu::GetDataSize(); }

  static uint16_t GetDataSize() { return sizeof(QueryRequestUpiu::Data); }

 private:
  // for test
  friend class ufs_mock_device::TransferRequestProcessor;
  friend class ufs_mock_device::QueryRequestProcessor;
  friend class UfsTest;
};

// UFS Specification Version 3.1, section 10.7.12 "NOP IN UPIU".
class NopInUpiu {
 protected:
  struct Data {
    // dword 0 ~ 2
    UpiuHeader header;
    // dword 3 ~ 7
    uint8_t reserved[20] = {0};
  } data_ __PACKED;
  static_assert(sizeof(NopInUpiu::Data) == 32, "NopInUpiu struct must be 32 bytes");
  static_assert(sizeof(NopInUpiu::Data) % kUpiuAlignment == 0, "UPIU requires 64-bit alignment");

 public:
  explicit NopInUpiu() { data_.header.set_trans_code(UpiuTransactionCodes::kNopIn); }

  UpiuHeader &GetHeader() { return data_.header; }
  static uint16_t GetDataSize() { return sizeof(NopInUpiu::Data); }

 private:
  // for test
  friend class ufs_mock_device::TransferRequestProcessor;
  friend class UfsTest;
};

// UFS Specification Version 3.1, section 10.7.11 "NOP OUT UPIU".
class NopOutUpiu : public RequestUpiu {
 protected:
  struct Data {
    // dword 0 ~ 2
    UpiuHeader header;
    // dword 3 ~ 7
    uint8_t reserved[20] = {0};
  } data_ __PACKED;
  static_assert(sizeof(NopOutUpiu::Data) == 32, "NopOutUpiu struct must be 32 bytes");
  static_assert(sizeof(NopOutUpiu::Data) % kUpiuAlignment == 0, "UPIU requires 64-bit alignment");

 public:
  explicit NopOutUpiu() { data_.header.set_trans_code(UpiuTransactionCodes::kNopOut); }
  ~NopOutUpiu() override = default;

  void *GetData() override { return static_cast<void *>(&data_); }
  UpiuHeader &GetHeader() override { return data_.header; }
  TransferRequestDescriptorDataDirection GetDataDirection() const override {
    return TransferRequestDescriptorDataDirection::kNone;
  }

  uint16_t GetResponseOffset() const override { return GetDataSize(); }
  uint16_t GetResponseLength() const override { return NopInUpiu::GetDataSize(); }

  static uint16_t GetDataSize() { return sizeof(NopOutUpiu::Data); }
} __PACKED;

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_UPIU_TRANSACTIONS_H_
