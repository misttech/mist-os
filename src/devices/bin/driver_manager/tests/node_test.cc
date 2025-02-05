// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/node.h"

#include <fidl/fuchsia.component/cpp/wire_test_base.h>
#include <fidl/fuchsia.driver.host/cpp/test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/sync/cpp/completion.h>

#include <bind/fuchsia/platform/cpp/bind.h>

#include "src/devices/bin/driver_manager/driver_host.h"
#include "src/devices/bin/driver_manager/tests/driver_manager_test_base.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

class TestRealm final : public fidl::testing::WireTestBase<fuchsia_component::Realm> {
 public:
  TestRealm(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  fidl::ClientEnd<fuchsia_component::Realm> Connect() {
    auto [client_end, server_end] = fidl::Endpoints<fuchsia_component::Realm>::Create();
    fidl::BindServer(dispatcher_, std::move(server_end), this);
    return std::move(client_end);
  }

  void CreateChild(CreateChildRequestView request, CreateChildCompleter::Sync& completer) override {
  }

  void DestroyChild(DestroyChildRequestView request,
                    DestroyChildCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }

 private:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ZX_PANIC("Unimplemented %s", name.c_str());
  }

  async_dispatcher_t* dispatcher_;
};

class FakeDriverHost : public driver_manager::DriverHost {
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
    cb(zx::ok());
  }

  zx::result<uint64_t> GetProcessKoid() const override { return zx::error(ZX_ERR_NOT_SUPPORTED); }

  void CloseDriver(std::string node_name) {
    drivers_[node_name].Close(ZX_OK);
    clients_[node_name].reset();
  }

  std::tuple<fidl::ServerEnd<fuchsia_driver_host::Driver>,
             fidl::ClientEnd<fuchsia_driver_framework::Node>>
  TakeDriver(const std::string& node_name) {
    auto driver = std::move(drivers_[node_name]);
    auto client = std::move(clients_[node_name]);
    drivers_.erase(node_name);
    clients_.erase(node_name);
    return std::make_tuple(std::move(driver), std::move(client));
  }

 private:
  std::unordered_map<std::string, fidl::ServerEnd<fuchsia_driver_host::Driver>> drivers_;
  std::unordered_map<std::string, fidl::ClientEnd<fuchsia_driver_framework::Node>> clients_;
};

class FakeNodeManager : public TestNodeManagerBase {
 public:
  FakeNodeManager(fidl::WireClient<fuchsia_component::Realm> realm) : realm_(std::move(realm)) {}

  zx::result<driver_manager::DriverHost*> CreateDriverHost(bool use_next_vdso) override {
    return zx::ok(&driver_host_);
  }

  void DestroyDriverComponent(
      driver_manager::Node& node,
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

  FakeDriverHost& driver_host() { return driver_host_; }

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

class FakeDriver : public fidl::testing::TestBase<fuchsia_driver_host::Driver> {
 public:
  using StopCallback = fit::function<void()>;

  FakeDriver(
      async_dispatcher_t* dispatcher, fidl::ServerEnd<fuchsia_driver_host::Driver> server_end,
      fidl::ClientEnd<fuchsia_driver_framework::Node> node, StopCallback stop_callback = []() {})
      : binding_(async_get_default_dispatcher(), std::move(server_end), this,
                 fidl::kIgnoreBindingClosure),
        node_(std::move(node)),
        stop_callback_(std::move(stop_callback)) {}

  void Stop(StopCompleter::Sync& completer) override {
    stop_callback_();
    node_.reset();
    binding_.Close(ZX_OK);
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: Driver::%s\n", name.c_str());
  }

 private:
  fidl::ServerBinding<fuchsia_driver_host::Driver> binding_;
  fidl::ClientEnd<fuchsia_driver_framework::Node> node_;
  StopCallback stop_callback_;
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
    node_manager = std::make_unique<FakeNodeManager>(
        fidl::WireClient<fuchsia_component::Realm>(std::move(client), dispatcher()));
  }

  void StartTestDriver(std::shared_ptr<driver_manager::Node> node,
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

    auto [_, server_end] = fidl::Endpoints<fuchsia_io::Directory>::Create();

    auto start_info = fuchsia_component_runner::ComponentStartInfo{{
        .resolved_url = "fuchsia-boot:///#meta/test-driver.cm",
        .program = fuchsia_data::Dictionary{{.entries = std::move(program_entries)}},
        .outgoing_dir = std::move(server_end),
    }};

    auto controller_endpoints =
        fidl::Endpoints<fuchsia_component_runner::ComponentController>::Create();

    node_manager->AddClient(node->name(), std::move(controller_endpoints.client));

    fidl::Arena arena;
    node->StartDriver(fidl::ToWire(arena, std::move(start_info)),
                      std::move(controller_endpoints.server),
                      [node](zx::result<> result) { node->CompleteBind(result); });
  }

 protected:
  fidl::WireClient<fuchsia_device::Controller> ConnectToDeviceController(
      std::shared_ptr<driver_manager::Node> node) {
    zx::result device_controller_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
    EXPECT_EQ(device_controller_endpoints.status_value(), ZX_OK);
    device_controller_bindings_.AddBinding(dispatcher(),
                                           std::move(device_controller_endpoints->server),
                                           node.get(), fidl::kIgnoreBindingClosure);
    return {std::move(device_controller_endpoints->client), dispatcher()};
  }

  driver_manager::NodeManager* GetNodeManager() override { return node_manager.get(); }

  std::unique_ptr<FakeNodeManager> node_manager;

 private:
  std::unique_ptr<TestRealm> realm_;
  fidl::ServerBindingGroup<fuchsia_device::Controller> device_controller_bindings_;
};

TEST_F(Dfv2NodeTest, AddChildWithDuplicatePropertyArgs) {
  auto node = CreateNode("test");
  StartTestDriver(node);
  ASSERT_TRUE(node->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kRunning, node->GetNodeState());

  // Add child with both properties and properties2 set in the args. The call should
  // fail.
  zx::result node_controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ASSERT_EQ(node_controller_endpoints.status_value(), ZX_OK);

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ASSERT_EQ(node_endpoints.status_value(), ZX_OK);

  fuchsia_driver_framework::NodeAddArgs args{
      {.name = "child",
       .properties = {{fdf::MakeProperty("key", "value")}},
       .properties2 = {{fdf::MakeProperty2("key", "value")}}}};
  node->AddChild(std::move(args), std::move(node_controller_endpoints->server),
                 std::move(node_endpoints->server),
                 [](fit::result<fuchsia_driver_framework::wire::NodeError,
                                std::shared_ptr<driver_manager::Node>>
                        result) {
                   ASSERT_TRUE(result.is_error());
                   ASSERT_EQ(result.error_value(),
                             fuchsia_driver_framework::wire::NodeError::kUnsupportedArgs);
                 });
}

TEST_F(Dfv2NodeTest, RemoveDuringFailedBind) {
  auto node = CreateNode("test");
  StartTestDriver(node);
  ASSERT_TRUE(node->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kRunning, node->GetNodeState());

  node->Remove(driver_manager::RemovalSet::kAll, nullptr);
  RunLoopUntilIdle();

  ASSERT_EQ(driver_manager::NodeState::kWaitingOnDriver, node->GetNodeState());

  node->CompleteBind(zx::error(ZX_ERR_NOT_FOUND));
  RunLoopUntilIdle();
  ASSERT_FALSE(node->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kStopped, node->GetNodeState());
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

  auto composite = CreateCompositeNode("composite", {parent_1, parent_2}, {{}, {}},
                                       /* primary_index */ 0);

  ASSERT_FALSE(composite->EvaluateRematchFlags(
      fuchsia_driver_development::RestartRematchFlags::kRequested |
          fuchsia_driver_development::RestartRematchFlags::kNonRequested,
      "some-url"));
}

TEST_F(Dfv2NodeTest, RemoveCompositeNodeForRebind) {
  auto parent_node_1 = CreateNode("parent_1");
  StartTestDriver(parent_node_1);
  ASSERT_TRUE(parent_node_1->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kRunning, parent_node_1->GetNodeState());

  auto parent_node_2 = CreateNode("parent_2");
  StartTestDriver(parent_node_2);
  ASSERT_TRUE(parent_node_2->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kRunning, parent_node_2->GetNodeState());

  auto composite = CreateCompositeNode("composite", {parent_node_1, parent_node_2}, {{}, {}});
  StartTestDriver(composite);
  ASSERT_TRUE(composite->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kRunning, composite->GetNodeState());

  ASSERT_EQ(1u, parent_node_1->children().size());
  ASSERT_EQ(1u, parent_node_2->children().size());

  auto remove_callback_succeeded = false;
  composite->RemoveCompositeNodeForRebind([&remove_callback_succeeded](zx::result<> result) {
    if (result.is_ok()) {
      remove_callback_succeeded = true;
    }
  });
  RunLoopUntilIdle();
  ASSERT_EQ(driver_manager::NodeState::kWaitingOnDriver, composite->GetNodeState());
  ASSERT_EQ(driver_manager::ShutdownIntent::kRebindComposite, composite->shutdown_intent());

  node_manager->CloseDriverForNode("composite");
  RunLoopUntilIdle();
  ASSERT_TRUE(remove_callback_succeeded);

  ASSERT_EQ(driver_manager::NodeState::kStopped, composite->GetNodeState());
}

// Verify that we receives a callback for composite rebind if the node is deallocated
// before shutdown is complete.
TEST_F(Dfv2NodeTest, RemoveCompositeNodeForRebind_Dealloc) {
  auto parent_node_1 = CreateNode("parent_1");
  StartTestDriver(parent_node_1);
  ASSERT_TRUE(parent_node_1->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kRunning, parent_node_1->GetNodeState());

  auto parent_node_2 = CreateNode("parent_2");
  StartTestDriver(parent_node_2);
  ASSERT_TRUE(parent_node_2->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kRunning, parent_node_2->GetNodeState());

  auto composite = CreateCompositeNode("composite", {parent_node_1, parent_node_2}, {{}, {}});
  StartTestDriver(composite);
  ASSERT_TRUE(composite->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kRunning, composite->GetNodeState());

  ASSERT_EQ(1u, parent_node_1->children().size());
  ASSERT_EQ(1u, parent_node_2->children().size());

  bool is_cancelled = false;
  composite->RemoveCompositeNodeForRebind([&is_cancelled](zx::result<> result) {
    if (result.is_error()) {
      is_cancelled = result.error_value() == ZX_ERR_CANCELED;
    }
  });
  RunLoopUntilIdle();
  ASSERT_EQ(driver_manager::NodeState::kWaitingOnDriver, composite->GetNodeState());
  ASSERT_EQ(driver_manager::ShutdownIntent::kRebindComposite, composite->shutdown_intent());

  parent_node_1.reset();
  parent_node_2.reset();
  composite.reset();
  RunLoopUntilIdle();
  ASSERT_TRUE(is_cancelled);
}

TEST_F(Dfv2NodeTest, RestartOnCrashComposite) {
  auto parent_node_1 = CreateNode("parent_1");
  StartTestDriver(parent_node_1);
  ASSERT_TRUE(parent_node_1->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kRunning, parent_node_1->GetNodeState());

  auto parent_node_2 = CreateNode("parent_2");
  StartTestDriver(parent_node_2);
  ASSERT_TRUE(parent_node_2->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kRunning, parent_node_2->GetNodeState());

  auto composite = CreateCompositeNode("composite", {parent_node_1, parent_node_2}, {{}, {}});
  StartTestDriver(composite, {.host_restart_on_crash = true});

  ASSERT_TRUE(composite->HasDriverComponent());
  ASSERT_EQ(driver_manager::NodeState::kRunning, composite->GetNodeState());

  ASSERT_EQ(1u, parent_node_1->children().size());
  ASSERT_EQ(1u, parent_node_2->children().size());

  // Simulate a crash by closing the driver side of channels.
  node_manager->CloseDriverForNode("composite");
  RunLoopUntilIdle();

  // The node should come back to running state.
  ASSERT_EQ(driver_manager::NodeState::kRunning, composite->GetNodeState());
}

TEST_F(Dfv2NodeTest, TestCompositeNodeProperties) {
  const char* kParent1Name = "parent-1";
  const std::vector<fuchsia_driver_framework::NodeProperty2> kParent1NodeProperties{
      fdf::MakeProperty2("test-key-1", 2u)};
  const char* kParent2Name = "parent-2";
  const std::vector<fuchsia_driver_framework::NodeProperty2> kParent2NodeProperties{
      fdf::MakeProperty2("test-key-2", "test=value")};
  auto parent_1 = CreateNode(kParent1Name);
  parent_1->SetNonCompositeProperties(kParent1NodeProperties);
  auto parent_2 = CreateNode(kParent2Name);
  parent_2->SetNonCompositeProperties(kParent2NodeProperties);

  std::vector<fuchsia_driver_framework::NodePropertyEntry2> parent_properties;
  parent_properties.emplace_back(kParent1Name, kParent1NodeProperties);
  parent_properties.emplace_back(kParent2Name, kParent2NodeProperties);
  auto composite = CreateCompositeNode("composite", {parent_1, parent_2}, parent_properties,
                                       /* primary_index */ 0);

  // Verify primary parent properties. Primary parent should be parent 1.
  const auto& primary_parent_node_properties = composite->GetNodeProperties();
  ASSERT_TRUE(primary_parent_node_properties.has_value());
  ASSERT_EQ(1ul, primary_parent_node_properties->size());

  const auto& primary_parent_node_property_1 = primary_parent_node_properties.value()[0];
  ASSERT_EQ(kParent1NodeProperties[0].key(), std::string(primary_parent_node_property_1.key.get()));
  ASSERT_EQ(kParent1NodeProperties[0].value().int_value().value(),
            primary_parent_node_property_1.value.int_value());

  // Verify parent 1 properties.
  const auto& parent_1_node_properties = composite->GetNodeProperties(kParent1Name);
  ASSERT_TRUE(parent_1_node_properties.has_value());
  ASSERT_EQ(1ul, parent_1_node_properties->size());

  const auto& parent_1_node_property_1 = parent_1_node_properties.value()[0];
  ASSERT_EQ(kParent1NodeProperties[0].key(), std::string(parent_1_node_property_1.key.get()));
  ASSERT_TRUE(parent_1_node_property_1.value.is_int_value());
  ASSERT_EQ(kParent1NodeProperties[0].value().int_value().value(),
            parent_1_node_property_1.value.int_value());

  // Verify parent 2 properties.
  const auto& parent_2_node_properties = composite->GetNodeProperties(kParent2Name);
  ASSERT_TRUE(parent_2_node_properties.has_value());
  ASSERT_EQ(1ul, parent_2_node_properties->size());

  const auto& parent_2_node_property_1 = parent_2_node_properties.value()[0];
  ASSERT_EQ(kParent2NodeProperties[0].key(), parent_2_node_property_1.key.get());
  ASSERT_TRUE(parent_2_node_property_1.value.is_string_value());
  ASSERT_EQ(kParent2NodeProperties[0].value().string_value().value(),
            parent_2_node_property_1.value.string_value().get());
}

// Verify Node::UnbindChildren() unbinds all of the children of a node with zero child.
TEST_F(Dfv2NodeTest, UnbindChildrenZeroChildren) {
  auto parent = CreateNode("parent");
  auto device_controller = ConnectToDeviceController(parent);

  ASSERT_TRUE(parent->children().empty());
  device_controller->UnbindChildren().ThenExactlyOnce(
      [](auto& result) { ASSERT_EQ(result.status(), ZX_OK); });
  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_TRUE(parent->children().empty());
}

// Verify Node::UnbindChildren() unbinds all of the children of a node with one child.
TEST_F(Dfv2NodeTest, UnbindChildrenOneChild) {
  auto parent = CreateNode("parent");
  auto child = CreateNode("child", parent);
  auto device_controller = ConnectToDeviceController(parent);

  ASSERT_EQ(parent->children().size(), 1u);
  device_controller->UnbindChildren().ThenExactlyOnce(
      [](auto& result) { ASSERT_EQ(result.status(), ZX_OK); });
  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_TRUE(parent->children().empty());
}

// Verify Node::UnbindChildren() unbinds all of the children of a node with one child that has a
// driver bound to it.
TEST_F(Dfv2NodeTest, UnbindChildrenOneBoundChild) {
  const std::string kChildNodeName = "child";

  auto parent = CreateNode("parent");
  auto child = CreateNode(kChildNodeName, parent);
  StartTestDriver(child);
  ASSERT_TRUE(child->HasDriverComponent());
  auto device_controller = ConnectToDeviceController(parent);

  // Get the driver so that the test can properly close the driver's connection when the driver
  // receives a Stop fidl request.
  auto [driver_server, node_client] = node_manager->driver_host().TakeDriver(kChildNodeName);
  FakeDriver driver{dispatcher(), std::move(driver_server), std::move(node_client)};

  ASSERT_EQ(parent->children().size(), 1u);
  device_controller->UnbindChildren().ThenExactlyOnce(
      [](auto& result) { ASSERT_EQ(result.status(), ZX_OK); });
  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_TRUE(parent->children().empty());
}

// Verify Node::UnbindChildren() unbinds all of the children of a node with four children.
TEST_F(Dfv2NodeTest, UnbindChildrenFourChildren) {
  const size_t kNumChildren = 4;

  auto parent = CreateNode("parent");
  std::vector<std::shared_ptr<driver_manager::Node>> children;
  for (size_t i = 0; i < kNumChildren; ++i) {
    children.emplace_back(CreateNode("child-" + std::to_string(i), parent));
  }
  auto device_controller = ConnectToDeviceController(parent);

  ASSERT_EQ(parent->children().size(), kNumChildren);
  device_controller->UnbindChildren().ThenExactlyOnce(
      [](auto& result) { ASSERT_EQ(result.status(), ZX_OK); });
  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_TRUE(parent->children().empty());
}

// Verify that multiple requests to Node::UnbindChildren() will succeed. Both of these requests
// are sent before the node can complete either.
TEST_F(Dfv2NodeTest, UnbindChildrenMultipleCalls) {
  auto parent = CreateNode("parent");
  auto child = CreateNode("child", parent);
  auto device_controller = ConnectToDeviceController(parent);

  ASSERT_EQ(parent->children().size(), 1u);
  size_t unbind_children_complete_count = 0;
  device_controller->UnbindChildren().ThenExactlyOnce(
      [&unbind_children_complete_count](auto& result) {
        ASSERT_EQ(result.status(), ZX_OK);
        unbind_children_complete_count += 1;
      });
  device_controller->UnbindChildren().ThenExactlyOnce(
      [&unbind_children_complete_count](auto& result) {
        ASSERT_EQ(result.status(), ZX_OK);
        unbind_children_complete_count += 1;
      });
  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_EQ(unbind_children_complete_count, 2u);
  ASSERT_TRUE(parent->children().empty());
}

// Verify that Node::AddChild() fails when a node is in the middle of unbinding children.
// TODO(https://fxbug.dev/333783189): Re-enable flaky test case.
TEST_F(Dfv2NodeTest, DISABLED_UnbindChildrenFailAddChild) {
  const std::string kChildNode1Name = "child-1";

  auto parent = CreateNode("parent");
  auto child_1 = CreateNode(kChildNode1Name, parent);
  StartTestDriver(child_1);
  ASSERT_TRUE(child_1->HasDriverComponent());
  auto device_controller = ConnectToDeviceController(parent);

  // Get the driver so that the test can prevent Node::UnbindChildren() from fully completing by
  // pausing TestDriver::Stop(). The driver will live on a separate thread in order to not block
  // the main thread while the driver waits to complete stopping.
  async::Loop driver_loop{&kAsyncLoopConfigNoAttachToCurrentThread};
  ASSERT_EQ(driver_loop.StartThread("driver"), ZX_OK);
  auto [driver_server, node_client] = node_manager->driver_host().TakeDriver(kChildNode1Name);
  auto complete_stop = std::make_shared<libsync::Completion>();
  async_patterns::TestDispatcherBound<FakeDriver> driver{
      driver_loop.dispatcher(),       std::in_place,
      async_patterns::PassDispatcher, std::move(driver_server),
      std::move(node_client),         [complete_stop]() {
        complete_stop->Wait(); }};

  ASSERT_EQ(parent->children().size(), 1u);
  device_controller->UnbindChildren().ThenExactlyOnce(
      [](auto& result) { ASSERT_EQ(result.status(), ZX_OK); });

  ASSERT_TRUE(RunLoopUntilIdle());
  // At this point, the node has received the UnbindChildren request and is waiting for child-1's
  // driver to fully stop.

  // Fail to add a second child.
  zx::result node_controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ASSERT_EQ(node_controller_endpoints.status_value(), ZX_OK);
  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ASSERT_EQ(node_endpoints.status_value(), ZX_OK);
  fuchsia_driver_framework::NodeAddArgs args{{.name = "child-2"}};
  parent->AddChild(std::move(args), std::move(node_controller_endpoints->server),
                   std::move(node_endpoints->server),
                   [](fit::result<fuchsia_driver_framework::wire::NodeError,
                                  std::shared_ptr<driver_manager::Node>>
                          result) {
                     ASSERT_TRUE(result.is_error());
                     ASSERT_EQ(
                         result.error_value(),
                         fuchsia_driver_framework::wire::NodeError::kUnbindChildrenInProgress);
                   });

  // Let the driver complete stopping.
  complete_stop->Signal();
  // Wait for the driver to complete stopping.
  driver.SyncCall([](FakeDriver* driver) {});

  // Let Node::UnbindChildren() complete.
  ASSERT_TRUE(RunLoopUntilIdle());

  ASSERT_TRUE(parent->children().empty());
}

// Verify `Node::ScheduleUnbind` will unbind a node that is bound to a driver.
TEST_F(Dfv2NodeTest, ScheduleUnbind) {
  const std::string kNodeName = "test";

  auto node = CreateNode(kNodeName);
  StartTestDriver(node);
  ASSERT_TRUE(node->HasDriverComponent());

  // Get the driver so that the test can properly close the driver's connection when the driver
  // receives a Stop fidl request.
  auto [driver_server, node_client] = node_manager->driver_host().TakeDriver(kNodeName);
  FakeDriver driver{dispatcher(), std::move(driver_server), std::move(node_client)};

  auto device_controller = ConnectToDeviceController(node);
  device_controller->ScheduleUnbind().ThenExactlyOnce(
      [](auto& result) { ASSERT_EQ(result.status(), ZX_OK); });
  RunLoopUntilIdle();
  ASSERT_FALSE(node->HasDriverComponent());
}
