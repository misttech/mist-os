// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/bootup_tracker.h"

#include "src/devices/bin/driver_manager/tests/bind_manager_test_base.h"

class TestBootupTracker : public driver_manager::BootupTracker {
 public:
  TestBootupTracker(driver_manager::BindManager* manager, async_dispatcher_t* dispatcher)
      : driver_manager::BootupTracker(manager, dispatcher) {}

  void ResetBootupTimer() override {}

  virtual bool IsUpdateDeadlineExceeded() const override { return should_exceed_update_deadline; }

  void TimeoutBootup() { OnBootupTimeout(); }

  bool should_exceed_update_deadline = false;
};

class BootupTrackerTest : public BindManagerTestBase {
 public:
  void SetUp() override {
    BindManagerTestBase::SetUp();
    tracker = std::make_unique<TestBootupTracker>(bind_manager(), dispatcher());
    tracker->Start();
  }

  void TriggerBootupTimeout() {
    tracker->TimeoutBootup();
    RunLoopUntilIdle();
  }

  void WaitForBootup() {
    tracker->WaitForBootup([this]() { bootup_completed_ = true; });
  }

  bool bootup_completed() const { return bootup_completed_; }

 protected:
  std::unique_ptr<TestBootupTracker> tracker;

 private:
  bool bootup_completed_ = false;
};

TEST_F(BootupTrackerTest, NoDrivers) {
  WaitForBootup();
  TriggerBootupTimeout();
  EXPECT_TRUE(bootup_completed());
}

TEST_F(BootupTrackerTest, StartRequestsOnly) {
  WaitForBootup();

  tracker->NotifyNewStartRequest("node_1", "driver_url");
  TriggerBootupTimeout();
  EXPECT_FALSE(bootup_completed());

  tracker->NotifyNewStartRequest("node_2", "driver_url");
  tracker->NotifyStartComplete("node_1");
  TriggerBootupTimeout();
  EXPECT_FALSE(bootup_completed());

  tracker->NotifyStartComplete("node_2");
  TriggerBootupTimeout();
  EXPECT_TRUE(bootup_completed());
}

TEST_F(BootupTrackerTest, StartAndBindRequests) {
  WaitForBootup();

  // Invoke bind for a new node in the bind manager.
  AddAndBindNode_EXPECT_BIND_START("node-a");
  VerifyBindOngoingWithRequests({{"node-a", 1}});

  // Bootup shouldn't be complete with an ongoing bind process.
  TriggerBootupTimeout();
  EXPECT_FALSE(bootup_completed());

  // Add a new start request for node_1.
  tracker->NotifyNewStartRequest("node_1", "driver_url");
  TriggerBootupTimeout();
  EXPECT_FALSE(bootup_completed());

  // Add a new start request for node_2 and complete node_1.
  tracker->NotifyNewStartRequest("node_2", "driver_url");
  tracker->NotifyStartComplete("node_1");
  TriggerBootupTimeout();
  EXPECT_FALSE(bootup_completed());

  // Complete node_2.
  tracker->NotifyStartComplete("node_2");
  TriggerBootupTimeout();
  EXPECT_FALSE(bootup_completed());

  // Complete the ongoing bind. Bootup should be complete.
  DriverIndexReplyWithDriver("node-a");
  VerifyNoOngoingBind();
  TriggerBootupTimeout();
  EXPECT_TRUE(bootup_completed());
}

TEST_F(BootupTrackerTest, OverlappingBindRequests) {
  WaitForBootup();

  AddAndOrphanNode("node-a");
  AddAndOrphanNode("node-b");

  // Invoke TryBindAllAvailable().
  InvokeTryBindAllAvailable_EXPECT_BIND_START();
  VerifyBindOngoingWithRequests({{"node-a", 1}, {"node-b", 1}});
  TriggerBootupTimeout();
  EXPECT_FALSE(bootup_completed());

  AddAndBindNode_EXPECT_QUEUED("node-c");
  TriggerBootupTimeout();
  EXPECT_FALSE(bootup_completed());

  // Complete the ongoing bind. This should kickstart another ongoing bind.
  DriverIndexReplyWithDriver("node-b");
  DriverIndexReplyWithDriver("node-a");
  VerifyBindOngoingWithRequests({{"node-c", 1}});
  TriggerBootupTimeout();
  EXPECT_FALSE(bootup_completed());

  // Complete the ongoing bind. Bootup should be complete.
  DriverIndexReplyWithDriver("node-c");
  VerifyNoOngoingBind();
  TriggerBootupTimeout();
  EXPECT_TRUE(bootup_completed());
}

TEST_F(BootupTrackerTest, WaitForBootupAfterComplete) {
  tracker->NotifyNewStartRequest("node_1", "driver_url");
  tracker->NotifyStartComplete("node_1");
  TriggerBootupTimeout();

  // If bootup already completed. then the wait call should immediately succeed.
  WaitForBootup();
  RunLoopUntilIdle();
  EXPECT_TRUE(bootup_completed());
}
