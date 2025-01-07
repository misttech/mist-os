// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.component/cpp/wire_test_base.h>

#include "src/devices/bin/driver_manager/driver_host.h"
#include "src/devices/bin/driver_manager/shutdown/node_removal_tracker.h"
#include "src/devices/bin/driver_manager/tests/driver_manager_test_base.h"

using namespace driver_manager;

class TestRealm final : public fidl::testing::WireTestBase<fuchsia_component::Realm> {
 public:
  TestRealm(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  fidl::ClientEnd<fuchsia_component::Realm> Connect() {
    auto [client_end, server_end] = fidl::Endpoints<fuchsia_component::Realm>::Create();
    fidl::BindServer(dispatcher_, std::move(server_end), this);
    return std::move(client_end);
  }

  void DestroyChild(DestroyChildRequestView request,
                    DestroyChildCompleter::Sync& completer) override {
    destroy_completers_[std::string(request->child.name.data(), request->child.name.size())] =
        completer.ToAsync();
  }

  void ReplyDestroyChildRequest(std::string child_moniker) {
    ASSERT_TRUE(destroy_completers_[child_moniker].has_value());
    destroy_completers_[child_moniker]->ReplySuccess();
    destroy_completers_[child_moniker].reset();
  }

 private:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ZX_PANIC("Unimplemented %s", name.c_str());
  }

  async_dispatcher_t* dispatcher_;

  std::unordered_map<std::string, std::optional<DestroyChildCompleter::Async>> destroy_completers_;
};

class FakeDriverHost : public DriverHost {
 public:
  using StartCallback = fit::callback<void(zx::result<>)>;
  void Start(fidl::ClientEnd<fuchsia_driver_framework::Node> client_end, std::string node_name,
             fuchsia_driver_framework::wire::NodePropertyDictionary2 node_properties,
             fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol> symbols,
             fidl::VectorView<fuchsia_driver_framework::wire::Offer> offers,
             fuchsia_component_runner::wire::ComponentStartInfo start_info, zx::event node_token,
             fidl::ServerEnd<fuchsia_driver_host::Driver> driver, StartCallback cb) override {
    drivers_[node_name] = std::move(driver);
    clients_[node_name] = std::move(client_end);

    if (should_queue_start_callback_) {
      start_callbacks_[node_name] = std::move(cb);
      return;
    }
    cb(zx::ok());
  }

  zx::result<uint64_t> GetProcessKoid() const override { return zx::error(ZX_ERR_NOT_SUPPORTED); }

  void CloseDriver(std::string node_name) {
    drivers_[node_name].Close(ZX_OK);
    clients_[node_name].reset();
  }

  void InvokeStartCallback(std::string node_name, zx::result<> result) {
    start_callbacks_[node_name](result);
    start_callbacks_.erase(node_name);
  }

  void set_should_queue_start_callback(bool should_queue) {
    should_queue_start_callback_ = should_queue;
  }

 private:
  bool should_queue_start_callback_ = false;
  std::unordered_map<std::string, StartCallback> start_callbacks_;

  std::unordered_map<std::string, fidl::ServerEnd<fuchsia_driver_host::Driver>> drivers_;
  std::unordered_map<std::string, fidl::ClientEnd<fuchsia_driver_framework::Node>> clients_;
};

class FakeNodeManager : public TestNodeManagerBase {
 public:
  FakeNodeManager(fidl::WireClient<fuchsia_component::Realm> realm) : realm_(std::move(realm)) {}

  zx::result<DriverHost*> CreateDriverHost(bool use_next_vdso) override {
    return zx::ok(&driver_host_);
  }

  void DestroyDriverComponent(
      Node& node,
      fit::callback<void(fidl::WireUnownedResult<fuchsia_component::Realm::DestroyChild>& result)>
          callback) override {
    auto name = node.MakeComponentMoniker();
    fuchsia_component_decl::wire::ChildRef child_ref{
        .name = fidl::StringView::FromExternal(name),
        .collection = "",
    };
    realm_->DestroyChild(child_ref).Then(std::move(callback));
    clients_.erase(node.name());
  }

  void CloseDriverForNode(std::string node_name) { driver_host_.CloseDriver(node_name); }

  void AddClient(const std::string& node_name,
                 fidl::ClientEnd<fuchsia_component_runner::ComponentController> client) {
    clients_[node_name] = std::move(client);
  }

  FakeDriverHost& driver_host() { return driver_host_; }

 private:
  fidl::WireClient<fuchsia_component::Realm> realm_;
  std::unordered_map<std::string, fidl::ClientEnd<fuchsia_component_runner::ComponentController>>
      clients_;
  FakeDriverHost driver_host_;
};

class NodeShutdownTest : public DriverManagerTestBase {
 public:
  void SetUp() override {
    DriverManagerTestBase::SetUp();
    realm_ = std::make_unique<TestRealm>(dispatcher());

    node_manager = std::make_unique<FakeNodeManager>(
        fidl::WireClient<fuchsia_component::Realm>(realm_->Connect(), dispatcher()));

    removal_tracker_ = std::make_unique<NodeRemovalTracker>(dispatcher());
    removal_tracker_->set_all_callback([this]() { remove_all_callback_invoked_ = true; });
    removal_tracker_->set_pkg_callback([this]() { remove_pkg_callback_invoked_ = true; });

    nodes_["root"] = root();
  }

  void StartDriver(std::string node_name) {
    ASSERT_NE(nodes_.find(node_name), nodes_.end());

    std::vector<fuchsia_data::DictionaryEntry> program_entries = {
        {{
            .key = "binary",
            .value = std::make_unique<fuchsia_data::DictionaryValue>(
                fuchsia_data::DictionaryValue::WithStr("driver/library.so")),
        }},
        {{
            .key = "colocate",
            .value = std::make_unique<fuchsia_data::DictionaryValue>(
                fuchsia_data::DictionaryValue::WithStr("false")),
        }},
    };

    auto [_, server_end] = fidl::Endpoints<fuchsia_io::Directory>::Create();

    auto start_info = fuchsia_component_runner::ComponentStartInfo{{
        .resolved_url = node_name,
        .program = fuchsia_data::Dictionary{{.entries = std::move(program_entries)}},
        .outgoing_dir = std::move(server_end),
    }};

    auto controller_endpoints =
        fidl::Endpoints<fuchsia_component_runner::ComponentController>::Create();

    auto node = nodes_[node_name].lock();
    ASSERT_TRUE(node);

    node_manager->AddClient(node->name(), std::move(controller_endpoints.client));

    fidl::Arena arena;
    node->StartDriver(fidl::ToWire(arena, std::move(start_info)),
                      std::move(controller_endpoints.server),
                      [node](zx::result<> result) { node->CompleteBind(result); });
    RunLoopUntilIdle();
  }

  void AddNode(std::string node) { AddChildNode("root", node); }

  void AddNodeAndStartDriver(std::string node) { AddNodeAndStartDriver(node, std::nullopt); }

  void AddNodeAndStartDriver(std::string node, std::optional<Collection> collection) {
    AddChildNodeAndStartDriver("root", node, collection);
  }

  void AddChildNode(std::string parent_name, std::string child_name) {
    AddChildNode(parent_name, child_name, std::nullopt);
  }

  void AddChildNode(std::string parent_name, std::string child_name,
                    std::optional<Collection> collection) {
    // This function should only be called for a new node.
    ASSERT_EQ(nodes_.find(child_name), nodes_.end());
    ASSERT_NE(nodes_.find(parent_name), nodes_.end());

    // For testing purposes, the parent should not contain the children with the same node names.
    auto parent = nodes_[parent_name].lock();
    ASSERT_TRUE(parent);
    for (auto child : parent->children()) {
      ASSERT_NE(child->name(), child_name);
    }
    std::shared_ptr<Node> child =
        DriverManagerTestBase::CreateNode(child_name, nodes_[parent_name]);
    if (collection.has_value()) {
      child->set_collection(collection.value());
    }
    nodes_[child_name] = child;
  }

  void AddCompositeNode(std::string composite_name, std::vector<std::string> parents) {
    ASSERT_EQ(nodes_.find(composite_name), nodes_.end());

    std::vector<std::weak_ptr<Node>> parent_nodes;
    parent_nodes.reserve(parents.size());
    for (auto& parent_name : parents) {
      ASSERT_NE(nodes_.find(parent_name), nodes_.end());
      parent_nodes.push_back(nodes_[parent_name]);
    }
    std::vector<fuchsia_driver_framework::NodePropertyEntry2> parent_properties(parents.size());
    nodes_[composite_name] = CreateCompositeNode(composite_name, parent_nodes, parent_properties,
                                                 /* primary_index */ 0);
  }

  std::shared_ptr<Node> GetNode(std::string node_name) { return nodes_[node_name].lock(); }

  void AddChildNodeAndStartDriver(std::string parent, std::string child) {
    AddChildNodeAndStartDriver(parent, child, std::nullopt);
  }

  void AddChildNodeAndStartDriver(std::string parent, std::string child,
                                  std::optional<Collection> collection) {
    AddChildNode(parent, child, collection);
    StartDriver(child);
  }

  void InvokeDestroyChildResponse(std::string node_name) {
    auto node = nodes_[node_name].lock();
    ASSERT_TRUE(node);
    realm_->ReplyDestroyChildRequest(node->MakeComponentMoniker());
    RunLoopUntilIdle();
  }

  void CloseDriverForNode(std::string node_name) {
    node_manager->CloseDriverForNode(node_name);
    RunLoopUntilIdle();
  }

  void InvokeRemoveNode(std::string node_name) { InvokeRemoveNode(node_name, RemovalSet::kAll); }

  void InvokeRemoveNode(std::string node_name, RemovalSet set) {
    auto node = nodes_[node_name].lock();
    ASSERT_TRUE(node);
    node->Remove(set, removal_tracker_.get());
    removal_tracker_->FinishEnumeration();
    RunLoopUntilIdle();
  }

  void VerifyState(std::string node_name, NodeState expected_state) {
    RunLoopUntilIdle();
    auto node = nodes_[node_name].lock();
    ASSERT_TRUE(node);
    ASSERT_EQ(expected_state, node->GetShutdownHelper().node_state())
        << "Node: " << node_name
        << "  Expected: " << ShutdownHelper::NodeStateAsString(expected_state)
        << "  Actual: " << node->GetShutdownHelper().NodeStateAsString();
  }

  void VerifyStates(std::map<std::string, NodeState> expected_states) {
    for (const auto& [node_name, state] : expected_states) {
      VerifyState(node_name, state);
    }
  }

  void VerifyNodeRemovedFromParent(std::string node_name, std::string parent_name) {
    RunLoopUntilIdle();
    ASSERT_FALSE(nodes_[node_name].lock())
        << " node_name: " << node_name << " parent_name: " << parent_name;
    if (auto parent = nodes_[parent_name].lock(); parent) {
      for (auto child : parent->children()) {
        ASSERT_NE(child->name(), node_name);
      }
    }
  }

  void VerifyRemovalTrackerPkgCallbackInvoked() { ASSERT_TRUE(remove_pkg_callback_invoked_); }

  void VerifyRemovalTrackerPkgCallbackNotInvoked() { ASSERT_FALSE(remove_pkg_callback_invoked_); }

  void VerifyRemovalTrackerAllCallbackInvoked() { ASSERT_TRUE(remove_all_callback_invoked_); }

  void VerifyRemovalTrackerAllCallbackNotInvoked() { ASSERT_FALSE(remove_all_callback_invoked_); }

 protected:
  NodeManager* GetNodeManager() override { return node_manager.get(); }

  TestRealm* realm() { return realm_.get(); }

  std::unique_ptr<FakeNodeManager> node_manager;

 private:
  std::unique_ptr<NodeRemovalTracker> removal_tracker_;

  bool remove_all_callback_invoked_ = false;

  bool remove_pkg_callback_invoked_ = false;

  std::unordered_map<std::string, std::weak_ptr<Node>> nodes_;

  std::unique_ptr<TestRealm> realm_;
};

TEST_F(NodeShutdownTest, BasicRemoveAllNodes) {
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");
  AddChildNodeAndStartDriver("node_a", "node_a_b");
  AddChildNodeAndStartDriver("node_a_b", "node_a_b_a");

  InvokeRemoveNode("node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriver},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriverComponent},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  // Close node_a_a's driver component. The node completes shutdown and should be removed.
  InvokeDestroyChildResponse("node_a_a");
  VerifyNodeRemovedFromParent("node_a_a", "node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriverComponent}});

  // Close node_a_b_a's driver component. The node should complete shutdown and be removed.
  InvokeDestroyChildResponse("node_a_b_a");
  VerifyNodeRemovedFromParent("node_a_b_a", "node_a_b");
  VerifyStates(
      {{"node_a", NodeState::kWaitingOnChildren}, {"node_a_b", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnDriverComponent}});

  InvokeDestroyChildResponse("node_a_b");
  VerifyNodeRemovedFromParent("node_a_b", "node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriver);

  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriverComponent);
  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerAllCallbackInvoked();
  VerifyRemovalTrackerPkgCallbackInvoked();
}

TEST_F(NodeShutdownTest, RemoveCompositeNode) {
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");
  AddChildNodeAndStartDriver("node_a", "node_a_b");
  AddChildNodeAndStartDriver("node_a", "node_a_c");

  AddCompositeNode("composite_abc", {"node_a_a", "node_a_b", "node_a_c"});
  StartDriver("composite_abc");

  InvokeRemoveNode("node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_c", NodeState::kWaitingOnChildren},
                {"composite_abc", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("composite_abc");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_c", NodeState::kWaitingOnChildren},
                {"composite_abc", NodeState::kWaitingOnDriverComponent}});

  InvokeDestroyChildResponse("composite_abc");
  VerifyNodeRemovedFromParent("composite_abc", "node_a_a");
  VerifyNodeRemovedFromParent("composite_abc", "node_a_b");
  VerifyNodeRemovedFromParent("composite_abc", "node_a_c");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriver},
                {"node_a_b", NodeState::kWaitingOnDriver},
                {"node_a_c", NodeState::kWaitingOnDriver}});

  auto remove_nodes = {"node_a_a", "node_a_b", "node_a_c"};
  for (auto node : remove_nodes) {
    CloseDriverForNode(node);
    InvokeDestroyChildResponse(node);
    VerifyNodeRemovedFromParent(node, "node_a");
  }

  VerifyState("node_a", NodeState::kWaitingOnDriver);
  CloseDriverForNode("node_a");
  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");

  VerifyRemovalTrackerAllCallbackInvoked();
}

TEST_F(NodeShutdownTest, RemoveLeafNode) {
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");

  InvokeRemoveNode("node_a_a");

  VerifyState("node_a", NodeState::kRunning);
  VerifyState("node_a_a", NodeState::kWaitingOnDriver);

  CloseDriverForNode("node_a_a");
  VerifyState("node_a_a", NodeState::kWaitingOnDriverComponent);

  InvokeDestroyChildResponse("node_a_a");
  VerifyNodeRemovedFromParent("node_a_a", "node_a");
  VerifyRemovalTrackerAllCallbackInvoked();
}

TEST_F(NodeShutdownTest, RemoveNodeWithNoChildren) {
  AddNodeAndStartDriver("node_a");
  InvokeRemoveNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriver);

  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriverComponent);

  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerAllCallbackInvoked();
}

TEST_F(NodeShutdownTest, RemoveNodeWithNoDriverOrChildren) {
  AddNode("node_a");  // No driver.
  InvokeRemoveNode("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
}

TEST_F(NodeShutdownTest, DriverShutdownWhileWaitingOnChildren) {
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");

  InvokeRemoveNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnChildren);
  VerifyState("node_a_a", NodeState::kWaitingOnDriver);

  // Close node_a's while it's still waiting for node_a_a. Node_a should
  // still wait for its children.
  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnChildren);
  VerifyState("node_a_a", NodeState::kWaitingOnDriver);

  // Close node_a_a's driver.
  CloseDriverForNode("node_a_a");
  VerifyState("node_a", NodeState::kWaitingOnChildren);
  VerifyState("node_a_a", NodeState::kWaitingOnDriverComponent);

  // Destroy node_a_a's driver component. Since node_a's driver was already
  // closed, it should go straight to destroying the driver component.
  InvokeDestroyChildResponse("node_a_a");
  VerifyNodeRemovedFromParent("node_a_a", "node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriverComponent);

  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerAllCallbackInvoked();
}

TEST_F(NodeShutdownTest, RemoveAfterBindFailure) {
  AddNodeAndStartDriver("node_a");
  GetNode("node_a")->CompleteBind(zx::error(ZX_ERR_NOT_FOUND));
  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kRunning);
  InvokeRemoveNode("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
}

TEST_F(NodeShutdownTest, WaitBindBeforeShutdown) {
  node_manager->driver_host().set_should_queue_start_callback(true);
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_b");
  AddChildNodeAndStartDriver("node_a_b", "node_a_b_a");

  // Complete bind successfully for node_a.
  node_manager->driver_host().InvokeStartCallback("node_a", zx::ok());

  InvokeRemoveNode("node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnDriverBind},
                {"node_a_b_a", NodeState::kWaitingOnDriverBind}});

  node_manager->driver_host().InvokeStartCallback("node_a_b", zx::ok());
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriverBind}});

  node_manager->driver_host().InvokeStartCallback("node_a_b_a", zx::ok());
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b_a");
  InvokeDestroyChildResponse("node_a_b_a");
  VerifyNodeRemovedFromParent("node_a_b_a", "node_a_b");

  CloseDriverForNode("node_a_b");
  InvokeDestroyChildResponse("node_a_b");
  VerifyNodeRemovedFromParent("node_a_b", "node_a");

  CloseDriverForNode("node_a");
  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");

  VerifyRemovalTrackerAllCallbackInvoked();
}

TEST_F(NodeShutdownTest, WaitBindBeforeShutdownForPkgNode) {
  const char* node_pkg1 = "node_package1";
  const char* node_pkg2 = "node_package2";
  const char* node_boot = "node_boot";

  node_manager->driver_host().set_should_queue_start_callback(true);
  AddNodeAndStartDriver(node_boot, Collection::kBoot);
  AddChildNodeAndStartDriver(node_boot, node_pkg1, Collection::kPackage);
  AddChildNodeAndStartDriver(node_boot, node_pkg2, Collection::kPackage);

  // Complete bind successfully for boot node.
  node_manager->driver_host().InvokeStartCallback(node_boot, zx::ok());
  node_manager->driver_host().InvokeStartCallback(node_pkg1, zx::ok());

  InvokeRemoveNode(node_boot, RemovalSet::kPackage);
  VerifyStates({{node_boot, NodeState::kPrestop},
                {node_pkg1, NodeState::kWaitingOnDriver},
                {node_pkg2, NodeState::kWaitingOnDriverBind}});

  CloseDriverForNode(node_pkg1);
  InvokeDestroyChildResponse(node_pkg1);
  VerifyNodeRemovedFromParent(node_pkg1, node_boot);

  node_manager->driver_host().InvokeStartCallback(node_pkg2, zx::ok());
  VerifyStates({{node_boot, NodeState::kPrestop}, {node_pkg2, NodeState::kWaitingOnDriver}});

  CloseDriverForNode(node_pkg2);
  InvokeDestroyChildResponse(node_pkg2);
  VerifyNodeRemovedFromParent(node_pkg2, node_boot);

  VerifyState(node_boot, NodeState::kPrestop);

  VerifyRemovalTrackerPkgCallbackInvoked();
}

TEST_F(NodeShutdownTest, BindFailureDuringRemove) {
  AddNodeAndStartDriver("node_a");

  InvokeRemoveNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriver);

  GetNode("node_a")->CompleteBind(zx::error(ZX_ERR_NOT_FOUND));
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerAllCallbackInvoked();
}

TEST_F(NodeShutdownTest, DriverHostFailure) {
  node_manager->driver_host().set_should_queue_start_callback(true);
  AddNodeAndStartDriver("node_a");
  node_manager->driver_host().InvokeStartCallback("node_a", zx::error(ZX_ERR_INTERNAL));
  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kRunning);
  InvokeRemoveNode("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
}

TEST_F(NodeShutdownTest, RemoveDuringDriverHostStartWithFailure) {
  node_manager->driver_host().set_should_queue_start_callback(true);
  AddNodeAndStartDriver("node_a");

  InvokeRemoveNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriverBind);
  node_manager->driver_host().InvokeStartCallback("node_a", zx::error(ZX_ERR_INTERNAL));

  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerAllCallbackInvoked();
}

TEST_F(NodeShutdownTest, OverlappingRemoveCalls) {
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");
  AddChildNodeAndStartDriver("node_a", "node_a_b");
  AddChildNodeAndStartDriver("node_a_b", "node_a_b_a");

  InvokeRemoveNode("node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriver},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriverComponent},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  InvokeRemoveNode("node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriverComponent},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  // Close node_a_a's driver component. The node completes shutdown and should be removed.
  InvokeDestroyChildResponse("node_a_a");
  VerifyNodeRemovedFromParent("node_a_a", "node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriverComponent}});

  // Close node_a_b_a's driver component. The node should complete shutdown and be removed.
  InvokeDestroyChildResponse("node_a_b_a");
  VerifyNodeRemovedFromParent("node_a_b_a", "node_a_b");
  VerifyStates(
      {{"node_a", NodeState::kWaitingOnChildren}, {"node_a_b", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnDriverComponent}});

  InvokeDestroyChildResponse("node_a_b");
  VerifyNodeRemovedFromParent("node_a_b", "node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriver);

  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriverComponent);
  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerAllCallbackInvoked();
}

TEST_F(NodeShutdownTest, OverlappingRemoveCalls_DifferentNodes) {
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");
  AddChildNodeAndStartDriver("node_a", "node_a_b");
  AddChildNodeAndStartDriver("node_a_b", "node_a_b_a");

  InvokeRemoveNode("node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriver},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriverComponent},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  // Close node_a_a's driver component. The node completes shutdown and should be removed.
  InvokeDestroyChildResponse("node_a_a");
  VerifyNodeRemovedFromParent("node_a_a", "node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  InvokeRemoveNode("node_a_b");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriverComponent}});

  // Close node_a_b_a's driver component. The node should complete shutdown and be removed.
  InvokeDestroyChildResponse("node_a_b_a");
  VerifyNodeRemovedFromParent("node_a_b_a", "node_a_b");
  VerifyStates(
      {{"node_a", NodeState::kWaitingOnChildren}, {"node_a_b", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnDriverComponent}});

  InvokeDestroyChildResponse("node_a_b");
  VerifyNodeRemovedFromParent("node_a_b", "node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriver);

  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriverComponent);
  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerAllCallbackInvoked();
}

// Test nodes spread across boot and package collections when we first just
// stop the package drivers and then ask to stop the boot drivers.
TEST_F(NodeShutdownTest, NodesInDifferentCollections) {
  // TEST OUTLINE
  // Create a root and attach to it three children. The first and third are
  // package drivers, the second is a boot driver. This arrangement is
  // intentional because the bug this test was written in response to only
  // happened when a boot driver child was first and a package driver child
  // after it as children were process LIFO. We put a boot driver in the middle
  // in case we reintroduce the bug *and* switch to FIFO processing of nodes.
  const char* root_name = "node_root";
  const char* node_pkg1 = "node_package1";
  const char* node_pkg2 = "node_package2";
  const char* node_boot = "node_boot";

  // Make the root and put it in the boot collection. We must have the root
  // in boot because if it were in package it would not matter if one of its
  // children were a boot driver.
  AddNodeAndStartDriver(root_name, Collection::kBoot);

  AddChildNodeAndStartDriver(root_name, node_pkg1, Collection::kPackage);

  AddChildNodeAndStartDriver(root_name, node_boot, Collection::kBoot);

  AddChildNodeAndStartDriver(root_name, node_pkg2, Collection::kPackage);

  // Make the call to remove package-based drivers. This should *NOT* stopt the
  // boot drivers, but instead put them in a pre-stop state.
  InvokeRemoveNode(root_name, RemovalSet::kPackage);
  VerifyStates({{root_name, NodeState::kPrestop},
                {node_boot, NodeState::kPrestop},
                {node_pkg1, NodeState::kWaitingOnDriver},
                {node_pkg2, NodeState::kWaitingOnDriver}});

  // Stop the drivers and components backing the package driver nodes.
  CloseDriverForNode(node_pkg1);
  InvokeDestroyChildResponse(node_pkg1);
  CloseDriverForNode(node_pkg2);
  InvokeDestroyChildResponse(node_pkg2);

  // Check these children are gone
  VerifyNodeRemovedFromParent(node_pkg1, root_name);
  VerifyNodeRemovedFromParent(node_pkg2, root_name);

  // Check remaining nodes are in expected state.
  VerifyStates({
      {root_name, NodeState::kPrestop},
      {node_boot, NodeState::kPrestop},
  });
  VerifyRemovalTrackerPkgCallbackInvoked();
  VerifyRemovalTrackerAllCallbackNotInvoked();

  // Now remove all drivers.
  InvokeRemoveNode(root_name, RemovalSet::kAll);
  VerifyStates({
      {root_name, NodeState::kWaitingOnChildren},
      {node_boot, NodeState::kWaitingOnDriver},
  });

  // Take the child of the test root through its stages.
  CloseDriverForNode(node_boot);
  VerifyState(node_boot, NodeState::kWaitingOnDriverComponent);
  InvokeDestroyChildResponse(node_boot);

  // Check the child was removed from the parent.
  VerifyNodeRemovedFromParent(node_boot, root_name);

  // Now test root just should be waiting on its driver, take it down the rest
  // of the way.
  VerifyState(root_name, NodeState::kWaitingOnDriver);
  CloseDriverForNode(root_name);
  VerifyState(root_name, NodeState::kWaitingOnDriverComponent);
  InvokeDestroyChildResponse(root_name);

  // Check the test root was removed from the realm root.
  VerifyNodeRemovedFromParent(root_name, "root");
  VerifyRemovalTrackerAllCallbackInvoked();
}

// Test behavior with nodes in boot and package sets when we shut down all
// drivers.
TEST_F(NodeShutdownTest, RemoveAllRemovesEverything) {
  // TEST OUTLINE
  // Create a test root node, then create three children. Two of those children
  // will be package drivers and one will be a boot driver. The root node is a
  // boot driver as well. We expect that when we issue a shutdown for
  // RemovalSet::kAll that they all get torn down.
  const char* root_name = "node_root";
  const char* node_pkg1 = "node_package1";
  const char* node_pkg2 = "node_package2";
  const char* node_boot = "node_boot";

  // Make the root and put it in the boot collection. We must have the root
  // in boot because if it were in package it would not matter if one of its
  // children were a boot driver.
  AddNodeAndStartDriver(root_name, Collection::kBoot);

  AddChildNodeAndStartDriver(root_name, node_pkg1, Collection::kPackage);

  AddChildNodeAndStartDriver(root_name, node_boot, Collection::kBoot);

  AddChildNodeAndStartDriver(root_name, node_pkg2, Collection::kPackage);

  // Make the call to remove package-based drivers. This should *NOT* stop the
  // boot drivers, but instead put them in a pre-stop state.
  InvokeRemoveNode(root_name, RemovalSet::kAll);
  VerifyStates({{root_name, NodeState::kWaitingOnChildren},
                {node_boot, NodeState::kWaitingOnDriver},
                {node_pkg1, NodeState::kWaitingOnDriver},
                {node_pkg2, NodeState::kWaitingOnDriver}});

  // Stop the drivers and components backing the package driver nodes.
  CloseDriverForNode(node_pkg1);
  InvokeDestroyChildResponse(node_pkg1);
  CloseDriverForNode(node_pkg2);
  InvokeDestroyChildResponse(node_pkg2);

  // Check these children are gone.
  VerifyNodeRemovedFromParent(node_pkg1, root_name);
  VerifyNodeRemovedFromParent(node_pkg2, root_name);
  VerifyRemovalTrackerPkgCallbackInvoked();
  VerifyRemovalTrackerAllCallbackNotInvoked();

  // Take the child of the test root through its stages.
  CloseDriverForNode(node_boot);
  VerifyState(node_boot, NodeState::kWaitingOnDriverComponent);
  InvokeDestroyChildResponse(node_boot);

  // Check the child was removed from the parent.
  VerifyNodeRemovedFromParent(node_boot, root_name);

  // Now test root just should be waiting on its driver, take it down the rest
  // of the way.
  VerifyState(root_name, NodeState::kWaitingOnDriver);
  CloseDriverForNode(root_name);
  VerifyState(root_name, NodeState::kWaitingOnDriverComponent);
  InvokeDestroyChildResponse(root_name);

  // Check the test root was removed from the realm root.
  VerifyNodeRemovedFromParent(root_name, "root");
  VerifyRemovalTrackerAllCallbackInvoked();
}
