// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/debug.h>

#include "ufs-mock-device.h"

namespace ufs {
namespace ufs_mock_device {

void UicCmdProcessor::HandleUicCmd(UicCommandOpcode value) {
  uint32_t ucmdarg1 =
      UicCommandArgument1Reg::Get().ReadFrom(mock_device_.GetRegisters()).reg_value();
  uint32_t ucmdarg2 =
      UicCommandArgument2Reg::Get().ReadFrom(mock_device_.GetRegisters()).reg_value();
  uint32_t ucmdarg3 =
      UicCommandArgument3Reg::Get().ReadFrom(mock_device_.GetRegisters()).reg_value();
  if (auto it = handlers_.find(value); it != handlers_.end()) {
    (it->second)(mock_device_, ucmdarg1, ucmdarg2, ucmdarg3);
  } else {
    // TODO(fxbug.dev/124835): Revisit it when UICCMD error handling logic is implemented in the
    // driver.
    zxlogf(ERROR, "UFS MOCK: uiccmd value: 0x%x is not supported", value);
  }

  InterruptStatusReg::Get()
      .ReadFrom(mock_device_.GetRegisters())
      .set_uic_command_completion_status(true)
      .WriteTo(mock_device_.GetRegisters());
  if (InterruptEnableReg::Get()
          .ReadFrom(mock_device_.GetRegisters())
          .uic_command_completion_enable()) {
    mock_device_.TriggerInterrupt();
  }
}

void UicCmdProcessor::DefaultDmeLinkStartUpHandler(UfsMockDevice& mock_device, uint32_t ucmdarg1,
                                                   uint32_t ucmdarg2, uint32_t ucmdarg3) {
  HostControllerStatusReg::Get()
      .ReadFrom(mock_device.GetRegisters())
      .set_device_present(true)
      .set_utp_transfer_request_list_ready(true)
      .set_utp_task_management_request_list_ready(true)
      .WriteTo(mock_device.GetRegisters());
}

}  // namespace ufs_mock_device
}  // namespace ufs
