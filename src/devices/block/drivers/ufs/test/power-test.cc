// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "unit-lib.h"
#include "zxtest/zxtest.h"

namespace ufs {
using namespace ufs_mock_device;

class PowerTest : public UfsTest {
 public:
  void SetUp() override { InitMockDevice(); }
};

TEST_F(PowerTest, PowerSuspendResume) {
  libsync::Completion sleep_complete;
  libsync::Completion awake_complete;
  mock_device_.GetUicCmdProcessor().SetHook(
      UicCommandOpcode::kDmeHibernateEnter,
      [&](UfsMockDevice& mock_device, uint32_t ucmdarg1, uint32_t ucmdarg2, uint32_t ucmdarg3) {
        mock_device_.GetUicCmdProcessor().DefaultDmeHibernateEnterHandler(mock_device, ucmdarg1,
                                                                          ucmdarg2, ucmdarg3);
        sleep_complete.Signal();
      });
  mock_device_.GetUicCmdProcessor().SetHook(
      UicCommandOpcode::kDmeHibernateExit,
      [&](UfsMockDevice& mock_device, uint32_t ucmdarg1, uint32_t ucmdarg2, uint32_t ucmdarg3) {
        mock_device_.GetUicCmdProcessor().DefaultDmeHibernateExitHandler(mock_device, ucmdarg1,
                                                                         ucmdarg2, ucmdarg3);
        awake_complete.Signal();
      });

  ASSERT_NO_FATAL_FAILURE(StartDriver(/*supply_power_framework=*/true));

  scsi::BlockDevice* block_device;
  block_info_t info;
  uint64_t op_size;
  const auto& block_devs = dut_->block_devs();
  block_device = block_devs.at(0).at(0).get();
  block_device->BlockImplQuery(&info, &op_size);

  // 1. Initial power level is kPowerLevelOff.
  runtime_.PerformBlockingWork([&] { sleep_complete.Wait(); });

  // TODO(https://fxbug.dev/42075643): Check if suspend is enabled with inspect
  ASSERT_FALSE(dut_->IsResumed());
  UfsPowerMode power_mode = UfsPowerMode::kSleep;
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerMode(), power_mode);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerCondition(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].first);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentLinkState(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].second);

  const zx::vmo inspect_vmo = dut_->inspect().DuplicateVmo();
  ASSERT_TRUE(inspect_vmo.is_valid());

  inspect::InspectTestHelper inspector;
  inspector.ReadInspect(inspect_vmo);

  const inspect::Hierarchy* root = inspector.hierarchy().GetByPath({"ufs"});
  ASSERT_NOT_NULL(root);

  const auto* power_suspended =
      root->node().get_property<inspect::BoolPropertyValue>("power_suspended");
  ASSERT_NOT_NULL(power_suspended);
  EXPECT_TRUE(power_suspended->value());
  const auto* wake_on_request_count =
      root->node().get_property<inspect::UintPropertyValue>("wake_on_request_count");
  ASSERT_NOT_NULL(wake_on_request_count);
  EXPECT_EQ(wake_on_request_count->value(), 0);

  // 2. Issue request while power is suspended.
  awake_complete.Reset();
  sleep_complete.Reset();

  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(ufs_mock_device::kMockBlockSize, 0, &vmo));
  zx_vaddr_t vaddr;
  ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                                       ufs_mock_device::kMockBlockSize, &vaddr));
  char* mapped_vaddr = reinterpret_cast<char*>(vaddr);
  std::strncpy(mapped_vaddr, "test", ufs_mock_device::kMockBlockSize);

  auto block_op = std::make_unique<uint8_t[]>(op_size);
  auto op = reinterpret_cast<block_op_t*>(block_op.get());
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_WRITE,
                  },
              .vmo = vmo.get(),
              .length = 1,
              .offset_dev = 0,
              .offset_vmo = 0,
          },
  };
  block_device->BlockImplQueue(op, callback, &done);
  runtime_.PerformBlockingWork([&] { awake_complete.Wait(); });
  sync_completion_wait(&done, ZX_TIME_INFINITE);

  // Return the driver to the suspended state.
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    incoming->power_broker.hardware_power_required_level_->required_level_ = Ufs::kPowerLevelOff;
  });
  runtime_.PerformBlockingWork([&] { sleep_complete.Wait(); });

  ASSERT_FALSE(dut_->IsResumed());
  power_mode = UfsPowerMode::kSleep;
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerMode(), power_mode);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerCondition(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].first);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentLinkState(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].second);

  inspector.ReadInspect(inspect_vmo);

  root = inspector.hierarchy().GetByPath({"ufs"});
  ASSERT_NOT_NULL(root);

  power_suspended = root->node().get_property<inspect::BoolPropertyValue>("power_suspended");
  ASSERT_NOT_NULL(power_suspended);
  EXPECT_TRUE(power_suspended->value());
  wake_on_request_count =
      root->node().get_property<inspect::UintPropertyValue>("wake_on_request_count");
  ASSERT_NOT_NULL(wake_on_request_count);
  EXPECT_EQ(wake_on_request_count->value(), 1);

  // 3. Trigger power level change to kPowerLevelOn.
  awake_complete.Reset();
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    incoming->power_broker.hardware_power_required_level_->required_level_ = Ufs::kPowerLevelOn;
  });
  runtime_.PerformBlockingWork([&] { awake_complete.Wait(); });

  ASSERT_TRUE(dut_->IsResumed());
  power_mode = UfsPowerMode::kActive;
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerMode(), power_mode);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerCondition(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].first);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentLinkState(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].second);

  inspector.ReadInspect(inspect_vmo);

  root = inspector.hierarchy().GetByPath({"ufs"});
  ASSERT_NOT_NULL(root);

  power_suspended = root->node().get_property<inspect::BoolPropertyValue>("power_suspended");
  ASSERT_NOT_NULL(power_suspended);
  EXPECT_FALSE(power_suspended->value());
  wake_on_request_count =
      root->node().get_property<inspect::UintPropertyValue>("wake_on_request_count");
  ASSERT_NOT_NULL(wake_on_request_count);
  EXPECT_EQ(wake_on_request_count->value(), 1);

  // 4. Trigger power level change to kPowerLevelOff.
  sleep_complete.Reset();
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    incoming->power_broker.hardware_power_required_level_->required_level_ = Ufs::kPowerLevelOff;
  });
  runtime_.PerformBlockingWork([&] { sleep_complete.Wait(); });

  ASSERT_FALSE(dut_->IsResumed());
  power_mode = UfsPowerMode::kSleep;
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerMode(), power_mode);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerCondition(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].first);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentLinkState(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].second);

  inspector.ReadInspect(inspect_vmo);

  root = inspector.hierarchy().GetByPath({"ufs"});
  ASSERT_NOT_NULL(root);

  power_suspended = root->node().get_property<inspect::BoolPropertyValue>("power_suspended");
  ASSERT_NOT_NULL(power_suspended);
  EXPECT_TRUE(power_suspended->value());
  wake_on_request_count =
      root->node().get_property<inspect::UintPropertyValue>("wake_on_request_count");
  ASSERT_NOT_NULL(wake_on_request_count);
  EXPECT_EQ(wake_on_request_count->value(), 1);
}

TEST_F(PowerTest, BackgroundOperations) {
  libsync::Completion sleep_complete;
  libsync::Completion awake_complete;
  mock_device_.GetUicCmdProcessor().SetHook(
      UicCommandOpcode::kDmeHibernateEnter,
      [&](UfsMockDevice& mock_device, uint32_t ucmdarg1, uint32_t ucmdarg2, uint32_t ucmdarg3) {
        mock_device_.GetUicCmdProcessor().DefaultDmeHibernateEnterHandler(mock_device, ucmdarg1,
                                                                          ucmdarg2, ucmdarg3);
        sleep_complete.Signal();
      });
  mock_device_.GetUicCmdProcessor().SetHook(
      UicCommandOpcode::kDmeHibernateExit,
      [&](UfsMockDevice& mock_device, uint32_t ucmdarg1, uint32_t ucmdarg2, uint32_t ucmdarg3) {
        mock_device_.GetUicCmdProcessor().DefaultDmeHibernateExitHandler(mock_device, ucmdarg1,
                                                                         ucmdarg2, ucmdarg3);
        awake_complete.Signal();
      });

  ASSERT_NO_FATAL_FAILURE(StartDriver(/*supply_power_framework=*/true));

  // 1. Background Operation is disabled at power off
  runtime_.PerformBlockingWork([&] { sleep_complete.Wait(); });

  ASSERT_FALSE(dut_->IsResumed());
  UfsPowerMode power_mode = UfsPowerMode::kSleep;
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerMode(), power_mode);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerCondition(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].first);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentLinkState(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].second);

  const zx::vmo inspect_vmo = dut_->inspect().DuplicateVmo();
  ASSERT_TRUE(inspect_vmo.is_valid());

  inspect::InspectTestHelper inspector;
  inspector.ReadInspect(inspect_vmo);

  const inspect::Hierarchy* root = inspector.hierarchy().GetByPath({"ufs"});
  ASSERT_NOT_NULL(root);

  const auto* power_suspended =
      root->node().get_property<inspect::BoolPropertyValue>("power_suspended");
  ASSERT_NOT_NULL(power_suspended);
  EXPECT_TRUE(power_suspended->value());

  const auto* bkop_node = root->GetByPath({"controller"})->GetByPath({"background_operations"});
  const auto* is_background_op_enabled =
      bkop_node->node().get_property<inspect::BoolPropertyValue>("is_background_op_enabled");
  ASSERT_NOT_NULL(is_background_op_enabled);
  EXPECT_FALSE(is_background_op_enabled->value());

  // 2. Background Operation is enabled at power on
  awake_complete.Reset();
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    incoming->power_broker.hardware_power_required_level_->required_level_ = Ufs::kPowerLevelOn;
  });
  runtime_.PerformBlockingWork([&] { awake_complete.Wait(); });

  ASSERT_TRUE(dut_->IsResumed());
  power_mode = UfsPowerMode::kActive;
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerMode(), power_mode);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerCondition(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].first);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentLinkState(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].second);

  inspector.ReadInspect(inspect_vmo);

  root = inspector.hierarchy().GetByPath({"ufs"});
  power_suspended = root->node().get_property<inspect::BoolPropertyValue>("power_suspended");
  EXPECT_FALSE(power_suspended->value());

  bkop_node = root->GetByPath({"controller"})->GetByPath({"background_operations"});
  is_background_op_enabled =
      bkop_node->node().get_property<inspect::BoolPropertyValue>("is_background_op_enabled");
  EXPECT_TRUE(is_background_op_enabled->value());

  // 3. Background operations change from disabled to enabled when an Urgent Background Operation
  // Exception Event occurs.
  ASSERT_OK(DisableBackgroundOp());

  inspector.ReadInspect(inspect_vmo);
  root = inspector.hierarchy().GetByPath({"ufs"});
  bkop_node = root->GetByPath({"controller"})->GetByPath({"background_operations"});
  is_background_op_enabled =
      bkop_node->node().get_property<inspect::BoolPropertyValue>("is_background_op_enabled");
  EXPECT_FALSE(is_background_op_enabled->value());

  // Using Exception Event to trigger Urgent Background Operations.
  mock_device_.SetExceptionEventAlert(true);
  ExceptionEventStatus ee_status = {0};
  ee_status.set_urgent_bkops(true);
  mock_device_.SetAttribute(Attributes::wExceptionEventStatus,
                            static_cast<uint32_t>(ee_status.value));
  mock_device_.SetAttribute(Attributes::bBackgroundOpStatus,
                            static_cast<uint32_t>(BackgroundOpStatus::kCritical));

  // To check for an Exception Event, send a command.
  auto attribute = ReadAttribute(Attributes::bBackgroundOpStatus);
  EXPECT_OK(attribute);

  // Wait for exception event completion
  auto wait_for = [&]() -> bool {
    // Check that Background Operations is enabled
    inspector.ReadInspect(inspect_vmo);
    root = inspector.hierarchy().GetByPath({"ufs"});
    bkop_node = root->GetByPath({"controller"})->GetByPath({"background_operations"});
    is_background_op_enabled =
        bkop_node->node().get_property<inspect::BoolPropertyValue>("is_background_op_enabled");
    return is_background_op_enabled->value();
  };
  fbl::String timeout_message = "Timeout waiting for enabling Background Op";
  constexpr uint32_t kTimeoutUs = 1000000;
  ASSERT_OK(dut_->WaitWithTimeout(wait_for, kTimeoutUs, timeout_message));

  // Clean up
  mock_device_.SetExceptionEventAlert(false);

  // 4. Background Operation is disabled at power off
  sleep_complete.Reset();
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    incoming->power_broker.hardware_power_required_level_->required_level_ = Ufs::kPowerLevelOff;
  });
  runtime_.PerformBlockingWork([&] { sleep_complete.Wait(); });

  ASSERT_FALSE(dut_->IsResumed());
  power_mode = UfsPowerMode::kSleep;
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerMode(), power_mode);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentPowerCondition(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].first);
  ASSERT_EQ(dut_->GetDeviceManager().GetCurrentLinkState(),
            dut_->GetDeviceManager().GetPowerModeMap()[power_mode].second);

  inspector.ReadInspect(inspect_vmo);

  root = inspector.hierarchy().GetByPath({"ufs"});
  power_suspended = root->node().get_property<inspect::BoolPropertyValue>("power_suspended");
  EXPECT_TRUE(power_suspended->value());

  bkop_node = root->GetByPath({"controller"})->GetByPath({"background_operations"});
  is_background_op_enabled =
      bkop_node->node().get_property<inspect::BoolPropertyValue>("is_background_op_enabled");
  EXPECT_FALSE(is_background_op_enabled->value());
}

}  // namespace ufs
