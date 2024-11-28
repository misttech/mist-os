// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/ufs/registers.h"
#include "src/devices/block/drivers/ufs/upiu/upiu_transactions.h"
#include "unit-lib.h"

namespace ufs {
using RegisterTest = UfsTest;

TEST_F(RegisterTest, HostCapabilities) {
  // Read only register
  EXPECT_FALSE(CapabilityReg::Get().ReadFrom(&dut_->GetMmio()).crypto_support());
  EXPECT_FALSE(
      CapabilityReg::Get().ReadFrom(&dut_->GetMmio()).uic_dme_test_mode_command_supported());
  EXPECT_FALSE(
      CapabilityReg::Get().ReadFrom(&dut_->GetMmio()).out_of_order_data_delivery_supported());
  EXPECT_TRUE(CapabilityReg::Get().ReadFrom(&dut_->GetMmio())._64_bit_addressing_supported());
  EXPECT_FALSE(CapabilityReg::Get().ReadFrom(&dut_->GetMmio()).auto_hibernation_support());
  EXPECT_EQ(CapabilityReg::Get()
                    .ReadFrom(&dut_->GetMmio())
                    .number_of_utp_task_management_request_slots() +
                1,
            ufs_mock_device::UfsMockDevice::kNutmrs);
  EXPECT_EQ(
      CapabilityReg::Get().ReadFrom(&dut_->GetMmio()).number_of_utp_transfer_request_slots() + 1,
      ufs_mock_device::UfsMockDevice::kNutrs);
}

TEST_F(RegisterTest, Version) {
  // Read only register
  EXPECT_EQ(VersionReg::Get().ReadFrom(&dut_->GetMmio()).major_version_number(),
            ufs_mock_device::kMajorVersion);
  EXPECT_EQ(VersionReg::Get().ReadFrom(&dut_->GetMmio()).minor_version_number(),
            ufs_mock_device::kMinorVersion);
  EXPECT_EQ(VersionReg::Get().ReadFrom(&dut_->GetMmio()).version_suffix(),
            ufs_mock_device::kVersionSuffix);
}

TEST_F(RegisterTest, InterruptStatus) {
  // Clear IS register to zero.
  InterruptStatusReg::Get()
      .ReadFrom(&dut_->GetMmio())
      .set_reg_value(0xffffffff)
      .WriteTo(&dut_->GetMmio());
  EXPECT_EQ(InterruptStatusReg::Get().ReadFrom(&dut_->GetMmio()).reg_value(), 0U);

  // Send UIC command to set |uic_command_completion_status|
  DmeLinkStartUpUicCommand link_startup_command(*dut_);
  EXPECT_TRUE(link_startup_command.SendCommand().is_ok());

  // InterruptStatus is cleared by SendUicCommand().
  EXPECT_FALSE(
      InterruptStatusReg::Get().ReadFrom(&dut_->GetMmio()).uic_command_completion_status());

  // Send UPIU command to set |utp_transfer_request_completion_status|
  uint8_t cdb_buffer[6] = {};
  auto cdb = reinterpret_cast<scsi::TestUnitReadyCDB*>(cdb_buffer);
  cdb->opcode = scsi::Opcode::TEST_UNIT_READY;

  ScsiCommandUpiu unit_ready_upiu(cdb_buffer, sizeof(*cdb), DataDirection::kNone);
  EXPECT_OK(dut_->GetTransferRequestProcessor().SendScsiUpiu(unit_ready_upiu, 0));

  // InterruptStatus is cleared by Isr().
  EXPECT_FALSE(InterruptStatusReg::Get()
                   .ReadFrom(&dut_->GetMmio())
                   .utp_transfer_request_completion_status());

  // Hook InterruptStatus handler to set interrupt status.
  mock_device_.GetRegisterMmioProcessor().SetHook(
      RegisterMap::kIS, [](ufs_mock_device::UfsMockDevice& mock_device, uint32_t value) {
        InterruptStatusReg::Get().FromValue(value).WriteTo(mock_device.GetRegisters());
      });

  auto register_value = InterruptStatusReg::Get().FromValue(0);
  // Set error in InterruptStatus
  register_value.set_uic_error(true)
      .set_device_fatal_error_status(true)
      .set_host_controller_fatal_error_status(true)
      .set_system_bus_fatal_error_status(true)
      .set_crypto_engine_fatal_error_status(true);
  // Set unused command completion
  register_value.set_utp_task_management_request_completion_status(true);
  register_value.WriteTo(&dut_->GetMmio());

  // Restore the default handler.
  mock_device_.GetRegisterMmioProcessor().SetHook(
      RegisterMap::kIS, ufs_mock_device::RegisterMmioProcessor::DefaultISHandler);

  mock_device_.TriggerInterrupt();

  // Wait for the interrupt to complete.
  auto wait_for = [&]() -> bool {
    return InterruptStatusReg::Get().ReadFrom(&dut_->GetMmio()).reg_value() == 0;
  };
  fbl::String timeout_message = "Timeout waiting for ISR()";
  constexpr uint32_t kTimeoutUs = 1000000;
  ASSERT_OK(dut_->WaitWithTimeout(wait_for, kTimeoutUs, timeout_message));

  // Verify that the ISR has processed all interruptStatus
  EXPECT_EQ(InterruptStatusReg::Get().ReadFrom(&dut_->GetMmio()).reg_value(), 0U);
}

TEST_F(RegisterTest, InterruptEnable) {
  EXPECT_TRUE(
      InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).crypto_engine_fatal_error_enable());
  EXPECT_TRUE(InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).system_bus_fatal_error_enable());
  EXPECT_TRUE(
      InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).host_controller_fatal_error_enable());
  EXPECT_TRUE(InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).utp_error_enable());
  EXPECT_TRUE(InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).device_fatal_error_enable());
  EXPECT_FALSE(
      InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).uic_command_completion_enable());
  EXPECT_TRUE(InterruptEnableReg::Get()
                  .ReadFrom(&dut_->GetMmio())
                  .utp_transfer_request_completion_enable());
  EXPECT_FALSE(
      InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).uic_link_startup_status_enable());
  EXPECT_TRUE(InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).uic_link_lost_status_enable());
  EXPECT_FALSE(
      InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).uic_hibernate_enter_status_enable());
  EXPECT_FALSE(
      InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).uic_hibernate_exit_status_enable());
  EXPECT_FALSE(InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).uic_power_mode_status_enable());
  EXPECT_TRUE(InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).uic_test_mode_status_enable());
  EXPECT_TRUE(InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).uic_error_enable());
  EXPECT_TRUE(InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).uic_dme_endpointreset());
  EXPECT_TRUE(InterruptEnableReg::Get()
                  .ReadFrom(&dut_->GetMmio())
                  .utp_task_management_request_completion_enable());

  // Set |uic_dme_endpointreset| to test interrupt enable
  InterruptEnableReg::Get()
      .ReadFrom(&dut_->GetMmio())
      .set_uic_dme_endpointreset(1)
      .WriteTo(&dut_->GetMmio());
  EXPECT_TRUE(InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).uic_dme_endpointreset());
  // Clear |uic_dme_endpointreset| to test interrupt enable
  InterruptEnableReg::Get()
      .ReadFrom(&dut_->GetMmio())
      .set_uic_dme_endpointreset(0)
      .WriteTo(&dut_->GetMmio());
  EXPECT_FALSE(InterruptEnableReg::Get().ReadFrom(&dut_->GetMmio()).uic_dme_endpointreset());
}

TEST_F(RegisterTest, HostControllerStatus) {
  // Read only register
  EXPECT_EQ(HostControllerStatusReg::Get().ReadFrom(&dut_->GetMmio()).target_lun_of_utp_error(),
            0U);
  EXPECT_EQ(HostControllerStatusReg::Get().ReadFrom(&dut_->GetMmio()).task_tag_of_utp_error(), 0U);
  EXPECT_EQ(HostControllerStatusReg::Get().ReadFrom(&dut_->GetMmio()).utp_error_code(), 0U);
  EXPECT_EQ(HostControllerStatusReg::Get()
                .ReadFrom(&dut_->GetMmio())
                .uic_power_mode_change_request_status(),
            HostControllerStatusReg::PowerModeStatus::kPowerLocal);
  EXPECT_TRUE(HostControllerStatusReg::Get().ReadFrom(&dut_->GetMmio()).uic_command_ready());
  EXPECT_TRUE(HostControllerStatusReg::Get()
                  .ReadFrom(&dut_->GetMmio())
                  .utp_task_management_request_list_ready());
  EXPECT_TRUE(
      HostControllerStatusReg::Get().ReadFrom(&dut_->GetMmio()).utp_transfer_request_list_ready());
  EXPECT_TRUE(HostControllerStatusReg::Get().ReadFrom(&dut_->GetMmio()).device_present());
}

TEST_F(RegisterTest, HostControllerEnable) {
  EXPECT_FALSE(HostControllerEnableReg::Get().ReadFrom(&dut_->GetMmio()).crypto_general_enable());
  EXPECT_TRUE(HostControllerEnableReg::Get().ReadFrom(&dut_->GetMmio()).host_controller_enable());

  EXPECT_OK(DisableController());
  EXPECT_FALSE(HostControllerEnableReg::Get().ReadFrom(&dut_->GetMmio()).host_controller_enable());

  EXPECT_OK(EnableController());
  EXPECT_TRUE(HostControllerEnableReg::Get().ReadFrom(&dut_->GetMmio()).host_controller_enable());
}

TEST_F(RegisterTest, UtpTransferRequestListBaseAddress) {
  // TODO(https://fxbug.dev/42075643): Writing unit test after a transfer request list is
  // implemented
}

TEST_F(RegisterTest, UtpTransferRequestListDoorbell) {
  // TODO(https://fxbug.dev/42075643): Writing unit test after a transfer request list is
  // implemented
}

TEST_F(RegisterTest, UtpTransferRequestListRunStop) {
  // TODO(https://fxbug.dev/42075643): Writing unit test after a transfer request list is
  // implemented
}

TEST_F(RegisterTest, UtpTaskManagementRequestListBaseAddress) {
  // TODO(https://fxbug.dev/42075643): Writing unit test after a task management list is implemented
}

TEST_F(RegisterTest, UtpTaskManagementRequestListDoorbell) {
  // TODO(https://fxbug.dev/42075643): Writing unit test after a task management list is implemented
}

TEST_F(RegisterTest, UTPTaskManagementRequestListRunStop) {
  // TODO(https://fxbug.dev/42075643): Writing unit test after a task management list is implemented
}

}  // namespace ufs
