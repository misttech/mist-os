// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/shutdown/node_shutdown_coordinator.h"

#include "src/devices/bin/driver_manager/node.h"
#include "src/devices/bin/driver_manager/shutdown/node_removal_tracker.h"
#include "src/devices/lib/log/log.h"

namespace driver_manager {

namespace {

// The range of test delay time in milliseconds.
constexpr uint32_t kMinTestDelayMs = 0;
constexpr uint32_t kMaxTestDelayMs = 5;

}  // namespace

NodeShutdownCoordinator::NodeShutdownCoordinator(NodeShutdownBridge* bridge,
                                                 async_dispatcher_t* dispatcher,
                                                 bool enable_test_shutdown_delays,
                                                 std::weak_ptr<std::mt19937> rng_gen)
    : bridge_(bridge),
      enable_test_shutdown_delays_(enable_test_shutdown_delays),
      rng_gen_(rng_gen),
      distribution_(kMinTestDelayMs, kMaxTestDelayMs),
      tasks_(dispatcher) {}

void NodeShutdownCoordinator::Remove(std::shared_ptr<Node> node, RemovalSet removal_set,
                                     NodeRemovalTracker* removal_tracker) {
  std::stack<std::shared_ptr<Node>> nodes_to_check_for_removal;
  std::stack<std::pair<std::shared_ptr<Node>, RemovalSet>> nodes =
      std::stack<std::pair<std::shared_ptr<Node>, RemovalSet>>({{node, removal_set}});
  while (!nodes.empty()) {
    std::pair<std::shared_ptr<Node>, RemovalSet> removal_record = nodes.top();
    nodes.pop();
    std::shared_ptr<Node> node = removal_record.first;
    RemovalSet removal_set = removal_record.second;

    NodeShutdownCoordinator& node_shutdown_coordinator = node->GetNodeShutdownCoordinator();
    if (!removal_tracker && node_shutdown_coordinator.removal_tracker_) {
      // TODO(https://fxbug.dev/42066485): Change this to an error when we track shutdown steps
      // better.
      LOGF(WARNING, "Untracked Node::Remove() called on %s, indicating an error during shutdown",
           node->MakeTopologicalPath().c_str());
    }

    if (removal_tracker) {
      node_shutdown_coordinator.SetRemovalTracker(removal_tracker);
    }

    LOGF(DEBUG, "Remove called on Node: %s", node->name().c_str());
    // Two cases where we will transition state and take action:
    // Removing kAll, and state is Running or Prestop
    // Removing kPkg, and state is Running
    if ((node_shutdown_coordinator.node_state() != NodeState::kPrestop &&
         node_shutdown_coordinator.node_state() != NodeState::kRunning) ||
        (node_shutdown_coordinator.node_state() == NodeState::kPrestop &&
         removal_set == RemovalSet::kPackage)) {
      if (node->parents().size() <= 1) {
        LOGF(WARNING, "Node::Remove() %s called late, already in state %s",
             node->MakeComponentMoniker().c_str(), node_shutdown_coordinator.NodeStateAsString());
      }
      continue;
    }

    // Now, the cases where we do something:
    // Set the new state
    if (removal_set == RemovalSet::kPackage &&
        (node->collection() == Collection::kBoot || node->collection() == Collection::kNone)) {
      node_shutdown_coordinator.node_state_ = NodeState::kPrestop;
    } else {
      node_shutdown_coordinator.node_state_ = NodeState::kWaitingOnDriverBind;
      // Either removing kAll, or is package driver and removing kPackage.
      // All children should be removed regardless as they block removal of this node.
      removal_set = RemovalSet::kAll;
    }

    // Propagate removal message to children
    node_shutdown_coordinator.NotifyRemovalTracker();

    // Ask each of our children to remove themselves.
    for (auto& child : node->children()) {
      LOGF(DEBUG, "Node: %s calling remove on child: %s", node->name().c_str(),
           child->name().c_str());
      nodes.emplace(child, removal_set);
    }
    nodes_to_check_for_removal.push(std::move(node));
  }

  while (!nodes_to_check_for_removal.empty()) {
    nodes_to_check_for_removal.top()->GetNodeShutdownCoordinator().CheckNodeState();
    nodes_to_check_for_removal.pop();
  }
}

void NodeShutdownCoordinator::ResetShutdown() {
  ZX_ASSERT_MSG(node_state_ == NodeState::kStopped,
                "NodeShutdownCoordinator::ResetShutdown called in invalid node state: %s",
                NodeStateAsString());
  node_state_ = NodeState::kRunning;
  shutdown_intent_ = ShutdownIntent::kRemoval;
}

void NodeShutdownCoordinator::CheckNodeState() {
  if (is_transition_pending_) {
    return;
  }

  switch (node_state_) {
    case NodeState::kRunning:
    case NodeState::kPrestop:
    case NodeState::kStopped:
      return;
    case NodeState::kWaitingOnDriverBind: {
      CheckWaitingOnDriverBind();
      return;
    }
    case NodeState::kWaitingOnChildren: {
      CheckWaitingOnChildren();
      return;
    }
    case NodeState::kWaitingOnDriver: {
      CheckWaitingOnDriver();
      return;
    }
    case NodeState::kWaitingOnDriverComponent: {
      CheckWaitingOnDriverComponent();
      return;
    }
  }
}

void NodeShutdownCoordinator::CheckWaitingOnDriverBind() {
  ZX_ASSERT(!is_transition_pending_);
  ZX_ASSERT_MSG(
      node_state_ == NodeState::kWaitingOnDriverBind,
      "NodeShutdownCoordinator::CheckWaitingOnDriverBind called in invalid node state: %s",
      NodeStateAsString());
  // Remain on this state if the node still has children.
  if (bridge_->IsPendingBind()) {
    return;
  }
  PerformTransition([this]() mutable { UpdateAndNotifyState(NodeState::kWaitingOnChildren); });
}

void NodeShutdownCoordinator::CheckWaitingOnChildren() {
  ZX_ASSERT(!is_transition_pending_);
  ZX_ASSERT_MSG(node_state_ == NodeState::kWaitingOnChildren,
                "NodeShutdownCoordinator::CheckWaitingOnChildren called in invalid node state: %s",
                NodeStateAsString());
  // Remain on this state if the node still has children.
  if (bridge_->HasChildren()) {
    return;
  }
  PerformTransition([this]() mutable {
    bridge_->StopDriver();
    UpdateAndNotifyState(NodeState::kWaitingOnDriver);
  });
}

void NodeShutdownCoordinator::CheckWaitingOnDriver() {
  ZX_ASSERT(!is_transition_pending_);
  ZX_ASSERT_MSG(node_state_ == NodeState::kWaitingOnDriver,
                "NodeShutdownCoordinator::CheckWaitingOnDriver called in invalid node state: %s",
                NodeStateAsString());

  // Remain on this state if the node still has a driver set to it.
  if (bridge_->HasDriver()) {
    return;
  }
  PerformTransition([this]() mutable {
    bridge_->StopDriverComponent();
    UpdateAndNotifyState(NodeState::kWaitingOnDriverComponent);
  });
}

void NodeShutdownCoordinator::CheckWaitingOnDriverComponent() {
  ZX_ASSERT(!is_transition_pending_);
  ZX_ASSERT_MSG(
      node_state_ == NodeState::kWaitingOnDriverComponent,
      "NodeShutdownCoordinator::CheckWaitingOnDriverComponent called in invalid node state: %s",
      NodeStateAsString());

  // Remain on this state if the node still has a driver component.
  if (bridge_->HasDriverComponent()) {
    return;
  }

  PerformTransition([this]() mutable {
    bridge_->FinishShutdown([this]() { UpdateAndNotifyState(NodeState::kStopped); });
  });
}

void NodeShutdownCoordinator::PerformTransition(fit::function<void()> action) {
  ZX_ASSERT(!is_transition_pending_);

  // Generate a test delay. If there is none, perform the action and return.
  // Otherwise, perform the action asynchronously with the delay.
  std::optional<uint32_t> test_delay = GenerateTestDelayMs();
  if (!test_delay) {
    action();
    return;
  }

  is_transition_pending_ = true;
  tasks_.Post([action = std::move(action), delay_amount = test_delay.value()] {
    std::this_thread::sleep_for(std::chrono::microseconds(delay_amount));
    action();
  });
}

void NodeShutdownCoordinator::UpdateAndNotifyState(NodeState state) {
  node_state_ = state;
  is_transition_pending_ = false;
  NotifyRemovalTracker();
  CheckNodeState();
}

void NodeShutdownCoordinator::NotifyRemovalTracker() {
  if (!removal_tracker_ || removal_id_ == std::nullopt) {
    return;
  }
  removal_tracker_->Notify(removal_id_.value(), node_state_);
}

bool NodeShutdownCoordinator::IsShuttingDown() const { return node_state_ != NodeState::kRunning; }

const char* NodeShutdownCoordinator::NodeStateAsString(NodeState state) {
  switch (state) {
    case NodeState::kWaitingOnDriverBind:
      return "kWaitingOnDriverBind";
    case NodeState::kRunning:
      return "kRunning";
    case NodeState::kPrestop:
      return "kPrestop";
    case NodeState::kWaitingOnChildren:
      return "kWaitingOnChildren";
    case NodeState::kWaitingOnDriver:
      return "kWaitingOnDriver";
    case NodeState::kWaitingOnDriverComponent:
      return "kWaitingOnDriverComponent";
    case NodeState::kStopped:
      return "kStopped";
  }
}

void NodeShutdownCoordinator::SetRemovalTracker(NodeRemovalTracker* removal_tracker) {
  ZX_ASSERT(removal_tracker);
  if (removal_tracker_) {
    // We should never have two competing trackers
    ZX_ASSERT(removal_tracker_ == removal_tracker);
    return;
  }
  removal_tracker_ = removal_tracker;
  std::pair<std::string, Collection> node_info = bridge_->GetRemovalTrackerInfo();
  removal_id_ = removal_tracker->RegisterNode(NodeRemovalTracker::Node{
      .name = node_info.first,
      .collection = node_info.second,
      .state = node_state_,
  });
}

std::optional<uint32_t> NodeShutdownCoordinator::GenerateTestDelayMs() {
  if (!enable_test_shutdown_delays_) {
    return std::nullopt;
  }

  auto rng = rng_gen_.lock();
  if (!rng) {
    LOGF(WARNING, "Shutdown test RNG released. Unable to generate a test delay");
    return std::nullopt;
  }

  // Randomize a 20% chance for adding a shutdown delay.
  if (distribution_(*rng) % 5 == 1) {
    return distribution_(*rng);
  }
  return std::nullopt;
}

}  // namespace driver_manager
