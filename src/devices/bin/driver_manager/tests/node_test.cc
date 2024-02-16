// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/node.h"

#include <fuchsia/component/cpp/fidl.h>

#include "src/devices/bin/driver_manager/composite_node_spec_v2.h"
#include "src/devices/bin/driver_manager/driver_host.h"
#include "src/devices/bin/driver_manager/tests/driver_manager_test_base.h"

class TestRealm final : public fidl::WireServer<fuchsia_component::Realm> {
 public:
  TestRealm(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  zx::result<fidl::ClientEnd<fuchsia_component::Realm>> Connect() {
    auto endpoints = fidl::CreateEndpoints<fuchsia_component::Realm>();
    if (endpoints.is_error()) {
      return zx::error(endpoints.status_value());
    }
    fidl::BindServer(dispatcher_, std::move(endpoints->server), this);
    return zx::ok(std::move(endpoints->client));
  }

  void OpenExposedDir(OpenExposedDirRequestView request,
                      OpenExposedDirCompleter::Sync& completer) override {}

  void CreateChild(CreateChildRequestView request, CreateChildCompleter::Sync& completer) override {
  }

  void DestroyChild(DestroyChildRequestView request,
                    DestroyChildCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }

  void ListChildren(ListChildrenRequestView request,
                    ListChildrenCompleter::Sync& completer) override {}

 private:
  async_dispatcher_t* dispatcher_;
};

class FakeDriverHost : public dfv2::DriverHost {
 public:
  using StartCallback = fit::callback<void(zx::result<>)>;
  void Start(fidl::ClientEnd<fuchsia_driver_framework::Node> client_end, std::string node_name,
             fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol> symbols,
             fuchsia_component_runner::wire::ComponentStartInfo start_info,
             fidl::ServerEnd<fuchsia_driver_host::Driver> driver, StartCallback cb) override {
    drivers_[node_name] = std::move(driver);
    clients_[node_name] = std::move(client_end);
    cb(zx::ok());
  }

  zx::result<uint64_t> GetProcessKoid() const override { return zx::error(ZX_ERR_NOT_SUPPORTED); }

  void CloseDriver(std::string node_name) {
    drivers_[node_name].Close(ZX_OK);
    clients_[node_name].reset();
  }

 private:
  std::unordered_map<std::string, fidl::ServerEnd<fuchsia_driver_host::Driver>> drivers_;
  std::unordered_map<std::string, fidl::ClientEnd<fuchsia_driver_framework::Node>> clients_;
};

class FakeNodeManager : public TestNodeManagerBase {
 public:
  FakeNodeManager(fidl::WireClient<fuchsia_component::Realm> realm) : realm_(std::move(realm)) {}

  zx::result<dfv2::DriverHost*> CreateDriverHost() override { return zx::ok(&driver_host_); }

  void DestroyDriverComponent(
      dfv2::Node& node,
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

 private:
  fidl::WireClient<fuchsia_component::Realm> realm_;
  std::unordered_map<std::string, fidl::ClientEnd<fuchsia_component_runner::ComponentController>>
      clients_;
  FakeDriverHost driver_host_;
};

class Dfv2NodeTest : public DriverManagerTestBase {
 public:
  struct StartDriverOptions {
    bool host_restart_on_crash;
  };

  void SetUp() override {
    DriverManagerTestBase::SetUp();
    realm_ = std::make_unique<TestRealm>(dispatcher());

    auto client = realm_->Connect();
    ASSERT_TRUE(client.is_ok());
    node_manager = std::make_unique<FakeNodeManager>(
        fidl::WireClient<fuchsia_component::Realm>(std::move(client.value()), dispatcher()));
  }

  void StartTestDriver(std::shared_ptr<dfv2::Node> node,
                       StartDriverOptions options = {.host_restart_on_crash = false}) {
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

    if (options.host_restart_on_crash) {
      program_entries.emplace_back(fuchsia_data::DictionaryEntry({
          .key = "host_restart_on_crash",
          .value = std::make_unique<fuchsia_data::DictionaryValue>(
              fuchsia_data::DictionaryValue::WithStr("true")),
      }));
    }

    auto outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    EXPECT_EQ(ZX_OK, outgoing_endpoints.status_value());

    auto start_info = fuchsia_component_runner::ComponentStartInfo{{
        .resolved_url = "fuchsia-boot:///#meta/test-driver.cm",
        .program = fuchsia_data::Dictionary{{.entries = std::move(program_entries)}},
        .outgoing_dir = std::move(outgoing_endpoints->server),
    }};

    auto controller_endpoints =
        fidl::CreateEndpoints<fuchsia_component_runner::ComponentController>();

    node_manager->AddClient(node->name(), std::move(controller_endpoints->client));

    fidl::Arena arena;
    node->StartDriver(fidl::ToWire(arena, std::move(start_info)),
                      std::move(controller_endpoints->server),
                      [node](zx::result<> result) { node->CompleteBind(result); });
  }

 protected:
  dfv2::NodeManager* GetNodeManager() override { return node_manager.get(); }

  std::unique_ptr<FakeNodeManager> node_manager;

 private:
  std::unique_ptr<TestRealm> realm_;
};

TEST_F(Dfv2NodeTest, RemoveDuringFailedBind) {
  auto node = CreateNode("test");
  StartTestDriver(node);
  ASSERT_TRUE(node->HasDriverComponent());
  ASSERT_EQ(dfv2::NodeState::kRunning, node->GetNodeState());

  node->Remove(dfv2::RemovalSet::kAll, nullptr);
  RunLoopUntilIdle();

  ASSERT_EQ(dfv2::NodeState::kWaitingOnDriver, node->GetNodeState());

  node->CompleteBind(zx::error(ZX_ERR_NOT_FOUND));
  RunLoopUntilIdle();
  ASSERT_FALSE(node->HasDriverComponent());
  ASSERT_EQ(dfv2::NodeState::kStopped, node->GetNodeState());
}

TEST_F(Dfv2NodeTest, TestEvaluateRematchFlags) {
  auto node = CreateNode("plain");
  ASSERT_FALSE(node->EvaluateRematchFlags(
      fuchsia_driver_development::RestartRematchFlags::kRequested, "some-url"));
  ASSERT_TRUE(
      node->EvaluateRematchFlags(fuchsia_driver_development::RestartRematchFlags::kRequested |
                                     fuchsia_driver_development::RestartRematchFlags::kNonRequested,
                                 "some-url"));

  auto parent_1 = CreateNode("p1");
  auto parent_2 = CreateNode("p2");

  auto legacy_composite = CreateCompositeNode("legacy-composite", {parent_1, parent_2},
                                              /* is_legacy*/ true, /* primary_index */ 0);

  ASSERT_FALSE(legacy_composite->EvaluateRematchFlags(
      fuchsia_driver_development::RestartRematchFlags::kRequested |
          fuchsia_driver_development::RestartRematchFlags::kNonRequested,
      "some-url"));
  ASSERT_TRUE(legacy_composite->EvaluateRematchFlags(
      fuchsia_driver_development::RestartRematchFlags::kRequested |
          fuchsia_driver_development::RestartRematchFlags::kNonRequested |
          fuchsia_driver_development::RestartRematchFlags::kLegacyComposite,
      "some-url"));

  auto composite = CreateCompositeNode("composite", {parent_1, parent_2},
                                       /* is_legacy*/ false, /* primary_index */ 0);

  ASSERT_FALSE(composite->EvaluateRematchFlags(
      fuchsia_driver_development::RestartRematchFlags::kRequested |
          fuchsia_driver_development::RestartRematchFlags::kNonRequested,
      "some-url"));
  ASSERT_FALSE(composite->EvaluateRematchFlags(
      fuchsia_driver_development::RestartRematchFlags::kRequested |
          fuchsia_driver_development::RestartRematchFlags::kNonRequested |
          fuchsia_driver_development::RestartRematchFlags::kLegacyComposite,
      "some-url"));

  ASSERT_TRUE(legacy_composite->EvaluateRematchFlags(
      fuchsia_driver_development::RestartRematchFlags::kRequested |
          fuchsia_driver_development::RestartRematchFlags::kNonRequested |
          fuchsia_driver_development::RestartRematchFlags::kLegacyComposite |
          fuchsia_driver_development::RestartRematchFlags::kCompositeSpec,
      "some-url"));
}

TEST_F(Dfv2NodeTest, RemoveCompositeNodeForRebind) {
  auto parent_node_1 = CreateNode("parent_1");
  StartTestDriver(parent_node_1);
  ASSERT_TRUE(parent_node_1->HasDriverComponent());
  ASSERT_EQ(dfv2::NodeState::kRunning, parent_node_1->GetNodeState());

  auto parent_node_2 = CreateNode("parent_2");
  StartTestDriver(parent_node_2);
  ASSERT_TRUE(parent_node_2->HasDriverComponent());
  ASSERT_EQ(dfv2::NodeState::kRunning, parent_node_2->GetNodeState());

  auto composite =
      CreateCompositeNode("composite", {parent_node_1, parent_node_2}, /* is_legacy*/ false);
  StartTestDriver(composite);
  ASSERT_TRUE(composite->HasDriverComponent());
  ASSERT_EQ(dfv2::NodeState::kRunning, composite->GetNodeState());

  ASSERT_EQ(1u, parent_node_1->children().size());
  ASSERT_EQ(1u, parent_node_2->children().size());

  auto remove_callback_succeeded = false;
  composite->RemoveCompositeNodeForRebind([&remove_callback_succeeded](zx::result<> result) {
    if (result.is_ok()) {
      remove_callback_succeeded = true;
    }
  });
  RunLoopUntilIdle();
  ASSERT_EQ(dfv2::NodeState::kWaitingOnDriver, composite->GetNodeState());
  ASSERT_EQ(dfv2::ShutdownIntent::kRebindComposite, composite->shutdown_intent());

  node_manager->CloseDriverForNode("composite");
  RunLoopUntilIdle();
  ASSERT_TRUE(remove_callback_succeeded);

  ASSERT_EQ(dfv2::NodeState::kStopped, composite->GetNodeState());
}

// Verify that we receives a callback for composite rebind if the node is deallocated
// before shutdown is complete.
TEST_F(Dfv2NodeTest, RemoveCompositeNodeForRebind_Dealloc) {
  auto parent_node_1 = CreateNode("parent_1");
  StartTestDriver(parent_node_1);
  ASSERT_TRUE(parent_node_1->HasDriverComponent());
  ASSERT_EQ(dfv2::NodeState::kRunning, parent_node_1->GetNodeState());

  auto parent_node_2 = CreateNode("parent_2");
  StartTestDriver(parent_node_2);
  ASSERT_TRUE(parent_node_2->HasDriverComponent());
  ASSERT_EQ(dfv2::NodeState::kRunning, parent_node_2->GetNodeState());

  auto composite =
      CreateCompositeNode("composite", {parent_node_1, parent_node_2}, /* is_legacy*/ false);
  StartTestDriver(composite);
  ASSERT_TRUE(composite->HasDriverComponent());
  ASSERT_EQ(dfv2::NodeState::kRunning, composite->GetNodeState());

  ASSERT_EQ(1u, parent_node_1->children().size());
  ASSERT_EQ(1u, parent_node_2->children().size());

  auto remove_callback_succeeded = false;
  composite->RemoveCompositeNodeForRebind([&remove_callback_succeeded](zx::result<> result) {
    if (result.is_ok()) {
      remove_callback_succeeded = true;
    }
  });
  RunLoopUntilIdle();
  ASSERT_EQ(dfv2::NodeState::kWaitingOnDriver, composite->GetNodeState());
  ASSERT_EQ(dfv2::ShutdownIntent::kRebindComposite, composite->shutdown_intent());

  parent_node_1.reset();
  parent_node_2.reset();
  composite.reset();
  RunLoopUntilIdle();
  ASSERT_TRUE(remove_callback_succeeded);
}

TEST_F(Dfv2NodeTest, RestartOnCrashComposite) {
  auto parent_node_1 = CreateNode("parent_1");
  StartTestDriver(parent_node_1);
  ASSERT_TRUE(parent_node_1->HasDriverComponent());
  ASSERT_EQ(dfv2::NodeState::kRunning, parent_node_1->GetNodeState());

  auto parent_node_2 = CreateNode("parent_2");
  StartTestDriver(parent_node_2);
  ASSERT_TRUE(parent_node_2->HasDriverComponent());
  ASSERT_EQ(dfv2::NodeState::kRunning, parent_node_2->GetNodeState());

  auto composite =
      CreateCompositeNode("composite", {parent_node_1, parent_node_2}, /* is_legacy*/ false);
  StartTestDriver(composite, {.host_restart_on_crash = true});

  ASSERT_TRUE(composite->HasDriverComponent());
  ASSERT_EQ(dfv2::NodeState::kRunning, composite->GetNodeState());

  ASSERT_EQ(1u, parent_node_1->children().size());
  ASSERT_EQ(1u, parent_node_2->children().size());

  // Simulate a crash by closing the driver side of channels.
  node_manager->CloseDriverForNode("composite");
  RunLoopUntilIdle();

  // The node should come back to running state.
  ASSERT_EQ(dfv2::NodeState::kRunning, composite->GetNodeState());
}
