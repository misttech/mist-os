// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/node_removal_tracker.h"

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

struct NodeBank {
  NodeBank(dfv2::NodeRemovalTracker *tracker) : tracker_(tracker) {}
  void AddNode(dfv2::Collection collection, dfv2::NodeState state) {
    ids_.insert(tracker_->RegisterNode(dfv2::NodeRemovalTracker::Node{
        .name = "node",
        .collection = collection,
        .state = state,
    }));
  }

  void NotifyRemovalComplete() {
    for (dfv2::NodeId id : ids_) {
      tracker_->Notify(id, dfv2::NodeState::kStopped);
    }
  }

  std::set<dfv2::NodeId> ids_;
  dfv2::NodeRemovalTracker *tracker_;
};

class NodeRemovalTrackerTest : public gtest::TestLoopFixture {};

TEST_F(NodeRemovalTrackerTest, RegisterOneNode) {
  dfv2::NodeRemovalTracker tracker(dispatcher());
  dfv2::NodeId id = tracker.RegisterNode(dfv2::NodeRemovalTracker::Node{
      .name = "node",
      .collection = dfv2::Collection::kBoot,
      .state = dfv2::NodeState::kRunning,
  });
  int package_callbacks = 0;
  int all_callbacks = 0;
  tracker.set_pkg_callback([&package_callbacks]() { package_callbacks++; });
  tracker.set_all_callback([&all_callbacks]() { all_callbacks++; });
  tracker.FinishEnumeration();
  tracker.Notify(id, dfv2::NodeState::kStopped);

  EXPECT_EQ(package_callbacks, 1);
  EXPECT_EQ(all_callbacks, 1);
}

TEST_F(NodeRemovalTrackerTest, RegisterManyNodes) {
  dfv2::NodeRemovalTracker tracker(dispatcher());
  NodeBank node_bank(&tracker);
  node_bank.AddNode(dfv2::Collection::kBoot, dfv2::NodeState::kRunning);
  node_bank.AddNode(dfv2::Collection::kBoot, dfv2::NodeState::kRunning);
  node_bank.AddNode(dfv2::Collection::kPackage, dfv2::NodeState::kRunning);
  node_bank.AddNode(dfv2::Collection::kPackage, dfv2::NodeState::kRunning);
  int package_callbacks = 0;
  int all_callbacks = 0;
  tracker.set_pkg_callback([&package_callbacks]() { package_callbacks++; });
  tracker.set_all_callback([&all_callbacks]() { all_callbacks++; });
  tracker.FinishEnumeration();
  EXPECT_EQ(package_callbacks, 0);
  EXPECT_EQ(all_callbacks, 0);
  node_bank.NotifyRemovalComplete();

  EXPECT_EQ(package_callbacks, 1);
  EXPECT_EQ(all_callbacks, 1);
}

// Make sure package callback is only called when package drivers stop
// and all callback is only called when all drivers stop
TEST_F(NodeRemovalTrackerTest, CallbacksCallOrder) {
  dfv2::NodeRemovalTracker tracker(dispatcher());
  NodeBank boot_node_bank(&tracker), package_node_bank(&tracker);
  boot_node_bank.AddNode(dfv2::Collection::kBoot, dfv2::NodeState::kRunning);
  boot_node_bank.AddNode(dfv2::Collection::kBoot, dfv2::NodeState::kRunning);
  package_node_bank.AddNode(dfv2::Collection::kPackage, dfv2::NodeState::kRunning);
  package_node_bank.AddNode(dfv2::Collection::kPackage, dfv2::NodeState::kRunning);
  int package_callbacks = 0;
  int all_callbacks = 0;
  tracker.set_pkg_callback([&package_callbacks]() { package_callbacks++; });
  tracker.set_all_callback([&all_callbacks]() { all_callbacks++; });
  EXPECT_EQ(package_callbacks, 0);
  EXPECT_EQ(all_callbacks, 0);
  tracker.FinishEnumeration();

  package_node_bank.NotifyRemovalComplete();

  EXPECT_EQ(package_callbacks, 1);
  EXPECT_EQ(all_callbacks, 0);

  boot_node_bank.NotifyRemovalComplete();

  EXPECT_EQ(package_callbacks, 1);
  EXPECT_EQ(all_callbacks, 1);
}

// This tests verifies that set_all_callback can be called
// during the pkg_callback without causing a deadlock.
TEST_F(NodeRemovalTrackerTest, CallbackDeadlock) {
  dfv2::NodeRemovalTracker tracker(dispatcher());
  dfv2::NodeId id = tracker.RegisterNode(dfv2::NodeRemovalTracker::Node{
      .name = "node",
      .collection = dfv2::Collection::kBoot,
      .state = dfv2::NodeState::kRunning,
  });
  int package_callbacks = 0;
  int all_callbacks = 0;
  tracker.set_pkg_callback([&tracker, &package_callbacks, &all_callbacks]() {
    package_callbacks++;
    tracker.set_all_callback([&all_callbacks]() { all_callbacks++; });
  });
  tracker.FinishEnumeration();
  tracker.Notify(id, dfv2::NodeState::kStopped);

  EXPECT_EQ(package_callbacks, 1);
  EXPECT_EQ(all_callbacks, 1);
}

// Make sure callbacks are not called until FinishEnumeration is called
TEST_F(NodeRemovalTrackerTest, FinishEnumeration) {
  dfv2::NodeRemovalTracker tracker(dispatcher());
  NodeBank node_bank(&tracker);
  node_bank.AddNode(dfv2::Collection::kBoot, dfv2::NodeState::kRunning);
  node_bank.AddNode(dfv2::Collection::kBoot, dfv2::NodeState::kRunning);
  node_bank.AddNode(dfv2::Collection::kPackage, dfv2::NodeState::kRunning);
  node_bank.AddNode(dfv2::Collection::kPackage, dfv2::NodeState::kRunning);
  int package_callbacks = 0;
  int all_callbacks = 0;
  tracker.set_pkg_callback([&package_callbacks]() { package_callbacks++; });
  tracker.set_all_callback([&all_callbacks]() { all_callbacks++; });
  EXPECT_EQ(package_callbacks, 0);
  EXPECT_EQ(all_callbacks, 0);
  node_bank.NotifyRemovalComplete();

  EXPECT_EQ(package_callbacks, 0);
  EXPECT_EQ(all_callbacks, 0);
  tracker.FinishEnumeration();

  EXPECT_EQ(package_callbacks, 1);
  EXPECT_EQ(all_callbacks, 1);
}
