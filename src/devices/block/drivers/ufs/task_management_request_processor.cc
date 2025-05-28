// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "task_management_request_processor.h"

#include <lib/trace/event.h>

#include <memory>

#include "src/devices/block/drivers/ufs/task_management_request_descriptor.h"
#include "src/devices/block/drivers/ufs/ufs.h"

namespace ufs {

zx::result<> TaskManagementRequestProcessor::Init() {
  zx_paddr_t paddr =
      request_list_.GetRequestDescriptorPhysicalAddress<TaskManagementRequestDescriptor>(0);
  UtmrListBaseAddressReg::Get().FromValue(paddr & 0xffffffff).WriteTo(&register_);
  UtmrListBaseAddressUpperReg::Get().FromValue(paddr >> 32).WriteTo(&register_);

  if (!HostControllerStatusReg::Get()
           .ReadFrom(&register_)
           .utp_task_management_request_list_ready()) {
    FDF_LOG(ERROR, "UTP task management request list is not ready\n");
    return zx::error(ZX_ERR_INTERNAL);
  }

  if (UtmrListDoorBellReg::Get().ReadFrom(&register_).door_bell() != 0) {
    FDF_LOG(ERROR, "UTP task management request list door bell is not ready\n");
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Start UTP task management request list.
  UtmrListRunStopReg::Get().FromValue(1).WriteTo(&register_);

  return zx::ok();
}

uint32_t TaskManagementRequestProcessor::IoRequestCompletion() {
  uint32_t completion_count = 0;

  // Search for all pending slots and signed the ones already done.
  for (uint8_t slot_num = 0; slot_num < request_list_.GetSlotCount(); ++slot_num) {
    RequestSlot &request_slot = request_list_.GetSlot(slot_num);
    if (request_slot.state == SlotState::kScheduled) {
      if (!(UtmrListDoorBellReg::Get().ReadFrom(&register_).door_bell() & (1 << slot_num))) {
        zx::result<> result = zx::ok();
        // Check task management response.
        auto descriptor =
            request_list_.GetRequestDescriptor<TaskManagementRequestDescriptor>(slot_num);
        TaskManagementResponseUpiu response(descriptor->GetResponseData());
        if (response.GetHeader().response != UpiuHeaderResponseCode::kTargetSuccess) {
          FDF_LOG(ERROR, "UTP task management request command failure: response=%x",
                  response.GetHeader().response);
          result = zx::error(ZX_ERR_BAD_STATE);
        }
        request_slot.result = result.status_value();
        sync_completion_signal(&request_slot.complete);

        ++completion_count;
      }
    }
  }
  return completion_count;
}

zx::result<TaskManagementResponseUpiu> TaskManagementRequestProcessor::SendTaskManagementRequest(
    TaskManagementRequestUpiu &request) {
  // TODO(https://fxbug.dev/42075643): Needs to be changed to be compatible with DFv2's dispatcher
  // Since the completion is handled by the I/O thread, submitting a synchronous command from the
  // I/O thread will cause a deadlock.
  // ZX_DEBUG_ASSERT(controller_.GetIoThread() != thrd_current());

  zx::result<uint8_t> slot = ReserveSlot();
  if (slot.is_error()) {
    return zx::error(ZX_ERR_NO_RESOURCES);
  }
  RequestSlot &request_slot = request_list_.GetSlot(slot.value());
  ZX_DEBUG_ASSERT_MSG(request_slot.state == SlotState::kReserved, "Invalid slot state");

  if (zx::result<> result = FillDescriptorAndSendRequest(slot.value(), request);
      result.is_error()) {
    FDF_LOG(ERROR, "Failed to send upiu: %s", result.status_string());
    if (zx::result<> result = ClearSlot(request_slot); result.is_error()) {
      return result.take_error();
    }
    return result.take_error();
  }

  // Wait for completion.
  TRACE_DURATION("ufs", "SendTaskManagementRequest::sync_completion_wait", "slot", slot.value());
  zx_status_t status = sync_completion_wait(&request_slot.complete, GetTimeout().get());
  zx_status_t request_result = request_slot.result;
  if (zx::result<> result = ClearSlot(request_slot); result.is_error()) {
    return result.take_error();
  }
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "SendTaskManagementRequest timed out: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  if (request_result != ZX_OK) {
    return zx::error(request_result);
  }

  // Get the UTP task management response UPIU.
  auto descriptor =
      request_list_.GetRequestDescriptor<TaskManagementRequestDescriptor>(slot.value());
  TaskManagementResponseUpiu response(descriptor->GetResponseData());

  return zx::ok(response);
}

zx::result<> TaskManagementRequestProcessor::FillDescriptorAndSendRequest(
    uint8_t slot, TaskManagementRequestUpiu &request) {
  auto descriptor = request_list_.GetRequestDescriptor<TaskManagementRequestDescriptor>(slot);

  // Fill up UTP task management request descriptor.
  memset(descriptor, 0, sizeof(TaskManagementRequestDescriptor));
  descriptor->set_interrupt(true);
  // If the command was successful, overwrite |overall_command_status| field with |kSuccess|.
  descriptor->set_overall_command_status(OverallCommandStatus::kInvalid);

  // Copy the UTP task management request UPIU to the descriptor.
  memcpy(descriptor->GetRequestData(), request.GetData(), sizeof(TaskManagementRequestUpiuData));

  if (zx::result<> result = controller_.Notify(NotifyEvent::kSetupTaskManagementRequestList, slot);
      result.is_error()) {
    return result.take_error();
  }
  if (zx::result<> result = RingRequestDoorbell(slot); result.is_error()) {
    FDF_LOG(ERROR, "Failed to send cmd %s", result.status_string());
    return result.take_error();
  }
  return zx::ok();
}

}  // namespace ufs
