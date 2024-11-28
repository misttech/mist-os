// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cstdint>
#include <memory>

#include "src/devices/block/drivers/ufs/upiu/descriptors.h"
#include "src/devices/block/drivers/ufs/upiu/upiu_transactions.h"
#include "unit-lib.h"
#include "zircon/errors.h"

namespace ufs {
using namespace ufs_mock_device;

using TaskManagementRequestProcessorTest = UfsTest;

TEST_F(TaskManagementRequestProcessorTest, RingRequestDoorbell) {
  // Disable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_.GetRegisters())
      .set_utp_task_management_request_completion_enable(false)
      .WriteTo(mock_device_.GetRegisters());

  auto slot_num = ReserveSlot<ufs::TaskManagementRequestProcessor>();
  ASSERT_TRUE(slot_num.is_ok());

  auto &slot = dut_->GetTaskManagementRequestProcessor().GetRequestList().GetSlot(slot_num.value());
  ASSERT_EQ(slot.state, SlotState::kReserved);

  ASSERT_EQ(
      RingRequestDoorbell<ufs::TaskManagementRequestProcessor>(slot_num.value()).status_value(),
      ZX_OK);
  ASSERT_EQ(slot.state, SlotState::kScheduled);
  ASSERT_EQ(dut_->GetTaskManagementRequestProcessor().IoRequestCompletion(), 1U);
  ASSERT_OK(slot.result);
}

TEST_F(TaskManagementRequestProcessorTest, FillDescriptorAndSendRequest) {
  // Disable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_.GetRegisters())
      .set_utp_task_management_request_completion_enable(false)
      .WriteTo(mock_device_.GetRegisters());

  auto slot_num = ReserveSlot<ufs::TaskManagementRequestProcessor>();
  ASSERT_TRUE(slot_num.is_ok());

  auto &slot = dut_->GetTaskManagementRequestProcessor().GetRequestList().GetSlot(slot_num.value());
  ASSERT_EQ(slot.state, SlotState::kReserved);

  TaskManagementRequestUpiu request(TaskManagementFunction::kQueryTask, 0, slot_num.value());
  ASSERT_EQ(TaskManagementFillDescriptorAndSendRequest(slot_num.value(), request).status_value(),
            ZX_OK);

  ASSERT_EQ(slot.state, SlotState::kScheduled);
  ASSERT_EQ(dut_->GetTaskManagementRequestProcessor().IoRequestCompletion(), 1U);
  ASSERT_OK(slot.result);

  // Check UTP Task Management Request Descriptor
  auto descriptor = dut_->GetTaskManagementRequestProcessor()
                        .GetRequestList()
                        .GetRequestDescriptor<TaskManagementRequestDescriptor>(slot_num.value());
  EXPECT_EQ(descriptor->interrupt(), 1U);
  EXPECT_EQ(descriptor->overall_command_status(), OverallCommandStatus::kSuccess);
}

TEST_F(TaskManagementRequestProcessorTest, SendTaskManagementRequest) {
  uint8_t target_lun = 0;
  uint8_t target_task_tag = 0;
  TaskManagementRequestUpiu request(TaskManagementFunction::kQueryTask, target_lun,
                                    target_task_tag);
  auto response = dut_->GetTaskManagementRequestProcessor().SendTaskManagementRequest(request);
  ASSERT_OK(response);

  // Check that the Request UPIU is copied into the request descriptor.
  constexpr uint8_t slot_num = 0;
  auto descriptor = dut_->GetTaskManagementRequestProcessor()
                        .GetRequestList()
                        .GetRequestDescriptor<TaskManagementRequestDescriptor>(slot_num);
  ASSERT_EQ(memcmp(response->GetData(), descriptor->GetResponseData(),
                   sizeof(TaskManagementResponseUpiuData)),
            0);

  // Check response
  ASSERT_EQ(response->GetHeader().trans_code(),
            static_cast<uint8_t>(UpiuTransactionCodes::kTaskManagementResponse));
  ASSERT_EQ(response->GetHeader().function,
            static_cast<uint8_t>(TaskManagementFunction::kQueryTask));
  ASSERT_EQ(response->GetHeader().response, UpiuHeaderResponse::kTargetSuccess);
  ASSERT_EQ(response->GetHeader().data_segment_length, 0);
  ASSERT_EQ(response->GetHeader().flags, 0);

  ASSERT_EQ(response->GetData<TaskManagementResponseUpiuData>()->output_param1,
            static_cast<uint32_t>(TaskManagementServiceResponse::kTaskManagementFunctionSucceeded));
}

TEST_F(TaskManagementRequestProcessorTest, SendTaskManagementRequestException) {
  // Disable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_.GetRegisters())
      .set_utp_task_management_request_completion_enable(false)
      .WriteTo(mock_device_.GetRegisters());

  uint8_t target_lun = 0;
  uint8_t target_task_tag = 0;
  TaskManagementRequestUpiu request(TaskManagementFunction::kQueryTask, target_lun,
                                    target_task_tag);
  dut_->GetTaskManagementRequestProcessor().SetTimeout(zx::msec(100));
  auto response = dut_->GetTaskManagementRequestProcessor().SendTaskManagementRequest(request);
  ASSERT_EQ(response.status_value(), ZX_ERR_TIMED_OUT);
  dut_->GetTaskManagementRequestProcessor().IoRequestCompletion();

  // Enable completion interrupt
  InterruptEnableReg::Get()
      .ReadFrom(mock_device_.GetRegisters())
      .set_utp_task_management_request_completion_enable(true)
      .WriteTo(mock_device_.GetRegisters());

  // Hook the query request handler to set a response error
  mock_device_.GetTaskManagementRequestProcessor().SetHook(
      TaskManagementFunction::kQueryTask,
      [](UfsMockDevice &mock_device, TaskManagementRequestDescriptor &descriptor) {
        TaskManagementResponseUpiuData *response_upiu = descriptor.GetResponseData();

        // Set response error
        response_upiu->header.response = UpiuHeaderResponse::kTargetFailure;
        mock_device.GetTaskManagementRequestProcessor().DefaultQueryTaskHandler(mock_device,
                                                                                descriptor);

        return ZX_OK;
      });

  dut_->GetTaskManagementRequestProcessor().SetTimeout(kCommandTimeout);
  response = dut_->GetTaskManagementRequestProcessor().SendTaskManagementRequest(request);
  ASSERT_EQ(response.status_value(), ZX_ERR_BAD_STATE);
}

TEST_F(TaskManagementRequestProcessorTest, SendScsiUpiuWithSlotIsFull) {
  constexpr uint8_t kTestLun = 0;
  const uint8_t kMaxSlotCount =
      dut_->GetTaskManagementRequestProcessor().GetRequestList().GetSlotCount();

  // Reserve all slots.
  for (uint32_t slot_num = 0; slot_num < kMaxSlotCount; ++slot_num) {
    ASSERT_OK(ReserveSlot<ufs::TaskManagementRequestProcessor>());
  }

  uint8_t target_task_tag = 0;
  TaskManagementRequestUpiu request(TaskManagementFunction::kQueryTask, kTestLun, target_task_tag);
  auto response = dut_->GetTaskManagementRequestProcessor().SendTaskManagementRequest(request);
  ASSERT_EQ(response.status_value(), ZX_ERR_NO_RESOURCES);
}

}  // namespace ufs
