// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device.manager/cpp/wire.h>
#include <fidl/fuchsia.device.power.test/cpp/wire.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.power.statecontrol/cpp/wire.h>
#include <fidl/fuchsia.process.lifecycle/cpp/wire.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver-integration-test/fixture.h>
#include <zircon/processargs.h>
#include <zircon/syscalls.h>

#include <zxtest/zxtest.h>

using driver_integration_test::IsolatedDevmgr;
using fuchsia_device::wire::DevicePerformanceStateInfo;
using fuchsia_device::wire::DevicePowerState;
using fuchsia_device::wire::DevicePowerStateInfo;
using fuchsia_device::wire::kDevicePerformanceStateP0;
using fuchsia_device::wire::kMaxDevicePowerStates;
using fuchsia_device::wire::SystemPowerStateInfo;
using fuchsia_device_manager::wire::SystemPowerState;
using fuchsia_device_power_test::TestDevice;
namespace device_manager_fidl = fuchsia_device_manager;
namespace lifecycle_fidl = fuchsia_process_lifecycle;

constexpr char kPowerTestPath[] = "sys/platform/11:0b:0/power-test";
constexpr char kPowerTestChild1Path[] = "sys/platform/11:0b:0/power-test/power-test-child";
constexpr char kPowerTestChild2Path[] = "sys/platform/11:0b:0/power-test/power-test-child-2";

class PowerTestCase : public zxtest::Test {
 public:
  ~PowerTestCase() override = default;
  void SetUp() override {
    IsolatedDevmgr::Args args;

    board_test::DeviceEntry dev = {};
    dev.vid = PDEV_VID_TEST;
    dev.pid = PDEV_PID_POWER_TEST;
    dev.did = 0;
    args.device_list.push_back(dev);

    zx_status_t status = IsolatedDevmgr::Create(&args, &devmgr);
    ASSERT_OK(status);

    zx::result parent_channel =
        device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), kPowerTestPath);
    ASSERT_OK(parent_channel);
    parent_device_client.Bind(fidl::ClientEnd<TestDevice>{std::move(parent_channel.value())});

    std::string parent_controller_path = std::string(kPowerTestPath) + "/device_controller";
    parent_channel = device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(),
                                                          parent_controller_path.c_str());
    ASSERT_OK(parent_channel);
    parent_device_controller.Bind(
        fidl::ClientEnd<fuchsia_device::Controller>{std::move(parent_channel.value())});

    zx::result child_channel =
        device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), kPowerTestChild1Path);
    ASSERT_OK(child_channel);
    child1_device_client.Bind(fidl::ClientEnd<TestDevice>{std::move(child_channel.value())});

    std::string child_controller_path = std::string(kPowerTestChild1Path) + "/device_controller";
    child_channel = device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(),
                                                         child_controller_path.c_str());
    ASSERT_OK(child_channel);
    child1_device_controller.Bind(
        fidl::ClientEnd<fuchsia_device::Controller>{std::move(child_channel.value())});
  }

  void AddChildWithPowerArgs(DevicePowerStateInfo *states, uint8_t sleep_state_count,
                             bool add_invisible = false) {
    auto power_states =
        ::fidl::VectorView<DevicePowerStateInfo>::FromExternal(states, sleep_state_count);
    auto response = child1_device_client->AddDeviceWithPowerArgs(power_states, add_invisible);
    ASSERT_OK(response.status());
    zx_status_t call_status = ZX_OK;
    if (response->is_error()) {
      call_status = response->error_value();
    }
    ASSERT_OK(call_status);

    zx::result channel =
        device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), kPowerTestChild2Path);
    ASSERT_OK(channel);
    child2_device_client.Bind(fidl::ClientEnd<TestDevice>{std::move(channel.value())});

    std::string child_controller_path = std::string(kPowerTestChild2Path) + "/device_controller";
    channel = device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(),
                                                   child_controller_path.c_str());
    ASSERT_OK(channel);
    child2_device_controller.Bind(
        fidl::ClientEnd<fuchsia_device::Controller>{std::move(channel.value())});
  }

  void WaitForDeviceSuspendCompletion(const fidl::WireSyncClient<TestDevice> &client) {
    const fidl::WireResult result = client->GetSuspendCompletionEvent();
    ASSERT_OK(result.status());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    zx_signals_t signals;
    ASSERT_OK(response->event.wait_one(ZX_USER_SIGNAL_0, zx::time::infinite(), &signals));
  }

  fidl::WireSyncClient<TestDevice> parent_device_client;
  fidl::WireSyncClient<fuchsia_device::Controller> parent_device_controller;

  fidl::WireSyncClient<TestDevice> child1_device_client;
  fidl::WireSyncClient<fuchsia_device::Controller> child1_device_controller;

  fidl::WireSyncClient<TestDevice> child2_device_client;
  fidl::WireSyncClient<fuchsia_device::Controller> child2_device_controller;
  IsolatedDevmgr devmgr;
};

TEST_F(PowerTestCase, InvalidDevicePowerCaps_Less) {
  std::array<DevicePowerStateInfo, 1> states;
  states[0].state_id = DevicePowerState::kDevicePowerStateD1;
  states[0].is_supported = true;
  auto response = child1_device_client->AddDeviceWithPowerArgs(
      fidl::VectorView<DevicePowerStateInfo>::FromExternal(states), false);
  ASSERT_OK(response.status());
  zx_status_t call_status = ZX_OK;
  if (response->is_error()) {
    call_status = response->error_value();
  }
  ASSERT_STATUS(call_status, ZX_ERR_INVALID_ARGS);
}

TEST_F(PowerTestCase, InvalidDevicePowerCaps_More) {
  std::array<DevicePowerStateInfo, kMaxDevicePowerStates + 1> states;
  for (uint8_t i = 0; i < kMaxDevicePowerStates + 1; i++) {
    states[i].state_id = DevicePowerState::kDevicePowerStateD1;
    states[i].is_supported = true;
  }
  auto response = child1_device_client->AddDeviceWithPowerArgs(
      fidl::VectorView<DevicePowerStateInfo>::FromExternal(states), false);
  ASSERT_OK(response.status());
  zx_status_t call_status = ZX_OK;
  if (response->is_error()) {
    call_status = response->error_value();
  }

  ASSERT_STATUS(call_status, ZX_ERR_INVALID_ARGS);
}

TEST_F(PowerTestCase, InvalidDevicePowerCaps_MissingRequired) {
  std::array<DevicePowerStateInfo, kMaxDevicePowerStates> states;
  for (uint8_t i = 0; i < kMaxDevicePowerStates; i++) {
    // Missing D0 and D3COLD
    states[i].state_id = DevicePowerState::kDevicePowerStateD1;
    states[i].is_supported = true;
  }
  auto response = child1_device_client->AddDeviceWithPowerArgs(
      fidl::VectorView<DevicePowerStateInfo>::FromExternal(states), false);
  ASSERT_OK(response.status());
  zx_status_t call_status = ZX_OK;
  if (response->is_error()) {
    call_status = response->error_value();
  }

  ASSERT_STATUS(call_status, ZX_ERR_INVALID_ARGS);
}

TEST_F(PowerTestCase, InvalidDevicePowerCaps_DuplicateCaps) {
  std::array<DevicePowerStateInfo, kMaxDevicePowerStates> states;
  states[0].state_id = DevicePowerState::kDevicePowerStateD0;
  states[0].is_supported = true;
  states[1].state_id = DevicePowerState::kDevicePowerStateD3Cold;
  states[1].is_supported = true;
  // Repeat
  states[2].state_id = DevicePowerState::kDevicePowerStateD3Cold;
  states[2].is_supported = true;
  auto response = child1_device_client->AddDeviceWithPowerArgs(
      fidl::VectorView<DevicePowerStateInfo>::FromExternal(states), false);
  ASSERT_OK(response.status());
  zx_status_t call_status = ZX_OK;
  if (response->is_error()) {
    call_status = response->error_value();
  }

  ASSERT_STATUS(call_status, ZX_ERR_INVALID_ARGS);
}

TEST_F(PowerTestCase, AddDevicePowerCaps_Success) {
  std::array<DevicePowerStateInfo, 2> states;
  states[0].state_id = DevicePowerState::kDevicePowerStateD0;
  states[0].is_supported = true;
  states[1].state_id = DevicePowerState::kDevicePowerStateD3Cold;
  states[1].is_supported = true;
  auto response = child1_device_client->AddDeviceWithPowerArgs(
      fidl::VectorView<DevicePowerStateInfo>::FromExternal(states), false);
  ASSERT_OK(response.status());
  zx_status_t call_status = ZX_OK;
  if (response->is_error()) {
    call_status = response->error_value();
  }

  ASSERT_STATUS(call_status, ZX_OK);
}

TEST_F(PowerTestCase, AddDevicePowerCaps_MakeVisible_Success) {
  DevicePowerStateInfo states[3];
  states[0].state_id = DevicePowerState::kDevicePowerStateD0;
  states[0].is_supported = true;
  states[0].restore_latency = 0;
  states[1].state_id = DevicePowerState::kDevicePowerStateD1;
  states[1].is_supported = true;
  states[1].restore_latency = 100;
  states[2].state_id = DevicePowerState::kDevicePowerStateD3Cold;
  states[2].is_supported = true;
  states[2].restore_latency = 1000;

  AddChildWithPowerArgs(states, std::size(states), true);
}

// TODO(http://fxbug.dev/119962): Re-enable this test after fixing.
// This test is not easy to replicate without a lot of plumbing changes to allow test to modify the
// response to fuchsia.device.manager/SystemStateTransition.GetTerminationSystemState.
TEST_F(PowerTestCase, DISABLED_SystemSuspend_SuspendReasonReboot) {
  // Add Capabilities
  DevicePowerStateInfo states[3];
  states[0].state_id = DevicePowerState::kDevicePowerStateD0;
  states[0].is_supported = true;
  states[0].restore_latency = 0;
  states[1].state_id = DevicePowerState::kDevicePowerStateD2;
  states[1].is_supported = true;
  states[1].restore_latency = 100;
  states[2].state_id = DevicePowerState::kDevicePowerStateD3Cold;
  states[2].is_supported = true;
  states[2].restore_latency = 1000;
  AddChildWithPowerArgs(states, std::size(states));

  ASSERT_OK(devmgr.SuspendDriverManager());

  // Wait till child2's suspend event is called.
  WaitForDeviceSuspendCompletion(child2_device_client);

  auto child_dev_suspend_response = child2_device_client->GetCurrentDevicePowerState();
  ASSERT_OK(child_dev_suspend_response.status());
  auto call_status = ZX_OK;
  if (child_dev_suspend_response->is_error()) {
    call_status = child_dev_suspend_response->error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_EQ(child_dev_suspend_response->value()->cur_state,
            DevicePowerState::kDevicePowerStateD3Cold);

  // Verify that the suspend reason is received correctly
  auto suspend_reason_response = child2_device_client->GetCurrentSuspendReason();
  ASSERT_OK(suspend_reason_response.status());
  call_status = ZX_OK;
  if (suspend_reason_response->is_error()) {
    call_status = suspend_reason_response->error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_EQ(suspend_reason_response->value()->cur_suspend_reason, DEVICE_SUSPEND_REASON_REBOOT);

  // Wait till parent's suspend event is called.
  WaitForDeviceSuspendCompletion(parent_device_client);

  auto parent_dev_suspend_response = parent_device_client->GetCurrentDevicePowerState();
  ASSERT_OK(parent_dev_suspend_response.status());
  call_status = ZX_OK;
  if (parent_dev_suspend_response->is_error()) {
    call_status = parent_dev_suspend_response->error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_EQ(parent_dev_suspend_response->value()->cur_state,
            DevicePowerState::kDevicePowerStateD3Cold);
}

// TODO(http://fxbug.dev/119962): Re-enable this test after fixing.
// This test is not easy to replicate without a lot of plumbing changes to allow test to modify the
// response to fuchsia.device.manager/SystemStateTransition.GetTerminationSystemState.
TEST_F(PowerTestCase, DISABLED_SystemSuspend_SuspendReasonRebootRecovery) {
  // Add Capabilities
  DevicePowerStateInfo states[3];
  states[0].state_id = DevicePowerState::kDevicePowerStateD0;
  states[0].is_supported = true;
  states[0].restore_latency = 0;
  states[1].state_id = DevicePowerState::kDevicePowerStateD2;
  states[1].is_supported = true;
  states[1].restore_latency = 100;
  states[2].state_id = DevicePowerState::kDevicePowerStateD3Cold;
  states[2].is_supported = true;
  states[2].restore_latency = 1000;
  AddChildWithPowerArgs(states, std::size(states));

  // TODO(http://fxbug.dev/119962): Modify GetTerminationSystemState response.

  ASSERT_OK(devmgr.SuspendDriverManager());

  // Wait till child2's suspend event is called.
  WaitForDeviceSuspendCompletion(child2_device_client);

  auto child_dev_suspend_response = child2_device_client->GetCurrentDevicePowerState();
  ASSERT_OK(child_dev_suspend_response.status());
  auto call_status = ZX_OK;
  if (child_dev_suspend_response->is_error()) {
    call_status = child_dev_suspend_response->error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_EQ(child_dev_suspend_response->value()->cur_state,
            DevicePowerState::kDevicePowerStateD3Cold);

  auto suspend_reason_response = child2_device_client->GetCurrentSuspendReason();
  ASSERT_OK(suspend_reason_response.status());
  call_status = ZX_OK;
  if (suspend_reason_response->is_error()) {
    call_status = suspend_reason_response->error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_EQ(suspend_reason_response->value()->cur_suspend_reason,
            DEVICE_SUSPEND_REASON_REBOOT_RECOVERY);

  // Wait till parent's suspend event is called.
  WaitForDeviceSuspendCompletion(parent_device_client);
  auto parent_dev_suspend_response = parent_device_client->GetCurrentDevicePowerState();
  ASSERT_OK(parent_dev_suspend_response.status());
  call_status = ZX_OK;
  if (parent_dev_suspend_response->is_error()) {
    call_status = parent_dev_suspend_response->error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_EQ(parent_dev_suspend_response->value()->cur_state,
            DevicePowerState::kDevicePowerStateD3Cold);
}
