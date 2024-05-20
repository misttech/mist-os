// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cstdint>
#include <memory>

#include "src/devices/block/drivers/ufs/transfer_request_descriptor.h"
#include "src/devices/block/drivers/ufs/upiu/descriptors.h"
#include "src/devices/block/drivers/ufs/upiu/upiu_transactions.h"
#include "unit-lib.h"
#include "zircon/errors.h"

namespace ufs {
using namespace ufs_mock_device;

using RequestProcessorTest = UfsTest;

TEST_F(RequestProcessorTest, RequestListCreate) {
  auto request_list =
      RequestList::Create(mock_device_->GetFakeBti().borrow(), kMaxTransferRequestListSize,
                          sizeof(TransferRequestDescriptor));

  // Check list size
  ASSERT_EQ(request_list->GetSlotCount(), kMaxTransferRequestListSize);

  // Check request list slots
  for (uint8_t i = 0; i < kMaxTransferRequestListSize; ++i) {
    auto &slot = request_list->GetSlot(i);
    ASSERT_EQ(slot.state, SlotState::kFree);
    ASSERT_TRUE(slot.command_descriptor_io);
  }
}

TEST_F(RequestProcessorTest, TransferRequestProcessorRingRequestDoorbell) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  // Disable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_->GetRegisters())
      .set_utp_transfer_request_completion_enable(false)
      .WriteTo(mock_device_->GetRegisters());

  // Reuse the command descriptor in admin slot.
  auto slot_num = ufs_->GetTransferRequestProcessor().ReserveAdminSlot();
  ASSERT_TRUE(slot_num.is_ok());

  auto &slot = ufs_->GetTransferRequestProcessor().GetRequestList().GetSlot(slot_num.value());
  ASSERT_EQ(slot.state, SlotState::kReserved);

  ASSERT_EQ(
      ufs_->GetTransferRequestProcessor().RingRequestDoorbell(slot_num.value()).status_value(),
      ZX_OK);
  ASSERT_EQ(slot.state, SlotState::kScheduled);
  ASSERT_EQ(ufs_->GetTransferRequestProcessor().RequestCompletion(), 1);
  ASSERT_EQ(slot.state, SlotState::kFree);
}

TEST_F(RequestProcessorTest, FillDescriptorAndSendRequest) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  // Disable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_->GetRegisters())
      .set_utp_transfer_request_completion_enable(false)
      .WriteTo(mock_device_->GetRegisters());

  auto slot_num = ufs_->GetTransferRequestProcessor().ReserveSlot();
  ASSERT_TRUE(slot_num.is_ok());

  auto &slot = ufs_->GetTransferRequestProcessor().GetRequestList().GetSlot(slot_num.value());
  ASSERT_EQ(slot.state, SlotState::kReserved);

  DataDirection data_dir = DataDirection::kHostToDevice;
  constexpr uint16_t response_offset = 0x12;
  constexpr uint16_t response_length = 0x34;
  constexpr uint16_t prdt_offset = 0x56;
  constexpr uint16_t prdt_entry_count = 0x78;
  ASSERT_EQ(FillDescriptorAndSendRequest(slot_num.value(), data_dir, response_offset,
                                         response_length, prdt_offset, prdt_entry_count)
                .status_value(),
            ZX_OK);

  ASSERT_EQ(slot.state, SlotState::kScheduled);
  ASSERT_EQ(ufs_->GetTransferRequestProcessor().RequestCompletion(), 1);
  ASSERT_EQ(slot.state, SlotState::kFree);

  // Check Utp Transfer Request Descriptor
  auto descriptor = ufs_->GetTransferRequestProcessor()
                        .GetRequestList()
                        .GetRequestDescriptor<TransferRequestDescriptor>(slot_num.value());
  EXPECT_EQ(descriptor->command_type(), kCommandTypeUfsStorage);
  EXPECT_EQ(descriptor->data_direction(), data_dir);
  EXPECT_EQ(descriptor->interrupt(), 1);
  EXPECT_EQ(descriptor->ce(), 0);                      // Crypto is not supported
  EXPECT_EQ(descriptor->cci(), 0);                     // Crypto is not supported
  EXPECT_EQ(descriptor->data_unit_number_lower(), 0);  // Crypto is not supported
  EXPECT_EQ(descriptor->data_unit_number_upper(), 0);  // Crypto is not supported

  zx_paddr_t paddr = ufs_->GetTransferRequestProcessor()
                         .GetRequestList()
                         .GetSlot(slot_num.value())
                         .command_descriptor_io->phys();
  EXPECT_EQ(descriptor->utp_command_descriptor_base_address(), paddr & UINT32_MAX);
  EXPECT_EQ(descriptor->utp_command_descriptor_base_address_upper(),
            static_cast<uint32_t>(paddr >> 32));
  constexpr uint16_t kDwordSize = 4;
  EXPECT_EQ(descriptor->response_upiu_offset(), response_offset / kDwordSize);
  EXPECT_EQ(descriptor->response_upiu_length(), response_length / kDwordSize);
  EXPECT_EQ(descriptor->prdt_offset(), prdt_offset / kDwordSize);
  EXPECT_EQ(descriptor->prdt_length(), prdt_entry_count);
}

TEST_F(RequestProcessorTest, SendQueryUpiu) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  ReadAttributeUpiu request(Attributes::bBootLunEn);
  auto response = ufs_->GetTransferRequestProcessor().SendQueryRequestUpiu(request);
  ASSERT_OK(response);

  // Check that the Request UPIU is copied into the command descriptor.
  constexpr uint8_t slot_num = kAdminCommandSlotNumber;
  AbstractUpiu command_descriptor(
      ufs_->GetTransferRequestProcessor().GetRequestList().GetDescriptorBuffer(slot_num));
  ASSERT_EQ(memcmp(request.GetData(), command_descriptor.GetData(), sizeof(QueryRequestUpiuData)),
            0);

  // Check response
  ASSERT_EQ(response->GetHeader().trans_code(), UpiuTransactionCodes::kQueryResponse);
  ASSERT_EQ(response->GetHeader().function,
            static_cast<uint8_t>(QueryFunction::kStandardReadRequest));
  ASSERT_EQ(response->GetHeader().response, UpiuHeaderResponse::kTargetSuccess);
  ASSERT_EQ(response->GetHeader().data_segment_length, 0);
  ASSERT_EQ(response->GetHeader().flags, 0);
  ASSERT_EQ(response->GetOpcode(), static_cast<uint8_t>(QueryOpcode::kReadAttribute));
  ASSERT_EQ(response->GetIdn(), static_cast<uint8_t>(Attributes::bBootLunEn));
  ASSERT_EQ(response->GetIndex(), 0);
}

TEST_F(RequestProcessorTest, SendQueryUpiuException) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  // Disable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_->GetRegisters())
      .set_utp_transfer_request_completion_enable(false)
      .WriteTo(mock_device_->GetRegisters());

  ReadAttributeUpiu request(Attributes::bBootLunEn);
  ufs_->GetTransferRequestProcessor().SetTimeout(zx::msec(100));
  auto response = ufs_->GetTransferRequestProcessor().SendQueryRequestUpiu(request);
  ASSERT_EQ(response.status_value(), ZX_ERR_TIMED_OUT);
  ufs_->ProcessCompletions();

  // Enable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_->GetRegisters())
      .set_utp_transfer_request_completion_enable(true)
      .WriteTo(mock_device_->GetRegisters());

  // Hook the query request handler to set a response error
  mock_device_->GetTransferRequestProcessor().SetHook(
      UpiuTransactionCodes::kQueryRequest,
      [](UfsMockDevice &mock_device, CommandDescriptorData command_descriptor_data) {
        QueryRequestUpiuData *request_upiu = reinterpret_cast<QueryRequestUpiuData *>(
            command_descriptor_data.command_upiu_base_addr);
        QueryResponseUpiuData *response_upiu = reinterpret_cast<QueryResponseUpiuData *>(
            command_descriptor_data.response_upiu_base_addr);

        response_upiu->opcode = request_upiu->opcode;
        response_upiu->idn = request_upiu->idn;
        response_upiu->index = request_upiu->index;
        response_upiu->selector = request_upiu->selector;

        // Set response error
        response_upiu->header.response = UpiuHeaderResponse::kTargetFailure;

        return mock_device.GetQueryRequestProcessor().HandleQueryRequest(*request_upiu,
                                                                         *response_upiu);
      });

  ufs_->GetTransferRequestProcessor().SetTimeout(kCommandTimeout);
  response = ufs_->GetTransferRequestProcessor().SendQueryRequestUpiu(request);
  ASSERT_EQ(response.status_value(), ZX_ERR_BAD_STATE);
}

TEST_F(RequestProcessorTest, SendNopUpiu) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  NopOutUpiu nop_out_upiu;
  auto nop_in =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<NopOutUpiu, NopInUpiu>(nop_out_upiu);
  ASSERT_OK(nop_in);

  // Check that the nop out UPIU is copied into the command descriptor.
  constexpr uint8_t slot_num = kAdminCommandSlotNumber;
  AbstractUpiu command_descriptor(
      ufs_->GetTransferRequestProcessor().GetRequestList().GetDescriptorBuffer(slot_num));
  ASSERT_EQ(memcmp(nop_out_upiu.GetData(), command_descriptor.GetData(), sizeof(NopOutUpiuData)),
            0);

  // Check response
  ASSERT_EQ(nop_in->GetHeader().trans_code(), UpiuTransactionCodes::kNopIn);
  ASSERT_EQ(nop_in->GetHeader().function, 0);
  ASSERT_EQ(nop_in->GetHeader().response, UpiuHeaderResponse::kTargetSuccess);
  ASSERT_EQ(nop_in->GetHeader().data_segment_length, 0);
  ASSERT_EQ(nop_in->GetHeader().flags, 0);
}

TEST_F(RequestProcessorTest, SendNopUpiuException) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  // Disable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_->GetRegisters())
      .set_utp_transfer_request_completion_enable(false)
      .WriteTo(mock_device_->GetRegisters());

  NopOutUpiu nop_out_upiu;
  ufs_->GetTransferRequestProcessor().SetTimeout(zx::msec(100));
  auto nop_in =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<NopOutUpiu, NopInUpiu>(nop_out_upiu);
  ASSERT_EQ(nop_in.status_value(), ZX_ERR_TIMED_OUT);
  ufs_->ProcessCompletions();

  // Enable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_->GetRegisters())
      .set_utp_transfer_request_completion_enable(true)
      .WriteTo(mock_device_->GetRegisters());

  // Hook the nop out handler to set a response error
  mock_device_->GetTransferRequestProcessor().SetHook(
      UpiuTransactionCodes::kNopOut,
      [](UfsMockDevice &mock_device, CommandDescriptorData command_descriptor_data) {
        NopInUpiuData *nop_in_upiu =
            reinterpret_cast<NopInUpiuData *>(command_descriptor_data.response_upiu_base_addr);
        nop_in_upiu->header.data_segment_length = 0;
        nop_in_upiu->header.flags = 0;
        nop_in_upiu->header.response = UpiuHeaderResponse::kTargetFailure;
        return ZX_OK;
      });

  ufs_->GetTransferRequestProcessor().SetTimeout(kCommandTimeout);
  nop_in = ufs_->GetTransferRequestProcessor().SendRequestUpiu<NopOutUpiu, NopInUpiu>(nop_out_upiu);
  ASSERT_EQ(nop_in.status_value(), ZX_ERR_BAD_STATE);
}

TEST_F(RequestProcessorTest, SendRequestUpiuWithAdminSlotIsFull) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  // Reserve admin slot.
  ASSERT_OK(ufs_->GetTransferRequestProcessor().ReserveAdminSlot());

  // Make request UPIU
  NopOutUpiu nop_out_upiu;
  auto nop_in =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<NopOutUpiu, NopInUpiu>(nop_out_upiu);
  ASSERT_EQ(nop_in.status_value(), ZX_ERR_NO_RESOURCES);
}

TEST_F(RequestProcessorTest, SendRequestUsingSlot) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  constexpr uint8_t kTestLun = 0;

  auto slot = ufs_->GetTransferRequestProcessor().ReserveSlot();
  ASSERT_OK(slot);

  // Send scsi command with SendRequestUsingSlot()
  uint8_t cdb_buffer[6] = {};
  auto cdb = reinterpret_cast<scsi::TestUnitReadyCDB *>(cdb_buffer);
  cdb->opcode = scsi::Opcode::TEST_UNIT_READY;

  ScsiCommandUpiu upiu(cdb_buffer, sizeof(*cdb), DataDirection::kNone);
  auto response_or = ufs_->GetTransferRequestProcessor().SendRequestUsingSlot<ScsiCommandUpiu>(
      upiu, kTestLun, slot.value(), std::nullopt, nullptr, /*is_sync*/ true);
  ASSERT_OK(response_or);

  // Check that the SCSI UPIU is copied into the command descriptor.
  AbstractUpiu command_descriptor(
      ufs_->GetTransferRequestProcessor().GetRequestList().GetDescriptorBuffer(slot.value()));

  ASSERT_EQ(memcmp(upiu.GetData(), command_descriptor.GetData(), sizeof(CommandUpiuData)), 0);

  // Check response
  ResponseUpiu response(response_or.value());
  EXPECT_EQ(response.GetHeader().trans_code(), UpiuTransactionCodes::kResponse);
  EXPECT_EQ(response.GetHeader().status, static_cast<uint8_t>(scsi::StatusCode::GOOD));
  EXPECT_EQ(response.GetHeader().response, UpiuHeaderResponse::kTargetSuccess);
}

TEST_F(RequestProcessorTest, SendRequestUsingSlotTimeout) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  constexpr uint8_t kTestLun = 0;

  // Disable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_->GetRegisters())
      .set_utp_transfer_request_completion_enable(false)
      .WriteTo(mock_device_->GetRegisters());

  ufs_->GetTransferRequestProcessor().SetTimeout(zx::msec(100));

  auto slot = ufs_->GetTransferRequestProcessor().ReserveSlot();
  ASSERT_OK(slot);

  // Send scsi command with SendRequestUsingSlot()
  uint8_t cdb_buffer[6] = {};
  auto cdb = reinterpret_cast<scsi::TestUnitReadyCDB *>(cdb_buffer);
  cdb->opcode = scsi::Opcode::TEST_UNIT_READY;

  ScsiCommandUpiu upiu(cdb_buffer, sizeof(*cdb), DataDirection::kNone);
  auto response_or = ufs_->GetTransferRequestProcessor().SendRequestUsingSlot<ScsiCommandUpiu>(
      upiu, kTestLun, slot.value(), std::nullopt, nullptr, /*is_sync*/ true);
  ASSERT_EQ(response_or.status_value(), ZX_ERR_TIMED_OUT);
}

TEST_F(RequestProcessorTest, SendScsiUpiu) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  constexpr uint8_t kTestLun = 0;

  // Send scsi command with SendScsiUpiu()
  uint8_t cdb_buffer[6] = {};
  auto cdb = reinterpret_cast<scsi::TestUnitReadyCDB *>(cdb_buffer);
  cdb->opcode = scsi::Opcode::TEST_UNIT_READY;

  ScsiCommandUpiu upiu(cdb_buffer, sizeof(*cdb), DataDirection::kNone);
  auto response_or = ufs_->GetTransferRequestProcessor().SendScsiUpiu(upiu, kTestLun);
  ASSERT_OK(response_or);

  // Check that the SCSI UPIU is copied into the command descriptor.
  AbstractUpiu command_descriptor(
      ufs_->GetTransferRequestProcessor().GetRequestList().GetDescriptorBuffer(
          kAdminCommandSlotNumber));
  ASSERT_EQ(memcmp(upiu.GetData(), command_descriptor.GetData(), sizeof(CommandUpiuData)), 0);

  // Check response
  ResponseUpiu response(response_or->GetData());
  EXPECT_EQ(response.GetHeader().trans_code(), UpiuTransactionCodes::kResponse);
  EXPECT_EQ(response.GetHeader().status, static_cast<uint8_t>(scsi::StatusCode::GOOD));
  EXPECT_EQ(response.GetHeader().response, UpiuHeaderResponse::kTargetSuccess);
}

TEST_F(RequestProcessorTest, SendScsiUpiuTimeout) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  constexpr uint8_t kTestLun = 0;

  // Disable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_->GetRegisters())
      .set_utp_transfer_request_completion_enable(false)
      .WriteTo(mock_device_->GetRegisters());

  ufs_->GetTransferRequestProcessor().SetTimeout(zx::msec(100));

  // Send scsi command with SendScsiUpiu()
  uint8_t cdb_buffer[6] = {};
  auto cdb = reinterpret_cast<scsi::TestUnitReadyCDB *>(cdb_buffer);
  cdb->opcode = scsi::Opcode::TEST_UNIT_READY;

  ScsiCommandUpiu upiu(cdb_buffer, sizeof(*cdb), DataDirection::kNone);
  auto response_or = ufs_->GetTransferRequestProcessor().SendScsiUpiu(upiu, kTestLun);
  ASSERT_EQ(response_or.status_value(), ZX_ERR_TIMED_OUT);
}

TEST_F(RequestProcessorTest, SendScsiUpiuWithAdminSlotIsFull) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  constexpr uint8_t kTestLun = 0;

  ASSERT_OK(ufs_->GetTransferRequestProcessor().ReserveAdminSlot());

  uint8_t cdb_buffer[6] = {};
  auto cdb = reinterpret_cast<scsi::TestUnitReadyCDB *>(cdb_buffer);
  cdb->opcode = scsi::Opcode::TEST_UNIT_READY;

  ScsiCommandUpiu upiu(cdb_buffer, sizeof(*cdb), DataDirection::kNone);
  auto response = ufs_->GetTransferRequestProcessor().SendScsiUpiu(upiu, kTestLun);
  ASSERT_EQ(response.status_value(), ZX_ERR_NO_RESOURCES);
}

TEST_F(RequestProcessorTest, SendScsiUpiuWithSlotIsFull) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  constexpr uint8_t kTestLun = 0;
  const uint8_t kMaxSlotCount =
      ufs_->GetTransferRequestProcessor().GetRequestList().GetSlotCount() - kAdminCommandSlotCount;

  // Reserve all slots.
  for (uint32_t slot_num = 0; slot_num < kMaxSlotCount; ++slot_num) {
    ASSERT_OK(ufs_->GetTransferRequestProcessor().ReserveSlot());
  }

  IoCommand empty_io_cmd;

  uint8_t cdb_buffer[6] = {};
  auto cdb = reinterpret_cast<scsi::TestUnitReadyCDB *>(cdb_buffer);
  cdb->opcode = scsi::Opcode::TEST_UNIT_READY;

  ScsiCommandUpiu upiu(cdb_buffer, sizeof(*cdb), DataDirection::kNone);
  auto response =
      ufs_->GetTransferRequestProcessor().SendScsiUpiu(upiu, kTestLun, std::nullopt, &empty_io_cmd);
  ASSERT_EQ(response.status_value(), ZX_ERR_NO_RESOURCES);
}

}  // namespace ufs
