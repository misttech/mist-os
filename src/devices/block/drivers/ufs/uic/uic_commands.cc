// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "uic_commands.h"

#include <optional>

#include "src/devices/block/drivers/ufs/registers.h"
#include "src/devices/block/drivers/ufs/ufs.h"

namespace ufs {

zx::result<std::optional<uint32_t>> UicCommand::SendCommand() {
  if (zx::result<> result = SendUicCommand(0, 0, 0); result.is_error()) {
    return result.take_error();
  }
  return zx::ok(std::nullopt);
}

zx::result<> UicCommand::SendUicCommand(uint32_t argument1, uint32_t argument2,
                                        uint32_t argument3) {
  fdf::MmioBuffer &mmio = GetController().GetMmio();

  // Clear 'UIC command completion status' if set
  if (InterruptStatusReg::Get().ReadFrom(&mmio).uic_command_completion_status()) {
    // 'UIC command completion status' is write and clear field.
    InterruptStatusReg::Get().FromValue(0).set_uic_command_completion_status(true).WriteTo(&mmio);
  }

  UicCommandArgument1Reg::Get().FromValue(argument1).WriteTo(&mmio);
  UicCommandArgument2Reg::Get().FromValue(argument2).WriteTo(&mmio);
  UicCommandArgument3Reg::Get().FromValue(argument3).WriteTo(&mmio);

  // Wait for 'UIC command ready'
  auto wait_for_command_ready = [&]() -> bool {
    return HostControllerStatusReg::Get().ReadFrom(&mmio).uic_command_ready();
  };
  fbl::String timeout_message = "Timeout waiting for 'UIC command ready'";
  if (zx_status_t status =
          controller_.WaitWithTimeout(wait_for_command_ready, timeout_usec_, timeout_message);
      status != ZX_OK) {
    return zx::error(status);
  }

  UicCommandReg::Get().FromValue(static_cast<uint8_t>(GetOpcode())).WriteTo(&mmio);

  // TODO(fxbug.dev/124835): Currently, the UIC commands are only used during the initialisation
  // process, so we implemented them as polling. However, if DME_HIBERNATE and DME_POWERMODE are
  // added in the future, it should be changed to an interrupt method.

  // Wait for 'UIC command completion status'
  auto wait_for_completion = [&]() -> bool {
    return InterruptStatusReg::Get().ReadFrom(&mmio).uic_command_completion_status();
  };
  timeout_message = "Timeout waiting for 'UIC command completion status'";
  if (zx_status_t status =
          controller_.WaitWithTimeout(wait_for_completion, timeout_usec_, timeout_message);
      status != ZX_OK) {
    return zx::error(status);
  }

  // DME_RESET does not return a result code.
  if (GetOpcode() != UicCommandOpcode::kDmeReset) {
    uint32_t result_code = UicCommandArgument2Reg::Get().ReadFrom(&mmio).result_code();
    if (result_code != UicCommandArgument2Reg::GenericErrorCode::kSuccess) {
      zxlogf(ERROR, "Failed to send UIC command, result_code = %u", result_code);
      return zx::error(ZX_ERR_INTERNAL);
    }
  }

  // If it is a hibernate command, wait for transition.
  if (GetOpcode() == UicCommandOpcode::kDmeHibernateEnter ||
      GetOpcode() == UicCommandOpcode::kDmeHibernateExit) {
    auto reg = InterruptStatusReg::Get().FromValue(0);
    uint32_t flag = GetOpcode() == UicCommandOpcode::kDmeHibernateEnter
                        ? reg.set_uic_hibernate_enter_status(1).reg_value()
                        : reg.set_uic_hibernate_exit_status(1).reg_value();

    auto wait_for = [&]() -> bool {
      return InterruptStatusReg::Get().ReadFrom(&mmio).reg_value() & flag;
    };
    timeout_message = "Timeout waiting for hibernation transition";
    if (zx_status_t status = controller_.WaitWithTimeout(wait_for, timeout_usec_, timeout_message);
        status != ZX_OK) {
      return zx::error(status);
    }

    InterruptStatusReg::Get().FromValue(flag).WriteTo(&mmio);
  }

  return zx::ok();
}

zx::result<std::optional<uint32_t>> DmeGetUicCommand::SendCommand() {
  uint32_t argument_1 = UicCommandArgument1Reg::Get()
                            .FromValue(0)
                            .set_mib_attribute(GetMbiAttribute())
                            .set_gen_selector_index(GetGenSelectorIndex())
                            .reg_value();
  if (zx::result<> result = SendUicCommand(argument_1, 0, 0); result.is_error()) {
    return result.take_error();
  }
  return zx::ok(UicCommandArgument3Reg::Get().ReadFrom(&GetController().GetMmio()).value());
}

zx::result<std::optional<uint32_t>> DmeSetUicCommand::SendCommand() {
  uint32_t argument_1 = UicCommandArgument1Reg::Get()
                            .FromValue(0)
                            .set_mib_attribute(GetMbiAttribute())
                            .set_gen_selector_index(GetGenSelectorIndex())
                            .reg_value();
  if (zx::result<> result = SendUicCommand(argument_1, 0, value_); result.is_error()) {
    return result.take_error();
  }
  return zx::ok(std::nullopt);
}

zx::result<std::optional<uint32_t>> DmeLinkStartUpUicCommand::SendCommandWithNotify() {
  if (zx::result<> result = GetController().Notify(NotifyEvent::kPreLinkStartup, 0);
      result.is_error()) {
    return result.take_error();
  }

  if (zx::result<std::optional<uint32_t>> result = SendCommand(); result.is_error()) {
    return result.take_error();
  }

  if (zx::result<> result = GetController().Notify(NotifyEvent::kPostLinkStartup, 0);
      result.is_error()) {
    return result.take_error();
  }
  return zx::ok(std::nullopt);
}

}  // namespace ufs
