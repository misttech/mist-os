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
    ASSERT_EQ(slot.command_descriptor_io.is_valid(), true);
  }
}

TEST_F(RequestProcessorTest, TransferRequestProcessorSendRequestSync) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  auto slot_num = ufs_->GetTransferRequestProcessor().ReserveSlot();
  ASSERT_TRUE(slot_num.is_ok());

  auto &slot = ufs_->GetTransferRequestProcessor().GetRequestList().GetSlot(slot_num.value());
  ASSERT_EQ(slot.state, SlotState::kReserved);

  ASSERT_EQ(ufs_->GetTransferRequestProcessor()
                .RingRequestDoorbell(slot_num.value(), true)
                .status_value(),
            ZX_OK);
  ASSERT_EQ(slot.state, SlotState::kFree);
}

TEST_F(RequestProcessorTest, TransferRequestProcessorSendRequestSyncTimeout) {
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

  ufs_->GetTransferRequestProcessor().SetTimeoutMsec(100);
  ASSERT_EQ(ufs_->GetTransferRequestProcessor()
                .RingRequestDoorbell(slot_num.value(), true)
                .status_value(),
            ZX_ERR_TIMED_OUT);
}

TEST_F(RequestProcessorTest, TransferRequestProcessorSendRequestAsync) {
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

  ASSERT_EQ(ufs_->GetTransferRequestProcessor()
                .RingRequestDoorbell(slot_num.value(), false)
                .status_value(),
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

  TransferRequestDescriptorDataDirection data_dir =
      TransferRequestDescriptorDataDirection::kHostToDevice;
  constexpr uint16_t response_offset = 0x12;
  constexpr uint16_t response_length = 0x34;
  constexpr uint16_t prdt_offset = 0x56;
  constexpr uint16_t prdt_entry_count = 0x78;
  constexpr bool sync = false;
  ASSERT_EQ(FillDescriptorAndSendRequest(slot_num.value(), data_dir, response_offset,
                                         response_length, prdt_offset, prdt_entry_count, sync)
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
                         .command_descriptor_io.phys();
  EXPECT_EQ(descriptor->utp_command_descriptor_base_address(), paddr & UINT32_MAX);
  EXPECT_EQ(descriptor->utp_command_descriptor_base_address_upper(),
            static_cast<uint32_t>(paddr >> 32));
  constexpr uint16_t kDwordSize = 4;
  EXPECT_EQ(descriptor->response_upiu_offset(), response_offset / kDwordSize);
  EXPECT_EQ(descriptor->response_upiu_length(), response_length / kDwordSize);
  EXPECT_EQ(descriptor->prdt_offset(), prdt_offset / kDwordSize);
  EXPECT_EQ(descriptor->prdt_length(), prdt_entry_count);
}

TEST_F(RequestProcessorTest, SendRequestDescriptorTimeout) {
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

  TransferRequestDescriptorDataDirection data_dir =
      TransferRequestDescriptorDataDirection::kHostToDevice;
  uint16_t response_offset = 0x12;
  uint16_t response_length = 0x34;
  uint16_t prdt_offset = 0x56;
  uint16_t prdt_entry_count = 0x78;
  bool sync = false;
  ASSERT_EQ(FillDescriptorAndSendRequest(slot_num.value(), data_dir, response_offset,
                                         response_length, prdt_offset, prdt_entry_count, sync)
                .status_value(),
            ZX_OK);

  ufs_->GetTransferRequestProcessor().SetTimeoutMsec(100);
  slot.state = SlotState::kReserved;
  ASSERT_EQ(ufs_->GetTransferRequestProcessor()
                .RingRequestDoorbell(slot_num.value(), true)
                .status_value(),
            ZX_ERR_TIMED_OUT);
}

TEST_F(RequestProcessorTest, SendQueryUpiu) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  ReadAttributeUpiu request(Attributes::bBootLunEn);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          request);
  ASSERT_OK(response);

  // Check that the Request UPIU is copied into the command descriptor.
  constexpr uint8_t slot_num = 0;
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
  ufs_->GetTransferRequestProcessor().SetTimeoutMsec(100);
  auto response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          request);
  ASSERT_EQ(response.status_value(), ZX_ERR_TIMED_OUT);

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

  response =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          request);
  ASSERT_EQ(response.status_value(), ZX_ERR_BAD_STATE);
}

TEST_F(RequestProcessorTest, SendNopUpiu) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  NopOutUpiu nop_out_upiu;
  auto nop_in =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<NopOutUpiu, NopInUpiu>(nop_out_upiu);
  ASSERT_OK(nop_in);

  // Check that the nop out UPIU is copied into the command descriptor.
  constexpr uint8_t slot_num = 0;
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
  ufs_->GetTransferRequestProcessor().SetTimeoutMsec(100);
  auto nop_in =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<NopOutUpiu, NopInUpiu>(nop_out_upiu);
  ASSERT_EQ(nop_in.status_value(), ZX_ERR_TIMED_OUT);

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

  nop_in = ufs_->GetTransferRequestProcessor().SendRequestUpiu<NopOutUpiu, NopInUpiu>(nop_out_upiu);
  ASSERT_EQ(nop_in.status_value(), ZX_ERR_BAD_STATE);
}

TEST_F(RequestProcessorTest, SendRequestUpiuWithSlotIsFull) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  const uint8_t kMaxSlotCount = ufs_->GetTransferRequestProcessor().GetRequestList().GetSlotCount();

  // Reserve all slots.
  for (uint32_t slot_num = 0; slot_num < kMaxSlotCount; ++slot_num) {
    ASSERT_OK(ufs_->GetTransferRequestProcessor().ReserveSlot());
  }

  // Make request UPIU
  NopOutUpiu nop_out_upiu;
  auto nop_in =
      ufs_->GetTransferRequestProcessor().SendRequestUpiu<NopOutUpiu, NopInUpiu>(nop_out_upiu);
  ASSERT_EQ(nop_in.status_value(), ZX_ERR_NO_RESOURCES);
}

TEST_F(RequestProcessorTest, SendScsiUpiu) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  constexpr uint8_t kTestLun = 0;

  auto slot = ufs_->GetTransferRequestProcessor().ReserveSlot();
  ASSERT_OK(slot);

  // Make SCSI UPIU
  constexpr uint32_t block_offset = 0;
  constexpr uint16_t block_length = 1;

  std::vector<zx_paddr_t> data_paddrs;

  // Send scsi command with SendRequestUsingSlot()
  ScsiSynchronizeCache10Upiu upiu(block_offset, block_length);
  auto response_or = ufs_->GetTransferRequestProcessor().SendRequestUsingSlot<ScsiCommandUpiu>(
      upiu, kTestLun, slot.value(), data_paddrs);
  ASSERT_OK(response_or.status_value());

  // Check that the SCSI UPIU is copied into the command descriptor.
  AbstractUpiu command_descriptor(
      ufs_->GetTransferRequestProcessor().GetRequestList().GetDescriptorBuffer(slot.value()));

  ASSERT_EQ(memcmp(upiu.GetData(), command_descriptor.GetData(), sizeof(CommandUpiuData)), 0);

  // Check response
  ResponseUpiu response(response_or.value());
  ASSERT_EQ(response.GetHeader().trans_code(), UpiuTransactionCodes::kResponse);
  ASSERT_EQ(response.GetHeader().status, static_cast<uint8_t>(scsi::StatusCode::GOOD));
  ASSERT_EQ(response.GetHeader().response, UpiuHeaderResponse::kTargetSuccess);

  // Send scsi command with SendScsiUpiu()
  ASSERT_OK(ufs_->GetTransferRequestProcessor().SendScsiUpiu(upiu, kTestLun, data_paddrs));
}

TEST_F(RequestProcessorTest, SendScsiUpiuWithSlotIsFull) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  constexpr uint8_t kTestLun = 0;
  const uint8_t kMaxSlotCount = ufs_->GetTransferRequestProcessor().GetRequestList().GetSlotCount();

  // Reserve all slots.
  for (uint32_t slot_num = 0; slot_num < kMaxSlotCount; ++slot_num) {
    ASSERT_OK(ufs_->GetTransferRequestProcessor().ReserveSlot());
  }

  // Make SCSI UPIU
  constexpr uint32_t block_offset = 0;
  constexpr uint16_t block_length = 1;

  std::vector<zx_paddr_t> data_paddrs;
  data_paddrs.resize(block_length, 0);

  ScsiSynchronizeCache10Upiu upiu(block_offset, block_length);
  auto response = ufs_->GetTransferRequestProcessor().SendScsiUpiu(upiu, kTestLun, data_paddrs);
  ASSERT_EQ(response.status_value(), ZX_ERR_NO_RESOURCES);
}

}  // namespace ufs
