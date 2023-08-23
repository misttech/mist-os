// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/node.h"

#include <fuchsia/component/cpp/fidl.h>

#include "src/devices/bin/driver_manager/v2/composite_node_spec_v2.h"
#include "src/devices/bin/driver_manager/v2/driver_host.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

// TODO(fxb/132293): Move FakeNodeManager and common node construction code into a separate class.

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
    cb(zx::ok());
  }

  zx::result<uint64_t> GetProcessKoid() const override { return zx::error(ZX_ERR_NOT_SUPPORTED); }

  void CloseDriver(std::string node_name) { drivers_[node_name].Close(ZX_OK); }

 private:
  std::unordered_map<std::string, fidl::ServerEnd<fuchsia_driver_host::Driver>> drivers_;
};

class FakeNodeManager : public dfv2::NodeManager {
 public:
  FakeNodeManager(fidl::WireClient<fuchsia_component::Realm> realm) : realm_(std::move(realm)) {}

  void Bind(dfv2::Node& node, std::shared_ptr<dfv2::BindResultTracker> result_tracker) override {}

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
  }

  void CloseDriverForNode(std::string node_name) { driver_host_.CloseDriver(node_name); }

 private:
  fidl::WireClient<fuchsia_component::Realm> realm_;

  FakeDriverHost driver_host_;
};

class Dfv2NodeTest : public gtest::TestLoopFixture {
 public:
  void SetUp() override {
    TestLoopFixture::SetUp();
    realm_ = std::make_unique<TestRealm>(dispatcher());

    auto client = realm_->Connect();
    ASSERT_TRUE(client.is_ok());
    node_manager = std::make_unique<FakeNodeManager>(
        fidl::WireClient<fuchsia_component::Realm>(std::move(client.value()), dispatcher()));

    devfs_.emplace(root_devnode_);
    root_ = CreateNode("root");
    root_->AddToDevfsForTesting(root_devnode_.value());
  }

  std::shared_ptr<dfv2::Node> CreateNode(const char* name) {
    auto node = std::make_shared<dfv2::Node>(name, std::vector<std::weak_ptr<dfv2::Node>>(),
                                             node_manager.get(), dispatcher(),
                                             inspect_.CreateDevice(name, zx::vmo(), 0));
    node->AddToDevfsForTesting(root_devnode_.value());
    return node;
  }

  std::shared_ptr<dfv2::Node> CreateCompositeNode(std::string_view name,
                                                  std::vector<std::weak_ptr<dfv2::Node>> parents,
                                                  bool is_legacy, uint32_t primary_index = 0) {
    std::vector<std::string> parent_names;
    parent_names.reserve(parents.size());
    for (auto& parent : parents) {
      parent_names.push_back(parent.lock()->name());
    }

    return dfv2::Node::CreateCompositeNode(name, parents, std::move(parent_names), {},
                                           node_manager.get(), dispatcher(), is_legacy,
                                           primary_index)
        .value();
  }

  void StartTestDriver(std::shared_ptr<dfv2::Node> node) {
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

    auto outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    EXPECT_EQ(ZX_OK, outgoing_endpoints.status_value());

    auto start_info = fuchsia_component_runner::ComponentStartInfo{{
        .resolved_url = "fuchsia-boot:///#meta/test-driver.cm",
        .program = fuchsia_data::Dictionary{{.entries = std::move(program_entries)}},
        .outgoing_dir = std::move(outgoing_endpoints->server),
    }};

    auto controller_endpoints =
        fidl::CreateEndpoints<fuchsia_component_runner::ComponentController>();

    fidl::Arena arena;
    node->StartDriver(fidl::ToWire(arena, std::move(start_info)),
                      std::move(controller_endpoints->server), [](zx::result<> result) {});
  }

 protected:
  std::unique_ptr<FakeNodeManager> node_manager;

 private:
  std::unique_ptr<TestRealm> realm_;

  InspectManager inspect_{dispatcher()};

  std::shared_ptr<dfv2::Node> root_;
  std::optional<Devnode> root_devnode_;
  std::optional<Devfs> devfs_;
};

TEST_F(Dfv2NodeTest, RemoveDuringFailedBind) {
  auto node = CreateNode("test");
  StartTestDriver(node);
  ASSERT_TRUE(node->has_driver_component());
  ASSERT_EQ(dfv2::NodeState::kRunning, node->node_state());

  node->Remove(dfv2::RemovalSet::kAll, nullptr);
  ASSERT_EQ(dfv2::NodeState::kWaitingOnDriver, node->node_state());

  node->CompleteBind(zx::error(ZX_ERR_NOT_FOUND));
  ASSERT_FALSE(node->has_driver_component());
  ASSERT_EQ(dfv2::NodeState::kStopping, node->node_state());
}

TEST_F(Dfv2NodeTest, TestEvaluateRematchFlags) {
  auto node = CreateNode("plain");
  ASSERT_FALSE(
      node->EvaluateRematchFlags(fuchsia_driver_development::RematchFlags::kRequested, "some-url"));
  ASSERT_TRUE(
      node->EvaluateRematchFlags(fuchsia_driver_development::RematchFlags::kRequested |
                                     fuchsia_driver_development::RematchFlags::kNonRequested,
                                 "some-url"));

  auto parent_1 = CreateNode("p1");
  auto parent_2 = CreateNode("p2");

  auto legacy_composite = CreateCompositeNode("legacy-composite", {parent_1, parent_2},
                                              /* is_legacy*/ true, /* primary_index */ 0);

  ASSERT_FALSE(legacy_composite->EvaluateRematchFlags(
      fuchsia_driver_development::RematchFlags::kRequested |
          fuchsia_driver_development::RematchFlags::kNonRequested,
      "some-url"));
  ASSERT_TRUE(legacy_composite->EvaluateRematchFlags(
      fuchsia_driver_development::RematchFlags::kRequested |
          fuchsia_driver_development::RematchFlags::kNonRequested |
          fuchsia_driver_development::RematchFlags::kLegacyComposite,
      "some-url"));

  auto composite = CreateCompositeNode("composite", {parent_1, parent_2},
                                       /* is_legacy*/ false, /* primary_index */ 0);

  ASSERT_FALSE(
      composite->EvaluateRematchFlags(fuchsia_driver_development::RematchFlags::kRequested |
                                          fuchsia_driver_development::RematchFlags::kNonRequested,
                                      "some-url"));
  ASSERT_FALSE(composite->EvaluateRematchFlags(
      fuchsia_driver_development::RematchFlags::kRequested |
          fuchsia_driver_development::RematchFlags::kNonRequested |
          fuchsia_driver_development::RematchFlags::kLegacyComposite,
      "some-url"));

  ASSERT_TRUE(legacy_composite->EvaluateRematchFlags(
      fuchsia_driver_development::RematchFlags::kRequested |
          fuchsia_driver_development::RematchFlags::kNonRequested |
          fuchsia_driver_development::RematchFlags::kLegacyComposite |
          fuchsia_driver_development::RematchFlags::kCompositeSpec,
      "some-url"));
}

TEST_F(Dfv2NodeTest, RemoveCompositeNodeForRebind) {
  auto parent_node_1 = CreateNode("parent_1");
  StartTestDriver(parent_node_1);
  ASSERT_TRUE(parent_node_1->has_driver_component());
  ASSERT_EQ(dfv2::NodeState::kRunning, parent_node_1->node_state());

  auto parent_node_2 = CreateNode("parent_2");
  StartTestDriver(parent_node_2);
  ASSERT_TRUE(parent_node_2->has_driver_component());
  ASSERT_EQ(dfv2::NodeState::kRunning, parent_node_2->node_state());

  auto composite =
      CreateCompositeNode("composite", {parent_node_1, parent_node_2}, /* is_legacy*/ false);
  StartTestDriver(composite);
  ASSERT_TRUE(composite->has_driver_component());
  ASSERT_EQ(dfv2::NodeState::kRunning, composite->node_state());

  ASSERT_EQ(1u, parent_node_1->children().size());
  ASSERT_EQ(1u, parent_node_2->children().size());

  auto remove_callback_succeeded = false;
  composite->RemoveCompositeNodeForRebind([&remove_callback_succeeded](zx::result<> result) {
    if (result.is_ok()) {
      remove_callback_succeeded = true;
    }
  });
  ASSERT_EQ(dfv2::NodeState::kWaitingOnDriver, composite->node_state());
  ASSERT_EQ(dfv2::ShutdownIntent::kRebindComposite, composite->shutdown_intent());

  node_manager->CloseDriverForNode("composite");
  RunLoopUntilIdle();
  ASSERT_TRUE(remove_callback_succeeded);

  ASSERT_EQ(dfv2::NodeState::kStopping, composite->node_state());
}

// Verify that we receives a callback for composite rebind if the node is deallocated
// before shutdown is complete.
TEST_F(Dfv2NodeTest, RemoveCompositeNodeForRebind_Dealloc) {
  auto parent_node_1 = CreateNode("parent_1");
  StartTestDriver(parent_node_1);
  ASSERT_TRUE(parent_node_1->has_driver_component());
  ASSERT_EQ(dfv2::NodeState::kRunning, parent_node_1->node_state());

  auto parent_node_2 = CreateNode("parent_2");
  StartTestDriver(parent_node_2);
  ASSERT_TRUE(parent_node_2->has_driver_component());
  ASSERT_EQ(dfv2::NodeState::kRunning, parent_node_2->node_state());

  auto composite =
      CreateCompositeNode("composite", {parent_node_1, parent_node_2}, /* is_legacy*/ false);
  StartTestDriver(composite);
  ASSERT_TRUE(composite->has_driver_component());
  ASSERT_EQ(dfv2::NodeState::kRunning, composite->node_state());

  ASSERT_EQ(1u, parent_node_1->children().size());
  ASSERT_EQ(1u, parent_node_2->children().size());

  auto remove_callback_succeeded = false;
  composite->RemoveCompositeNodeForRebind([&remove_callback_succeeded](zx::result<> result) {
    if (result.is_ok()) {
      remove_callback_succeeded = true;
    }
  });
  ASSERT_EQ(dfv2::NodeState::kWaitingOnDriver, composite->node_state());
  ASSERT_EQ(dfv2::ShutdownIntent::kRebindComposite, composite->shutdown_intent());

  parent_node_1.reset();
  parent_node_2.reset();
  composite.reset();
  RunLoopUntilIdle();
  ASSERT_TRUE(remove_callback_succeeded);
}
