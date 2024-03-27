// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/driver_runner.h"

#include <fidl/fuchsia.component.decl/cpp/test_base.h>
#include <fidl/fuchsia.component/cpp/test_base.h>
#include <fidl/fuchsia.driver.framework/cpp/test_base.h>
#include <fidl/fuchsia.driver.host/cpp/test_base.h>
#include <fidl/fuchsia.io/cpp/test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fit/defer.h>
#include <lib/inspect/cpp/reader.h>
#include <lib/inspect/testing/cpp/inspect.h>

#include <bind/fuchsia/platform/cpp/bind.h>

#include "src/devices/bin/driver_manager/composite_node_spec_v2.h"
#include "src/devices/bin/driver_manager/testing/fake_driver_index.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"

namespace fdata = fuchsia_data;
namespace fdfw = fuchsia_driver_framework;
namespace fdh = fuchsia_driver_host;
namespace fio = fuchsia_io;
namespace fprocess = fuchsia_process;
namespace frunner = fuchsia_component_runner;
namespace fcomponent = fuchsia_component;
namespace fdecl = fuchsia_component_decl;

using driver_manager::BindResultTracker;
using driver_manager::Collection;
using driver_manager::CompositeNodeSpecCreateInfo;
using driver_manager::CompositeNodeSpecV2;
using driver_manager::CreateCompositeServiceOffer;
using driver_manager::Devfs;
using driver_manager::DriverRunner;
using driver_manager::InspectManager;
using driver_manager::Node;
using driver_manager::NodeType;
using testing::ElementsAre;

const std::string root_driver_url = "fuchsia-boot:///#meta/root-driver.cm";
const std::string root_driver_binary = "driver/root-driver.so";

const std::string second_driver_url = "fuchsia-boot:///#meta/second-driver.cm";
const std::string second_driver_binary = "driver/second-driver.so-";

struct NodeChecker {
  std::vector<std::string> node_name;
  std::vector<std::string> child_names;
  std::map<std::string, std::string> str_properties;
};

struct CreatedChild {
  std::optional<fidl::Client<fdfw::Node>> node;
  std::optional<fidl::Client<fdfw::NodeController>> node_controller;
};

void CheckNode(const inspect::Hierarchy& hierarchy, const NodeChecker& checker) {
  auto node = hierarchy.GetByPath(checker.node_name);
  ASSERT_NE(nullptr, node);

  if (node->children().size() != checker.child_names.size()) {
    printf("Mismatched children\n");
    for (size_t i = 0; i < node->children().size(); i++) {
      printf("Child %ld : %s\n", i, node->children()[i].name().c_str());
    }
    ASSERT_EQ(node->children().size(), checker.child_names.size());
  }

  for (auto& child : checker.child_names) {
    auto ptr = node->GetByPath({child});
    if (!ptr) {
      printf("Failed to find child %s\n", child.c_str());
    }
    ASSERT_NE(nullptr, ptr);
  }

  for (auto& property : checker.str_properties) {
    auto prop = node->node().get_property<inspect::StringPropertyValue>(property.first);
    if (!prop) {
      printf("Failed to find property %s\n", property.first.c_str());
    }
    ASSERT_EQ(property.second, prop->value());
  }
}

zx::result<fidl::ClientEnd<fuchsia_ldsvc::Loader>> LoaderFactory() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_ldsvc::Loader>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  return zx::ok(std::move(endpoints->client));
}

fdecl::ChildRef CreateChildRef(std::string name, std::string collection) {
  return fdecl::ChildRef({.name = std::move(name), .collection = std::move(collection)});
}

class FakeContext : public fpromise::context {
 public:
  fpromise::executor* executor() const override {
    EXPECT_TRUE(false);
    return nullptr;
  }

  fpromise::suspended_task suspend_task() override {
    EXPECT_TRUE(false);
    return fpromise::suspended_task();
  }
};

fidl::AnyTeardownObserver TeardownWatcher(size_t index, std::vector<size_t>& indices) {
  return fidl::ObserveTeardown([&indices = indices, index] { indices.emplace_back(index); });
}

class TestRealm : public fidl::testing::TestBase<fcomponent::Realm> {
 public:
  using CreateChildHandler = fit::function<void(fdecl::CollectionRef collection, fdecl::Child decl,
                                                std::vector<fdecl::Offer> offers)>;
  using OpenExposedDirHandler =
      fit::function<void(fdecl::ChildRef child, fidl::ServerEnd<fio::Directory> exposed_dir)>;

  void SetCreateChildHandler(CreateChildHandler create_child_handler) {
    create_child_handler_ = std::move(create_child_handler);
  }

  void SetOpenExposedDirHandler(OpenExposedDirHandler open_exposed_dir_handler) {
    open_exposed_dir_handler_ = std::move(open_exposed_dir_handler);
  }

  fidl::VectorView<fprocess::wire::HandleInfo> TakeHandles(fidl::AnyArena& arena) {
    if (handles_.has_value()) {
      return fidl::ToWire(arena, std::move(handles_));
    }

    return fidl::VectorView<fprocess::wire::HandleInfo>(arena, 0);
  }

  void AssertDestroyedChildren(const std::vector<fdecl::ChildRef>& expected) {
    auto destroyed_children = destroyed_children_;
    for (const auto& child : expected) {
      auto it = std::find_if(destroyed_children.begin(), destroyed_children.end(),
                             [&child](const fdecl::ChildRef& other) {
                               return child.name() == other.name() &&
                                      child.collection() == other.collection();
                             });
      ASSERT_NE(it, destroyed_children.end());
      destroyed_children.erase(it);
    }
    ASSERT_EQ(destroyed_children.size(), 0ul);
  }

 private:
  void CreateChild(CreateChildRequest& request, CreateChildCompleter::Sync& completer) override {
    handles_ = std::move(request.args().numbered_handles());
    auto offers = request.args().dynamic_offers();
    create_child_handler_(
        std::move(request.collection()), std::move(request.decl()),
        offers.has_value() ? std::move(offers.value()) : std::vector<fdecl::Offer>{});
    completer.Reply(fidl::Response<fuchsia_component::Realm::CreateChild>(fit::ok()));
  }

  void DestroyChild(DestroyChildRequest& request, DestroyChildCompleter::Sync& completer) override {
    destroyed_children_.push_back(std::move(request.child()));
    completer.Reply(fidl::Response<fuchsia_component::Realm::DestroyChild>(fit::ok()));
  }

  void OpenExposedDir(OpenExposedDirRequest& request,
                      OpenExposedDirCompleter::Sync& completer) override {
    open_exposed_dir_handler_(std::move(request.child()), std::move(request.exposed_dir()));
    completer.Reply(fidl::Response<fuchsia_component::Realm::OpenExposedDir>(fit::ok()));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: Realm::%s\n", name.c_str());
  }

  CreateChildHandler create_child_handler_;
  OpenExposedDirHandler open_exposed_dir_handler_;
  std::optional<std::vector<fprocess::HandleInfo>> handles_;
  std::vector<fdecl::ChildRef> destroyed_children_;
};

class TestDirectory : public fidl::testing::TestBase<fio::Directory> {
 public:
  using OpenHandler =
      fit::function<void(const std::string& path, fidl::ServerEnd<fio::Node> object)>;

  explicit TestDirectory(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  void Bind(fidl::ServerEnd<fio::Directory> request) {
    bindings_.AddBinding(dispatcher_, std::move(request), this, fidl::kIgnoreBindingClosure);
  }

  void SetOpenHandler(OpenHandler open_handler) { open_handler_ = std::move(open_handler); }

 private:
  void Clone(CloneRequest& request, CloneCompleter::Sync& completer) override {
    EXPECT_EQ(fio::OpenFlags::kCloneSameRights, request.flags());
    fidl::ServerEnd<fio::Directory> dir(request.object().TakeChannel());
    Bind(std::move(dir));
  }

  void Open(OpenRequest& request, OpenCompleter::Sync& completer) override {
    open_handler_(request.path(), std::move(request.object()));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: Directory::%s\n", name.c_str());
  }

  async_dispatcher_t* dispatcher_;
  fidl::ServerBindingGroup<fio::Directory> bindings_;
  OpenHandler open_handler_;
};

class TestDriver : public fidl::testing::TestBase<fdh::Driver> {
 public:
  explicit TestDriver(async_dispatcher_t* dispatcher, fidl::ClientEnd<fdfw::Node> node,
                      fidl::ServerEnd<fdh::Driver> server)
      : dispatcher_(dispatcher),
        stop_handler_([]() {}),
        node_(std::move(node), dispatcher),
        driver_binding_(dispatcher, std::move(server), this, fidl::kIgnoreBindingClosure) {}

  fidl::Client<fdfw::Node>& node() { return node_; }

  using StopHandler = fit::function<void()>;
  void SetStopHandler(StopHandler handler) { stop_handler_ = std::move(handler); }

  void SetDontCloseBindingInStop() { dont_close_binding_in_stop_ = true; }

  void Stop(StopCompleter::Sync& completer) override {
    stop_handler_();
    if (!dont_close_binding_in_stop_) {
      driver_binding_.Close(ZX_OK);
    }
  }

  void DropNode() { node_ = {}; }
  void CloseBinding() { driver_binding_.Close(ZX_OK); }

  std::shared_ptr<CreatedChild> AddChild(std::string_view child_name, bool owned,
                                         bool expect_error) {
    fdfw::NodeAddArgs args({.name = std::make_optional<std::string>(child_name)});
    return AddChild(std::move(args), owned, expect_error);
  }

  std::shared_ptr<CreatedChild> AddChild(
      fdfw::NodeAddArgs child_args, bool owned, bool expect_error,
      fit::function<void()> on_bind = []() {}) {
    auto controller_endpoints = fidl::CreateEndpoints<fdfw::NodeController>();
    ZX_ASSERT(ZX_OK == controller_endpoints.status_value());

    auto child_node_endpoints = fidl::CreateEndpoints<fdfw::Node>();
    ZX_ASSERT(ZX_OK == child_node_endpoints.status_value());

    fidl::ServerEnd<fdfw::Node> child_node_server = {};
    if (owned) {
      child_node_server = std::move(child_node_endpoints->server);
    }

    node_
        ->AddChild({std::move(child_args), std::move(controller_endpoints->server),
                    std::move(child_node_server)})
        .Then([expect_error](fidl::Result<fdfw::Node::AddChild> result) {
          if (expect_error) {
            EXPECT_TRUE(result.is_error());
          } else {
            EXPECT_TRUE(result.is_ok());
          }
        });

    class NodeEventHandler : public fidl::AsyncEventHandler<fdfw::Node> {
     public:
      explicit NodeEventHandler(std::shared_ptr<CreatedChild> child) : child_(std::move(child)) {}
      void on_fidl_error(::fidl::UnbindInfo error) override {
        child_->node.reset();
        delete this;
      }
      void handle_unknown_event(fidl::UnknownEventMetadata<fdfw::Node> metadata) override {}

     private:
      std::shared_ptr<CreatedChild> child_;
    };

    class ControllerEventHandler : public fidl::AsyncEventHandler<fdfw::NodeController> {
     public:
      explicit ControllerEventHandler(std::shared_ptr<CreatedChild> child,
                                      fit::function<void()> on_bind)
          : child_(std::move(child)), on_bind_(std::move(on_bind)) {}
      void OnBind() override { on_bind_(); }
      void on_fidl_error(::fidl::UnbindInfo error) override {
        child_->node_controller.reset();
        delete this;
      }
      void handle_unknown_event(
          fidl::UnknownEventMetadata<fdfw::NodeController> metadata) override {}

     private:
      std::shared_ptr<CreatedChild> child_;
      fit::function<void()> on_bind_;
    };

    std::shared_ptr<CreatedChild> child = std::make_shared<CreatedChild>();
    child->node_controller.emplace(std::move(controller_endpoints->client), dispatcher_,
                                   new ControllerEventHandler(child, std::move(on_bind)));
    if (owned) {
      child->node.emplace(std::move(child_node_endpoints->client), dispatcher_,
                          new NodeEventHandler(child));
    }

    return child;
  }

 private:
  async_dispatcher_t* dispatcher_;
  StopHandler stop_handler_;
  fidl::Client<fdfw::Node> node_;
  fidl::ServerBinding<fdh::Driver> driver_binding_;
  bool dont_close_binding_in_stop_ = false;

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: Driver::%s\n", name.c_str());
  }
};

class TestDriverHost : public fidl::testing::TestBase<fdh::DriverHost> {
 public:
  using StartHandler =
      fit::function<void(fdfw::DriverStartArgs start_args, fidl::ServerEnd<fdh::Driver> driver)>;

  void SetStartHandler(StartHandler start_handler) { start_handler_ = std::move(start_handler); }

 private:
  void Start(StartRequest& request, StartCompleter::Sync& completer) override {
    start_handler_(std::move(request.start_args()), std::move(request.driver()));
    completer.Reply(zx::ok());
  }

  void InstallLoader(InstallLoaderRequest& request,
                     InstallLoaderCompleter::Sync& completer) override {}

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: DriverHost::%s\n", name.data());
  }

  StartHandler start_handler_;
};

class TestTransaction : public fidl::Transaction {
 public:
  explicit TestTransaction(bool close) : close_(close) {}

 private:
  std::unique_ptr<Transaction> TakeOwnership() override {
    return std::make_unique<TestTransaction>(close_);
  }

  zx_status_t Reply(fidl::OutgoingMessage* message, fidl::WriteOptions write_options) override {
    EXPECT_TRUE(false);
    return ZX_OK;
  }

  void Close(zx_status_t epitaph) override {
    EXPECT_TRUE(close_) << "epitaph: " << zx_status_get_string(epitaph);
  }

  bool close_;
};

struct Driver {
  std::string url;
  std::string binary;
  bool colocate = false;
  bool close = false;
  bool host_restart_on_crash = false;
  bool use_next_vdso = false;
};

class DriverRunnerTest : public gtest::TestLoopFixture {
 public:
  void TearDown() override { Unbind(); }

 protected:
  InspectManager& inspect() { return inspect_; }
  TestRealm& realm() { return realm_; }
  TestDirectory& driver_dir() { return driver_dir_; }
  TestDriverHost& driver_host() { return driver_host_; }

  fidl::WireClient<fuchsia_device::Controller> ConnectToDeviceController(
      std::string_view child_name) {
    fs::SynchronousVfs vfs(dispatcher());
    zx::result dev_res = devfs().Connect(vfs);
    EXPECT_EQ(dev_res.status_value(), ZX_OK);
    fidl::WireClient<fuchsia_io::Directory> dev{std::move(*dev_res), dispatcher()};
    zx::result controller_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
    EXPECT_EQ(controller_endpoints.status_value(), ZX_OK);

    auto device_controller_path = std::string(child_name) + "/device_controller";
    EXPECT_EQ(
        dev->Open(fuchsia_io::OpenFlags::kNotDirectory, {},
                  fidl::StringView::FromExternal(device_controller_path),
                  fidl::ServerEnd<fuchsia_io::Node>(controller_endpoints->server.TakeChannel()))
            .status(),
        ZX_OK);
    EXPECT_TRUE(RunLoopUntilIdle());

    return fidl::WireClient<fuchsia_device::Controller>{std::move(controller_endpoints->client),
                                                        dispatcher()};
  }

  fidl::ClientEnd<fuchsia_component::Realm> ConnectToRealm() {
    zx::result realm_endpoints = fidl::CreateEndpoints<fcomponent::Realm>();
    ZX_ASSERT(ZX_OK == realm_endpoints.status_value());
    realm_binding_.emplace(dispatcher(), std::move(realm_endpoints->server), &realm_,
                           fidl::kIgnoreBindingClosure);
    return std::move(realm_endpoints->client);
  }

  FakeDriverIndex CreateDriverIndex() {
    return FakeDriverIndex(dispatcher(), [](auto args) -> zx::result<FakeDriverIndex::MatchResult> {
      if (args.name().get() == "second") {
        return zx::ok(FakeDriverIndex::MatchResult{
            .url = second_driver_url,
        });
      }

      if (args.name().get() == "dev-group-0") {
        return zx::ok(FakeDriverIndex::MatchResult{
            .spec = fdfw::CompositeParent({
                .composite = fdfw::CompositeInfo{{
                    .spec = fdfw::CompositeNodeSpec{{
                        .name = "test-group",
                        .parents = std::vector<fdfw::ParentSpec>(2),
                    }},
                    .matched_driver = fdfw::CompositeDriverMatch{{
                        .composite_driver = fdfw::CompositeDriverInfo{{
                            .composite_name = "test-composite",
                            .driver_info = fdfw::DriverInfo{{
                                .url = "fuchsia-boot:///#meta/composite-driver.cm",
                                .colocate = true,
                                .package_type = fdfw::DriverPackageType::kBoot,
                            }},
                        }},
                        .parent_names = {{"node-0", "node-1"}},
                        .primary_parent_index = 1,
                    }},
                }},
                .index = 0,
            })});
      }

      if (args.name().get() == "dev-group-1") {
        return zx::ok(FakeDriverIndex::MatchResult{
            .spec = fdfw::CompositeParent({
                .composite = fdfw::CompositeInfo{{
                    .spec = fdfw::CompositeNodeSpec{{
                        .name = "test-group",
                        .parents = std::vector<fdfw::ParentSpec>(2),
                    }},
                    .matched_driver = fdfw::CompositeDriverMatch{{
                        .composite_driver = fdfw::CompositeDriverInfo{{
                            .composite_name = "test-composite",
                            .driver_info = fdfw::DriverInfo{{
                                .url = "fuchsia-boot:///#meta/composite-driver.cm",
                                .colocate = true,
                                .package_type = fdfw::DriverPackageType::kBoot,
                            }},
                        }},
                        .parent_names = {{"node-0", "node-1"}},
                        .primary_parent_index = 1,
                    }},
                }},
                .index = 1,
            })});
      }

      return zx::error(ZX_ERR_NOT_FOUND);
    });
  }

  void SetupDriverRunner(FakeDriverIndex driver_index) {
    driver_index_.emplace(std::move(driver_index));
    driver_runner_.emplace(ConnectToRealm(), driver_index_->Connect(), inspect(), &LoaderFactory,
                           dispatcher(), false);
    SetupDevfs();
  }

  void SetupDriverRunner() { SetupDriverRunner(CreateDriverIndex()); }

  void PrepareRealmForDriverComponentStart(const std::string& name, const std::string& url) {
    realm().SetCreateChildHandler(
        [name, url](fdecl::CollectionRef collection, fdecl::Child decl, auto offers) {
          EXPECT_EQ("boot-drivers", collection.name());
          EXPECT_EQ(name, decl.name().value());
          EXPECT_EQ(url, decl.url().value());
        });
  }

  void PrepareRealmForSecondDriverComponentStart() {
    PrepareRealmForDriverComponentStart("dev.second", second_driver_url);
  }

  void PrepareRealmForStartDriverHost(bool use_next_vdso) {
    constexpr std::string_view kDriverHostName = "driver-host-";
    std::string coll = "driver-hosts";
    realm().SetCreateChildHandler(
        [coll, kDriverHostName, use_next_vdso](fdecl::CollectionRef collection, fdecl::Child decl,
                                               auto offers) {
          EXPECT_EQ(coll, collection.name());
          EXPECT_EQ(kDriverHostName, decl.name().value().substr(0, kDriverHostName.size()));
          if (use_next_vdso) {
            EXPECT_EQ("fuchsia-boot:///driver_host#meta/driver_host_next.cm", decl.url());
          } else {
            EXPECT_EQ("fuchsia-boot:///driver_host#meta/driver_host.cm", decl.url());
          }
        });
    realm().SetOpenExposedDirHandler(
        [this, coll, kDriverHostName](fdecl::ChildRef child, auto exposed_dir) {
          EXPECT_EQ(coll, child.collection().value_or(""));
          EXPECT_EQ(kDriverHostName, child.name().substr(0, kDriverHostName.size()));
          driver_host_dir_.Bind(std::move(exposed_dir));
        });
    driver_host_dir_.SetOpenHandler([this](const std::string& path, auto object) {
      EXPECT_EQ(fidl::DiscoverableProtocolName<fdh::DriverHost>, path);
      driver_host_binding_.emplace(dispatcher(),
                                   fidl::ServerEnd<fdh::DriverHost>(object.TakeChannel()),
                                   &driver_host_, fidl::kIgnoreBindingClosure);
    });
  }

  void StopDriverComponent(fidl::ClientEnd<frunner::ComponentController> component) {
    fidl::WireClient client(std::move(component), dispatcher());
    auto stop_result = client->Stop();
    ASSERT_EQ(ZX_OK, stop_result.status());
    EXPECT_TRUE(RunLoopUntilIdle());
  }

  struct StartDriverResult {
    std::unique_ptr<TestDriver> driver;
    fidl::ClientEnd<frunner::ComponentController> controller;
  };

  using StartDriverHandler = fit::function<void(TestDriver*, fdfw::DriverStartArgs)>;

  StartDriverResult StartDriver(Driver driver,
                                std::optional<StartDriverHandler> start_handler = std::nullopt) {
    std::unique_ptr<TestDriver> started_driver;
    driver_host().SetStartHandler(
        [&started_driver, dispatcher = dispatcher(), start_handler = std::move(start_handler)](
            fdfw::DriverStartArgs start_args, fidl::ServerEnd<fdh::Driver> driver) mutable {
          started_driver = std::make_unique<TestDriver>(
              dispatcher, std::move(start_args.node().value()), std::move(driver));
          start_args.node().reset();
          if (start_handler.has_value()) {
            start_handler.value()(started_driver.get(), std::move(start_args));
          }
        });

    if (!driver.colocate) {
      PrepareRealmForStartDriverHost(driver.use_next_vdso);
    }

    fidl::Arena arena;

    fidl::VectorView<fdata::wire::DictionaryEntry> program_entries(arena, 4);
    program_entries[0].key.Set(arena, "binary");
    program_entries[0].value = fdata::wire::DictionaryValue::WithStr(arena, driver.binary);

    program_entries[1].key.Set(arena, "colocate");
    program_entries[1].value =
        fdata::wire::DictionaryValue::WithStr(arena, driver.colocate ? "true" : "false");

    program_entries[2].key.Set(arena, "host_restart_on_crash");
    program_entries[2].value = fdata::wire::DictionaryValue::WithStr(
        arena, driver.host_restart_on_crash ? "true" : "false");

    program_entries[3].key.Set(arena, "use_next_vdso");
    program_entries[3].value =
        fdata::wire::DictionaryValue::WithStr(arena, driver.use_next_vdso ? "true" : "false");

    auto program_builder = fdata::wire::Dictionary::Builder(arena);
    program_builder.entries(program_entries);

    auto outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    EXPECT_EQ(ZX_OK, outgoing_endpoints.status_value());

    auto start_info_builder = frunner::wire::ComponentStartInfo::Builder(arena);
    start_info_builder.resolved_url(driver.url)
        .program(program_builder.Build())
        .outgoing_dir(std::move(outgoing_endpoints->server))
        .ns({})
        .numbered_handles(realm().TakeHandles(arena));

    auto controller_endpoints = fidl::CreateEndpoints<frunner::ComponentController>();
    EXPECT_EQ(ZX_OK, controller_endpoints.status_value());
    TestTransaction transaction(driver.close);
    {
      fidl::WireServer<frunner::ComponentRunner>::StartCompleter::Sync completer(&transaction);
      fidl::WireRequest<frunner::ComponentRunner::Start> request{
          start_info_builder.Build(), std::move(controller_endpoints->server)};
      static_cast<fidl::WireServer<frunner::ComponentRunner>&>(driver_runner().runner_for_tests())
          .Start(&request, completer);
    }
    RunLoopUntilIdle();
    return {std::move(started_driver), std::move(controller_endpoints->client)};
  }

  zx::result<StartDriverResult> StartRootDriver() {
    realm().SetCreateChildHandler(
        [](fdecl::CollectionRef collection, fdecl::Child decl, auto offers) {
          EXPECT_EQ("boot-drivers", collection.name());
          EXPECT_EQ("dev", decl.name());
          EXPECT_EQ(root_driver_url, decl.url());
        });
    auto start = driver_runner().StartRootDriver(root_driver_url);
    if (start.is_error()) {
      return start.take_error();
    }
    EXPECT_TRUE(RunLoopUntilIdle());

    StartDriverHandler start_handler = [](TestDriver* driver, fdfw::DriverStartArgs start_args) {
      ValidateProgram(start_args.program(), root_driver_binary, "false", "false", "false");
    };
    return zx::ok(StartDriver(
        {
            .url = root_driver_url,
            .binary = root_driver_binary,
        },
        std::move(start_handler)));
  }

  StartDriverResult StartSecondDriver(bool colocate = false, bool host_restart_on_crash = false,
                                      bool use_next_vdso = false) {
    StartDriverHandler start_handler = [colocate, host_restart_on_crash, use_next_vdso](
                                           TestDriver* driver, fdfw::DriverStartArgs start_args) {
      if (!colocate) {
        EXPECT_FALSE(start_args.symbols().has_value());
      }

      ValidateProgram(start_args.program(), second_driver_binary, colocate ? "true" : "false",
                      host_restart_on_crash ? "true" : "false", use_next_vdso ? "true" : "false");
    };
    return StartDriver(
        {
            .url = second_driver_url,
            .binary = second_driver_binary,
            .colocate = colocate,
            .host_restart_on_crash = host_restart_on_crash,
            .use_next_vdso = use_next_vdso,
        },
        std::move(start_handler));
  }

  void Unbind() {
    if (driver_host_binding_.has_value()) {
      driver_host_binding_.reset();
      EXPECT_TRUE(RunLoopUntilIdle());
    }
  }

  static void ValidateProgram(std::optional<::fuchsia_data::Dictionary>& program,
                              std::string_view binary, std::string_view colocate,
                              std::string_view host_restart_on_crash,
                              std::string_view use_next_vdso) {
    ZX_ASSERT(program.has_value());
    auto& entries_opt = program.value().entries();
    ZX_ASSERT(entries_opt.has_value());
    auto& entries = entries_opt.value();
    EXPECT_EQ(4u, entries.size());
    EXPECT_EQ("binary", entries[0].key());
    EXPECT_EQ(std::string(binary), entries[0].value()->str().value());
    EXPECT_EQ("colocate", entries[1].key());
    EXPECT_EQ(std::string(colocate), entries[1].value()->str().value());
    EXPECT_EQ("host_restart_on_crash", entries[2].key());
    EXPECT_EQ(std::string(host_restart_on_crash), entries[2].value()->str().value());
    EXPECT_EQ("use_next_vdso", entries[3].key());
    EXPECT_EQ(std::string(use_next_vdso), entries[3].value()->str().value());
  }

  static void AssertNodeBound(const std::shared_ptr<CreatedChild>& child) {
    auto& node = child->node;
    ASSERT_TRUE(node.has_value() && node.value().is_valid());
  }

  static void AssertNodeNotBound(const std::shared_ptr<CreatedChild>& child) {
    auto& node = child->node;
    ASSERT_FALSE(node.has_value() && node.value().is_valid());
  }

  static void AssertNodeControllerBound(const std::shared_ptr<CreatedChild>& child) {
    auto& controller = child->node_controller;
    ASSERT_TRUE(controller.has_value() && controller.value().is_valid());
  }

  static void AssertNodeControllerNotBound(const std::shared_ptr<CreatedChild>& child) {
    auto& controller = child->node_controller;
    ASSERT_FALSE(controller.has_value() && controller.value().is_valid());
  }

  inspect::Hierarchy Inspect() {
    FakeContext context;
    auto inspector = driver_runner().Inspect()(context).take_value();
    return inspect::ReadFromInspector(inspector)(context).take_value();
  }

  void SetupDevfs() { driver_runner().root_node()->SetupDevfsForRootNode(devfs_); }

  Devfs& devfs() {
    ZX_ASSERT(devfs_.has_value());
    return devfs_.value();
  }

  DriverRunner& driver_runner() { return driver_runner_.value(); }

  FakeDriverIndex& driver_index() { return driver_index_.value(); }

 private:
  TestRealm realm_;
  TestDirectory driver_host_dir_{dispatcher()};
  TestDirectory driver_dir_{dispatcher()};
  TestDriverHost driver_host_;
  std::optional<fidl::ServerBinding<fcomponent::Realm>> realm_binding_;
  std::optional<fidl::ServerBinding<fdh::DriverHost>> driver_host_binding_;

  std::optional<Devfs> devfs_;
  InspectManager inspect_{dispatcher()};
  std::optional<FakeDriverIndex> driver_index_;
  std::optional<DriverRunner> driver_runner_;
};

// Start the root driver.
TEST_F(DriverRunnerTest, StartRootDriver) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the root driver. Make sure that the driver is stopped before the Component is exited.
TEST_F(DriverRunnerTest, StartRootDriver_DriverStopBeforeComponentExit) {
  SetupDriverRunner();

  std::vector<size_t> event_order;

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());
  fidl::WireSharedClient<frunner::ComponentController> root_client(
      std::move(root_driver->controller), dispatcher(), TeardownWatcher(1, event_order));

  root_driver->driver->SetStopHandler([&event_order]() { event_order.push_back(0); });
  root_driver->driver->DropNode();
  EXPECT_TRUE(RunLoopUntilIdle());
  // Make sure the driver was stopped before we told the component framework the driver was stopped.
  EXPECT_THAT(event_order, ElementsAre(0, 1));
}

// Start the root driver, and add a child node owned by the root driver.
TEST_F(DriverRunnerTest, StartRootDriver_AddOwnedChild) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  root_driver->driver->AddChild("second", true, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the root driver, add a child node, then remove it.
TEST_F(DriverRunnerTest, StartRootDriver_RemoveOwnedChild) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  std::shared_ptr<CreatedChild> created_child =
      root_driver->driver->AddChild("second", true, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  AssertNodeBound(created_child);
  AssertNodeControllerBound(created_child);

  EXPECT_TRUE(created_child->node_controller.value()->Remove().is_ok());
  EXPECT_TRUE(RunLoopUntilIdle());

  AssertNodeNotBound(created_child);
  ASSERT_NE(nullptr, root_driver->driver.get());
  EXPECT_TRUE(root_driver->driver->node().is_valid());

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the root driver, and add two child nodes with duplicate names.
TEST_F(DriverRunnerTest, StartRootDriver_AddOwnedChild_DuplicateNames) {
  SetupDriverRunner();
  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("second", true, false);
  std::shared_ptr<CreatedChild> invalid_child = root_driver->driver->AddChild("second", true, true);
  EXPECT_TRUE(RunLoopUntilIdle());

  AssertNodeNotBound(invalid_child);
  AssertNodeBound(child);

  ASSERT_NE(nullptr, root_driver->driver.get());
  EXPECT_TRUE(root_driver->driver->node().is_valid());

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the root driver, and add a child node with an offer that is missing a
// source.
TEST_F(DriverRunnerTest, StartRootDriver_AddUnownedChild_OfferMissingSource) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  fdfw::NodeAddArgs args({
      .name = "second",
      .offers2 =
          {
              {
                  fuchsia_driver_framework::Offer::WithZirconTransport(
                      fuchsia_component_decl::Offer::WithProtocol(fdecl::OfferProtocol({
                          .target_name = std::make_optional<std::string>("fuchsia.package.Renamed"),
                      }))),
              },
          },
  });
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild(std::move(args), false, true);
  EXPECT_TRUE(RunLoopUntilIdle());
  AssertNodeControllerNotBound(child);

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the root driver, and add a child node with one offer that has a source
// and another that has a target.
TEST_F(DriverRunnerTest, StartRootDriver_AddUnownedChild_OfferHasRef) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  fdfw::NodeAddArgs args({
      .name = "second",
      .offers2 =
          {
              {
                  fuchsia_driver_framework::Offer::WithZirconTransport(
                      fuchsia_component_decl::Offer::WithProtocol(fdecl::OfferProtocol({
                          .source = fdecl::Ref::WithSelf(fdecl::SelfRef()),
                          .source_name = "fuchsia.package.Protocol",
                      }))),
                  fuchsia_driver_framework::Offer::WithZirconTransport(
                      fuchsia_component_decl::Offer::WithProtocol(fdecl::OfferProtocol({
                          .source_name = "fuchsia.package.Protocol",
                          .target = fdecl::Ref::WithSelf(fdecl::SelfRef()),
                      }))),
              },
          },
  });
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild(std::move(args), false, true);
  EXPECT_TRUE(RunLoopUntilIdle());
  AssertNodeControllerNotBound(child);

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the root driver, and add a child node with duplicate symbols. The child
// node is unowned, so if we did not have duplicate symbols, the second driver
// would bind to it.
TEST_F(DriverRunnerTest, StartRootDriver_AddUnownedChild_DuplicateSymbols) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  fdfw::NodeAddArgs args({
      .name = "second",
      .symbols =
          {
              {
                  fdfw::NodeSymbol({
                      .name = "sym",
                      .address = 0xf00d,
                  }),
                  fdfw::NodeSymbol({
                      .name = "sym",
                      .address = 0xf00d,
                  }),
              },
          },
  });
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild(std::move(args), false, true);
  EXPECT_TRUE(RunLoopUntilIdle());
  AssertNodeControllerNotBound(child);

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the root driver, and add a child node that has a symbol without an
// address.
TEST_F(DriverRunnerTest, StartRootDriver_AddUnownedChild_SymbolMissingAddress) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  fdfw::NodeAddArgs args({
      .name = "second",
      .symbols =
          {
              {
                  fdfw::NodeSymbol({
                      .name = std::make_optional<std::string>("sym"),
                  }),
              },
          },
  });
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild(std::move(args), false, true);
  EXPECT_TRUE(RunLoopUntilIdle());
  AssertNodeControllerNotBound(child);

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the root driver, and add a child node that has a symbol without a name.
TEST_F(DriverRunnerTest, StartRootDriver_AddUnownedChild_SymbolMissingName) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  fdfw::NodeAddArgs args({
      .name = "second",
      .symbols =
          {
              {
                  fdfw::NodeSymbol({
                      .name = std::nullopt,
                      .address = 0xfeed,
                  }),
              },
          },
  });
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild(std::move(args), false, true);
  EXPECT_TRUE(RunLoopUntilIdle());
  AssertNodeControllerNotBound(child);

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the root driver, and then start a second driver in a new driver host.
TEST_F(DriverRunnerTest, StartSecondDriver_NewDriverHost) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  realm().SetCreateChildHandler(
      [](fdecl::CollectionRef collection, fdecl::Child decl, std::vector<fdecl::Offer> offers) {
        EXPECT_EQ("boot-drivers", collection.name());
        EXPECT_EQ("dev.second", decl.name());
        EXPECT_EQ(second_driver_url, decl.url());

        EXPECT_EQ(1u, offers.size());
        ASSERT_TRUE(offers[0].Which() == fdecl::Offer::Tag::kProtocol);
        auto& protocol = offers[0].protocol().value();

        ASSERT_TRUE(protocol.source().has_value());
        ASSERT_TRUE(protocol.source().value().Which() == fdecl::Ref::Tag::kChild);
        auto& source_ref = protocol.source().value().child().value();
        EXPECT_EQ("dev", source_ref.name());
        EXPECT_EQ("boot-drivers", source_ref.collection().value_or("missing"));

        ASSERT_TRUE(protocol.source_name().has_value());
        EXPECT_EQ("fuchsia.package.Protocol", protocol.source_name().value());

        ASSERT_TRUE(protocol.target_name().has_value());
        EXPECT_EQ("fuchsia.package.Renamed", protocol.target_name());
      });

  fdfw::NodeAddArgs args({
      .name = "second",
      .symbols =
          {
              {
                  fdfw::NodeSymbol({
                      .name = "sym",
                      .address = 0xfeed,
                  }),
              },
          },
      .offers2 =
          {
              {
                  fuchsia_driver_framework::Offer::WithZirconTransport(
                      fuchsia_component_decl::Offer::WithProtocol(fdecl::OfferProtocol({
                          .source_name = "fuchsia.package.Protocol",
                          .target_name = "fuchsia.package.Renamed",
                      }))),
              },
          },
  });

  bool did_bind = false;
  auto on_bind = [&did_bind]() { did_bind = true; };
  std::shared_ptr<CreatedChild> child =
      root_driver->driver->AddChild(std::move(args), false, false, std::move(on_bind));
  EXPECT_TRUE(RunLoopUntilIdle());
  EXPECT_TRUE(did_bind);

  auto [driver, controller] = StartSecondDriver();

  driver->CloseBinding();
  driver->DropNode();
  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren(
      {CreateChildRef("dev", "boot-drivers"), CreateChildRef("dev.second", "boot-drivers")});
}

// Start the root driver, and then start a second driver in the same driver
// host.
TEST_F(DriverRunnerTest, StartSecondDriver_SameDriverHost) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  fdfw::NodeAddArgs args({
      .name = "second",
      .symbols =
          {
              {
                  fdfw::NodeSymbol({
                      .name = "sym",
                      .address = 0xfeed,
                  }),
              },
          },
      .offers2 =
          {
              {
                  fuchsia_driver_framework::Offer::WithZirconTransport(
                      fuchsia_component_decl::Offer::WithProtocol(fdecl::OfferProtocol({
                          .source_name = "fuchsia.package.Protocol",
                          .target_name = "fuchsia.package.Renamed",
                      }))),
              },
          },
  });

  bool did_bind = false;
  auto on_bind = [&did_bind]() { did_bind = true; };
  std::shared_ptr<CreatedChild> child =
      root_driver->driver->AddChild(std::move(args), false, false, std::move(on_bind));
  EXPECT_TRUE(RunLoopUntilIdle());
  EXPECT_TRUE(did_bind);

  StartDriverHandler start_handler = [](TestDriver* driver, fdfw::DriverStartArgs start_args) {
    auto& symbols = start_args.symbols().value();
    EXPECT_EQ(1u, symbols.size());
    EXPECT_EQ("sym", symbols[0].name().value());
    EXPECT_EQ(0xfeedu, symbols[0].address());
    ValidateProgram(start_args.program(), second_driver_binary, "true", "false", "false");
  };
  auto [driver, controller] = StartDriver(
      {
          .url = second_driver_url,
          .binary = second_driver_binary,
          .colocate = true,
      },
      std::move(start_handler));

  driver->CloseBinding();
  driver->DropNode();
  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren(
      {CreateChildRef("dev", "boot-drivers"), CreateChildRef("dev.second", "boot-drivers")});
}

// Start the root driver, and then start a second driver that we match based on
// node properties.
TEST_F(DriverRunnerTest, StartSecondDriver_UseProperties) {
  FakeDriverIndex driver_index(
      dispatcher(), [](auto args) -> zx::result<FakeDriverIndex::MatchResult> {
        if (args.has_properties() && args.properties()[0].key.is_int_value() &&
            args.properties()[0].key.int_value() == 0x1985 &&
            args.properties()[0].value.is_int_value() &&
            args.properties()[0].value.int_value() == 0x2301

            && args.properties()[1].key.is_string_value() &&
            args.properties()[1].key.string_value().get() ==
                bind_fuchsia_platform::DRIVER_FRAMEWORK_VERSION &&
            args.properties()[1].value.is_int_value() && args.properties()[1].value.int_value() == 2

        ) {
          return zx::ok(FakeDriverIndex::MatchResult{
              .url = second_driver_url,
          });
        } else {
          return zx::error(ZX_ERR_NOT_FOUND);
        }
      });
  SetupDriverRunner(std::move(driver_index));

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  fdfw::NodeAddArgs args({
      .name = "second",
      .properties =
          {
              {
                  fdfw::NodeProperty({
                      .key = fdfw::NodePropertyKey::WithIntValue(0x1985),
                      .value = fdfw::NodePropertyValue::WithIntValue(0x2301),
                  }),
              },
          },
  });

  std::shared_ptr<CreatedChild> child =
      root_driver->driver->AddChild(std::move(args), false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  auto [driver, controller] = StartSecondDriver(true);

  driver->CloseBinding();
  driver->DropNode();
  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren(
      {CreateChildRef("dev", "boot-drivers"), CreateChildRef("dev.second", "boot-drivers")});
}

// Start the second driver, and then disable and rematch it which should make it available for
// matching. Undisable the driver and then restart with rematch, which should get the node again.
TEST_F(DriverRunnerTest, StartSecondDriver_DisableAndRematch_UndisableAndRestart) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("second", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  auto [driver, controller] = StartSecondDriver();

  EXPECT_EQ(0u, driver_runner().bind_manager().NumOrphanedNodes());

  // Disable the second-driver url, and restart with rematching of the requested.
  driver_index().disable_driver_url(second_driver_url);
  zx::result count = driver_runner().RestartNodesColocatedWithDriverUrl(
      second_driver_url, fuchsia_driver_development::RestartRematchFlags::kRequested);
  EXPECT_EQ(1u, count.value());

  // Our driver should get closed.
  EXPECT_TRUE(RunLoopUntilIdle());
  zx_signals_t signals = 0;
  ASSERT_EQ(ZX_OK,
            controller.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(), &signals));
  ASSERT_TRUE(signals & ZX_CHANNEL_PEER_CLOSED);

  // Since we disabled the driver url, the rematch should have not gotten a match, and therefore
  // the node should haver become orphaned.
  EXPECT_EQ(1u, driver_runner().bind_manager().NumOrphanedNodes());

  // Undisable the driver, and try binding all available nodes. This should cause it to get
  // started again.
  driver_index().un_disable_driver_url(second_driver_url);

  PrepareRealmForSecondDriverComponentStart();
  driver_runner().TryBindAllAvailable();
  EXPECT_TRUE(RunLoopUntilIdle());

  auto [driver_2, controller_2] = StartSecondDriver();

  // This list should be empty now that it got bound again.
  EXPECT_EQ(0u, driver_runner().bind_manager().NumOrphanedNodes());

  StopDriverComponent(std::move(root_driver->controller));
  // The node was destroyed twice.
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers"),
                                   CreateChildRef("dev.second", "boot-drivers"),
                                   CreateChildRef("dev.second", "boot-drivers")});
}

// Start the second driver with host_restart_on_crash enabled, and then kill the driver host, and
// observe the node start the driver again in another host. Done by both a node client drop, and a
// driver host server binding close.
TEST_F(DriverRunnerTest, StartSecondDriverHostRestartOnCrash) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("second", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  auto [driver_1, controller_1] = StartSecondDriver(false, true);

  EXPECT_EQ(0u, driver_runner().bind_manager().NumOrphanedNodes());

  // Stop the driver host binding.
  PrepareRealmForSecondDriverComponentStart();
  driver_1->CloseBinding();
  EXPECT_TRUE(RunLoopUntilIdle());

  zx_signals_t signals = 0;
  ASSERT_EQ(ZX_OK, controller_1.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(),
                                                   &signals));
  ASSERT_TRUE(signals & ZX_CHANNEL_PEER_CLOSED);

  // The driver host and driver should be started again by the node.
  auto [driver_2, controller_2] = StartSecondDriver(false, true);

  // Drop the node client binding.
  PrepareRealmForSecondDriverComponentStart();
  driver_2->DropNode();
  EXPECT_TRUE(RunLoopUntilIdle());

  signals = 0;
  ASSERT_EQ(ZX_OK, controller_2.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(),
                                                   &signals));
  ASSERT_TRUE(signals & ZX_CHANNEL_PEER_CLOSED);

  // The driver host and driver should be started again by the node.
  auto [driver_3, controller_3] = StartSecondDriver(false, true);

  // Now try to drop the node and close the binding at the same time. They should not break each
  // other.
  PrepareRealmForSecondDriverComponentStart();
  driver_3->CloseBinding();
  EXPECT_TRUE(RunLoopUntilIdle());
  driver_3->DropNode();
  EXPECT_FALSE(RunLoopUntilIdle());

  signals = 0;
  ASSERT_EQ(ZX_OK, controller_3.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(),
                                                   &signals));
  ASSERT_TRUE(signals & ZX_CHANNEL_PEER_CLOSED);

  // The driver host and driver should be started again by the node.
  auto [driver_4, controller_4] = StartSecondDriver(false, true);

  // Again try to drop the node and close the binding at the same time but in opposite order.
  PrepareRealmForSecondDriverComponentStart();
  driver_4->DropNode();
  EXPECT_TRUE(RunLoopUntilIdle());
  driver_4->CloseBinding();
  EXPECT_FALSE(RunLoopUntilIdle());

  signals = 0;
  ASSERT_EQ(ZX_OK, controller_4.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(),
                                                   &signals));
  ASSERT_TRUE(signals & ZX_CHANNEL_PEER_CLOSED);

  // The driver host and driver should be started again by the node.
  auto [driver_5, controller_5] = StartSecondDriver(false, true);

  // Finally don't RunLoopUntilIdle in between the two.
  PrepareRealmForSecondDriverComponentStart();
  driver_5->CloseBinding();
  driver_5->DropNode();
  EXPECT_TRUE(RunLoopUntilIdle());

  signals = 0;
  ASSERT_EQ(ZX_OK, controller_5.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(),
                                                   &signals));
  ASSERT_TRUE(signals & ZX_CHANNEL_PEER_CLOSED);

  // The driver host and driver should be started again by the node.
  auto [driver_6, controller_6] = StartSecondDriver(false, true);

  StopDriverComponent(std::move(root_driver->controller));

  realm().AssertDestroyedChildren(
      {CreateChildRef("dev", "boot-drivers"), CreateChildRef("dev.second", "boot-drivers"),
       CreateChildRef("dev.second", "boot-drivers"), CreateChildRef("dev.second", "boot-drivers"),
       CreateChildRef("dev.second", "boot-drivers"), CreateChildRef("dev.second", "boot-drivers"),
       CreateChildRef("dev.second", "boot-drivers")});
}

// Start the second driver with use_next_vdso enabled,
TEST_F(DriverRunnerTest, StartSecondDriver_UseNextVdso) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("second", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  auto [driver_1, controller_1] = StartSecondDriver(true, false, true);

  EXPECT_EQ(0u, driver_runner().bind_manager().NumOrphanedNodes());

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren(
      {CreateChildRef("dev", "boot-drivers"), CreateChildRef("dev.second", "boot-drivers")});
}

// The root driver adds a node that only binds after a RequestBind() call.
TEST_F(DriverRunnerTest, BindThroughRequest) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("child", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  ASSERT_EQ(1u, driver_runner().bind_manager().NumOrphanedNodes());

  driver_index().set_match_callback([](auto args) -> zx::result<FakeDriverIndex::MatchResult> {
    return zx::ok(FakeDriverIndex::MatchResult{
        .url = second_driver_url,
    });
  });

  PrepareRealmForDriverComponentStart("dev.child", second_driver_url);
  AssertNodeControllerBound(child);
  child->node_controller.value()
      ->RequestBind(fdfw::NodeControllerRequestBindRequest())
      .Then([](auto result) {});
  EXPECT_TRUE(RunLoopUntilIdle());
  ASSERT_EQ(0u, driver_runner().bind_manager().NumOrphanedNodes());

  auto [driver, controller] = StartSecondDriver();

  driver->CloseBinding();
  driver->DropNode();
  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren(
      {CreateChildRef("dev", "boot-drivers"), CreateChildRef("dev.child", "boot-drivers")});
}

// The root driver adds a node that only binds after a RequestBind() call. Then Restarts through
// RequestBind() with force_rebind, once without a url suffix, and another with the url suffix.
TEST_F(DriverRunnerTest, BindAndRestartThroughRequest) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("child", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  ASSERT_EQ(1u, driver_runner().bind_manager().NumOrphanedNodes());

  driver_index().set_match_callback([](auto args) -> zx::result<FakeDriverIndex::MatchResult> {
    if (args.has_driver_url_suffix()) {
      return zx::ok(FakeDriverIndex::MatchResult{
          .url = "fuchsia-boot:///#meta/third-driver.cm",
      });
    }
    return zx::ok(FakeDriverIndex::MatchResult{
        .url = second_driver_url,
    });
  });

  // Prepare realm for the second-driver CreateChild.
  PrepareRealmForDriverComponentStart("dev.child", second_driver_url);

  // Bind the child node to the second-driver driver.
  auto bind_request = fdfw::NodeControllerRequestBindRequest();
  child->node_controller.value()
      ->RequestBind(fdfw::NodeControllerRequestBindRequest())
      .Then([](auto result) {});
  EXPECT_TRUE(RunLoopUntilIdle());
  ASSERT_EQ(0u, driver_runner().bind_manager().NumOrphanedNodes());

  // Get the second-driver running.
  auto [driver_1, controller_1] = StartSecondDriver();

  // Prepare realm for the second-driver CreateChild again.
  PrepareRealmForDriverComponentStart("dev.child", second_driver_url);

  // Request rebind of the second-driver to the node.
  child->node_controller.value()
      ->RequestBind(fdfw::NodeControllerRequestBindRequest({
          .force_rebind = true,
      }))
      .Then([](auto result) {});
  EXPECT_TRUE(RunLoopUntilIdle());

  // Get the second-driver running again.
  auto [driver_2, controller_2] = StartSecondDriver();

  // Prepare realm for the third-driver CreateChild.
  PrepareRealmForDriverComponentStart("dev.child", "fuchsia-boot:///#meta/third-driver.cm");

  // Request rebind of the node with the third-driver.
  child->node_controller.value()
      ->RequestBind(fdfw::NodeControllerRequestBindRequest({
          .force_rebind = true,
          .driver_url_suffix = "third",
      }))
      .Then([](auto result) {});
  EXPECT_TRUE(RunLoopUntilIdle());

  // Get the third-driver running.
  StartDriverHandler start_handler = [&](TestDriver* driver, fdfw::DriverStartArgs start_args) {
    EXPECT_FALSE(start_args.symbols().has_value());
    ValidateProgram(start_args.program(), "driver/third-driver.so", "false", "false", "false");
  };
  auto third_driver = StartDriver(
      {
          .url = "fuchsia-boot:///#meta/third-driver.cm",
          .binary = "driver/third-driver.so",
      },
      std::move(start_handler));

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({
      CreateChildRef("dev", "boot-drivers"),
      CreateChildRef("dev.child", "boot-drivers"),
      CreateChildRef("dev.child", "boot-drivers"),
      CreateChildRef("dev.child", "boot-drivers"),
  });
}

// Start the root driver, and then add a child node that does not bind to a
// second driver.
TEST_F(DriverRunnerTest, StartSecondDriver_UnknownNode) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("unknown-node", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  StartDriver({.close = true});
  ASSERT_EQ(1u, driver_runner().bind_manager().NumOrphanedNodes());

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the root driver, and then add a child node that only binds to a base driver.
TEST_F(DriverRunnerTest, StartSecondDriver_BindOrphanToBaseDriver) {
  bool base_drivers_loaded = false;
  FakeDriverIndex fake_driver_index(
      dispatcher(), [&base_drivers_loaded](auto args) -> zx::result<FakeDriverIndex::MatchResult> {
        if (base_drivers_loaded) {
          if (args.name().get() == "second") {
            return zx::ok(FakeDriverIndex::MatchResult{
                .url = second_driver_url,
            });
          }
        }
        return zx::error(ZX_ERR_NOT_FOUND);
      });
  SetupDriverRunner(std::move(fake_driver_index));

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  fdfw::NodeAddArgs args({
      .name = "second",
      .properties =
          {
              {
                  fdfw::NodeProperty({
                      .key = fdfw::NodePropertyKey::WithStringValue("driver.prop-one"),
                      .value = fdfw::NodePropertyValue::WithStringValue("value"),
                  }),
              },
          },
  });
  std::shared_ptr<CreatedChild> child =
      root_driver->driver->AddChild(std::move(args), false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  // Make sure the node we added was orphaned.
  ASSERT_EQ(1u, driver_runner().bind_manager().NumOrphanedNodes());

  // Set the handlers for the new driver.
  PrepareRealmForSecondDriverComponentStart();

  // Tell driver index to return the second driver, and wait for base drivers to load.
  base_drivers_loaded = true;
  driver_runner().ScheduleWatchForDriverLoad();
  ASSERT_TRUE(RunLoopUntilIdle());

  driver_index().InvokeWatchDriverResponse();
  ASSERT_TRUE(RunLoopUntilIdle());

  // See that we don't have an orphan anymore.
  ASSERT_EQ(0u, driver_runner().bind_manager().NumOrphanedNodes());

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the second driver, and then unbind its associated node.
TEST_F(DriverRunnerTest, StartSecondDriver_UnbindSecondNode) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("second", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());
  auto [driver, controller] = StartSecondDriver();

  // Unbinding the second node stops the driver bound to it.
  driver->DropNode();
  EXPECT_TRUE(RunLoopUntilIdle());
  zx_signals_t signals = 0;
  ASSERT_EQ(ZX_OK,
            controller.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(), &signals));
  ASSERT_TRUE(signals & ZX_CHANNEL_PEER_CLOSED);

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren(
      {CreateChildRef("dev", "boot-drivers"), CreateChildRef("dev.second", "boot-drivers")});
}

// Start the second driver, and then close the associated Driver protocol
// channel.
TEST_F(DriverRunnerTest, StartSecondDriver_CloseSecondDriver) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("second", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());
  auto [driver, controller] = StartSecondDriver();

  // Closing the Driver protocol channel of the second driver causes the driver
  // to be stopped.
  driver->CloseBinding();
  EXPECT_TRUE(RunLoopUntilIdle());
  zx_signals_t signals = 0;
  ASSERT_EQ(ZX_OK,
            controller.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(), &signals));
  ASSERT_TRUE(signals & ZX_CHANNEL_PEER_CLOSED);

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren(
      {CreateChildRef("dev", "boot-drivers"), CreateChildRef("dev.second", "boot-drivers")});
}

// Start a chain of drivers, and then unbind the second driver's node.
TEST_F(DriverRunnerTest, StartDriverChain_UnbindSecondNode) {
  FakeDriverIndex driver_index(dispatcher(),
                               [](auto args) -> zx::result<FakeDriverIndex::MatchResult> {
                                 std::string name(args.name().get());
                                 return zx::ok(FakeDriverIndex::MatchResult{
                                     .url = "fuchsia-boot:///#meta/" + name + "-driver.cm",
                                 });
                               });
  SetupDriverRunner(std::move(driver_index));

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  constexpr size_t kMaxNodes = 10;

  // The drivers vector will start with the root.
  std::vector<StartDriverResult> drivers;
  drivers.reserve(kMaxNodes + 1);
  drivers.emplace_back(std::move(root_driver.value()));

  std::vector<std::shared_ptr<CreatedChild>> children;
  children.reserve(kMaxNodes);

  std::string component_moniker = "dev";
  for (size_t i = 0; i < kMaxNodes; i++) {
    auto child_name = "node-" + std::to_string(i);
    component_moniker += "." + child_name;
    PrepareRealmForDriverComponentStart(component_moniker,
                                        "fuchsia-boot:///#meta/" + child_name + "-driver.cm");
    children.emplace_back(drivers.back().driver->AddChild(child_name, false, false));
    EXPECT_TRUE(RunLoopUntilIdle());

    StartDriverHandler start_handler = [](TestDriver* driver, fdfw::DriverStartArgs start_args) {
      EXPECT_FALSE(start_args.symbols().has_value());
      ValidateProgram(start_args.program(), "driver/driver.so", "false", "false", "false");
    };
    drivers.emplace_back(StartDriver(
        {
            .url = "fuchsia-boot:///#meta/node-" + std::to_string(i) + "-driver.cm",
            .binary = "driver/driver.so",
        },
        std::move(start_handler)));
  }

  // Unbinding the second node stops all drivers bound in the sub-tree, in a
  // depth-first order.
  std::vector<size_t> indices;
  std::vector<fidl::WireSharedClient<frunner::ComponentController>> clients;

  // Start at 1 since 0 is the root driver.
  for (size_t i = 1; i < drivers.size(); i++) {
    clients.emplace_back(std::move(drivers[i].controller), dispatcher(),
                         TeardownWatcher(clients.size() + 1, indices));
  }

  drivers[1].driver->DropNode();
  EXPECT_TRUE(RunLoopUntilIdle());
  EXPECT_THAT(indices, ElementsAre(10, 9, 8, 7, 6, 5, 4, 3, 2, 1));

  StopDriverComponent(std::move(drivers[0].controller));
  realm().AssertDestroyedChildren(
      {CreateChildRef("dev", "boot-drivers"), CreateChildRef("dev.node-0", "boot-drivers"),
       CreateChildRef("dev.node-0.node-1", "boot-drivers"),
       CreateChildRef("dev.node-0.node-1.node-2", "boot-drivers"),
       CreateChildRef("dev.node-0.node-1.node-2.node-3", "boot-drivers"),
       CreateChildRef("dev.node-0.node-1.node-2.node-3.node-4", "boot-drivers"),
       CreateChildRef("dev.node-0.node-1.node-2.node-3.node-4.node-5", "boot-drivers"),
       CreateChildRef("dev.node-0.node-1.node-2.node-3.node-4.node-5.node-6", "boot-drivers"),
       CreateChildRef("dev.node-0.node-1.node-2.node-3.node-4.node-5.node-6.node-7",
                      "boot-drivers"),
       CreateChildRef("dev.node-0.node-1.node-2.node-3.node-4.node-5.node-6.node-7.node-8",
                      "boot-drivers"),
       CreateChildRef("dev.node-0.node-1.node-2.node-3.node-4.node-5.node-6.node-7.node-8.node-9",
                      "boot-drivers")});
}

// Start the second driver, and then unbind the root node.
TEST_F(DriverRunnerTest, StartSecondDriver_UnbindRootNode) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("second", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());
  auto [driver, controller] = StartSecondDriver();

  // Unbinding the root node stops all drivers.
  std::vector<size_t> indices;
  fidl::WireSharedClient<frunner::ComponentController> root_client(
      std::move(root_driver->controller), dispatcher(), TeardownWatcher(0, indices));
  fidl::WireSharedClient<frunner::ComponentController> second_client(
      std::move(controller), dispatcher(), TeardownWatcher(1, indices));
  root_driver->driver->DropNode();
  EXPECT_TRUE(RunLoopUntilIdle());
  EXPECT_THAT(indices, ElementsAre(1, 0));
}

// Start the second driver, and then Stop the root node.
TEST_F(DriverRunnerTest, StartSecondDriver_StopRootNode) {
  SetupDriverRunner();

  // These represent the order that Driver::Stop is called
  std::vector<size_t> driver_stop_indices;

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  root_driver->driver->SetStopHandler(
      [&driver_stop_indices]() { driver_stop_indices.push_back(0); });

  PrepareRealmForSecondDriverComponentStart();
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("second", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());
  auto [driver, controller] = StartSecondDriver();

  driver->SetStopHandler([&driver_stop_indices]() { driver_stop_indices.push_back(1); });

  std::vector<size_t> indices;
  fidl::WireSharedClient<frunner::ComponentController> root_client(
      std::move(root_driver->controller), dispatcher(), TeardownWatcher(0, indices));
  fidl::WireSharedClient<frunner::ComponentController> second_client(
      std::move(controller), dispatcher(), TeardownWatcher(1, indices));

  // Simulate the Component Framework calling Stop on the root driver.
  [[maybe_unused]] auto result = root_client->Stop();

  EXPECT_TRUE(RunLoopUntilIdle());
  // Check that the driver components were shut down in order.
  EXPECT_THAT(indices, ElementsAre(1, 0));
  // Check that Driver::Stop was called in order.
  EXPECT_THAT(driver_stop_indices, ElementsAre(1, 0));
}

// Start the second driver, and then stop the root driver.
TEST_F(DriverRunnerTest, StartSecondDriver_StopRootDriver) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("second", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());
  auto [driver, controller] = StartSecondDriver();

  // Stopping the root driver stops all drivers.
  std::vector<size_t> indices;
  fidl::WireSharedClient<frunner::ComponentController> root_client(
      std::move(root_driver->controller), dispatcher(), TeardownWatcher(0, indices));
  fidl::WireSharedClient<frunner::ComponentController> second_client(
      std::move(controller), dispatcher(), TeardownWatcher(1, indices));
  [[maybe_unused]] auto result = root_client->Stop();
  EXPECT_TRUE(RunLoopUntilIdle());
  EXPECT_THAT(indices, ElementsAre(1, 0));
}

// Start the second driver, stop the root driver, and block while waiting on the
// second driver to shut down.
TEST_F(DriverRunnerTest, StartSecondDriver_BlockOnSecondDriver) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("second", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());
  auto [driver, controller] = StartSecondDriver();

  // When the second driver gets asked to stop, don't drop the binding,
  // which means DriverRunner will wait for the binding to drop.
  driver->SetDontCloseBindingInStop();

  // Stopping the root driver stops all drivers, but is blocked waiting on the
  // second driver to stop.
  std::vector<size_t> indices;
  fidl::WireSharedClient<frunner::ComponentController> root_client(
      std::move(root_driver->controller), dispatcher(), TeardownWatcher(0, indices));
  fidl::WireSharedClient<frunner::ComponentController> second_client(
      std::move(controller), dispatcher(), TeardownWatcher(1, indices));
  [[maybe_unused]] auto result = root_client->Stop();
  EXPECT_TRUE(RunLoopUntilIdle());
  // Nothing has shut down yet, since we are waiting.
  EXPECT_THAT(indices, ElementsAre());

  // Attempt to add a child node to a removed node.
  driver->AddChild("should_fail", false, true);
  EXPECT_TRUE(RunLoopUntilIdle());

  // Unbind the second node, indicating the second driver has stopped, thereby
  // continuing the stop sequence.
  driver->CloseBinding();
  EXPECT_TRUE(RunLoopUntilIdle());
  EXPECT_THAT(indices, ElementsAre(1, 0));
}

TEST_F(DriverRunnerTest, CreateAndBindCompositeNodeSpec) {
  SetupDriverRunner();

  // Add a match for the composite node spec that we are creating.
  std::string name("test-group");

  const fuchsia_driver_framework::CompositeNodeSpec fidl_spec(
      {.name = name,
       .parents = std::vector<fuchsia_driver_framework::ParentSpec>{
           fuchsia_driver_framework::ParentSpec({
               .bind_rules = std::vector<fuchsia_driver_framework::BindRule>(),
               .properties = std::vector<fuchsia_driver_framework::NodeProperty>(),
           }),
           fuchsia_driver_framework::ParentSpec({
               .bind_rules = std::vector<fuchsia_driver_framework::BindRule>(),
               .properties = std::vector<fuchsia_driver_framework::NodeProperty>(),
           })}});

  auto spec = std::make_unique<CompositeNodeSpecV2>(
      CompositeNodeSpecCreateInfo{
          .name = name,
          .size = 2,
      },
      dispatcher(), &driver_runner());
  fidl::Arena<> arena;
  auto added = driver_runner().composite_node_spec_manager().AddSpec(fidl::ToWire(arena, fidl_spec),
                                                                     std::move(spec));
  ASSERT_TRUE(added.is_ok());

  EXPECT_TRUE(RunLoopUntilIdle());

  ASSERT_EQ(2u,
            driver_runner().composite_node_spec_manager().specs().at(name)->parent_specs().size());

  ASSERT_FALSE(
      driver_runner().composite_node_spec_manager().specs().at(name)->parent_specs().at(0));

  ASSERT_FALSE(
      driver_runner().composite_node_spec_manager().specs().at(name)->parent_specs().at(1));

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  std::shared_ptr<CreatedChild> child_0 =
      root_driver->driver->AddChild("dev-group-0", false, false);
  std::shared_ptr<CreatedChild> child_1 =
      root_driver->driver->AddChild("dev-group-1", false, false);

  PrepareRealmForDriverComponentStart("dev.dev-group-1.test-group",
                                      "fuchsia-boot:///#meta/composite-driver.cm");
  EXPECT_TRUE(RunLoopUntilIdle());

  ASSERT_TRUE(driver_runner().composite_node_spec_manager().specs().at(name)->parent_specs().at(0));

  ASSERT_TRUE(driver_runner().composite_node_spec_manager().specs().at(name)->parent_specs().at(1));

  StartDriverHandler start_handler = [](TestDriver* driver, fdfw::DriverStartArgs start_args) {
    ValidateProgram(start_args.program(), "driver/composite-driver.so", "true", "false", "false");
  };
  auto composite_driver = StartDriver(
      {
          .url = "fuchsia-boot:///#meta/composite-driver.cm",
          .binary = "driver/composite-driver.so",
          .colocate = true,
      },
      std::move(start_handler));

  auto hierarchy = Inspect();
  ASSERT_NO_FATAL_FAILURE(CheckNode(hierarchy, {
                                                   .node_name = {"node_topology"},
                                                   .child_names = {"dev"},
                                               }));

  ASSERT_NO_FATAL_FAILURE(CheckNode(hierarchy, {.node_name = {"node_topology", "dev"},
                                                .child_names = {"dev-group-0", "dev-group-1"},
                                                .str_properties = {
                                                    {"driver", root_driver_url},
                                                }}));

  ASSERT_NO_FATAL_FAILURE(CheckNode(
      hierarchy,
      {.node_name = {"node_topology", "dev", "dev-group-0"}, .child_names = {"test-group"}}));

  ASSERT_NO_FATAL_FAILURE(CheckNode(
      hierarchy,
      {.node_name = {"node_topology", "dev", "dev-group-1"}, .child_names = {"test-group"}}));

  ASSERT_NO_FATAL_FAILURE(
      CheckNode(hierarchy, {.node_name = {"node_topology", "dev", "dev-group-0", "test-group"},
                            .str_properties = {
                                {"driver", "fuchsia-boot:///#meta/composite-driver.cm"},
                            }}));

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers"),
                                   CreateChildRef("dev.dev-group-1.test-group", "boot-drivers")});
}

TEST_F(DriverRunnerTest, StartAndInspectLegacyOffers) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  fdfw::NodeAddArgs args({
      .name = "second",
      .offers =
          {
              {

                  fuchsia_component_decl::Offer::WithProtocol(fdecl::OfferProtocol({
                      .source_name = "fuchsia.package.ProtocolA",
                      .target_name = "fuchsia.package.RenamedA",
                  })),

                  fuchsia_component_decl::Offer::WithProtocol(fdecl::OfferProtocol({
                      .source_name = "fuchsia.package.ProtocolB",
                      .target_name = "fuchsia.package.RenamedB",
                  })),
              },
          },
      .symbols =
          {
              {
                  fdfw::NodeSymbol({
                      .name = "symbol-A",
                      .address = 0x2301,
                  }),
                  fdfw::NodeSymbol({
                      .name = "symbol-B",
                      .address = 0x1985,
                  }),
              },
          },
  });
  std::shared_ptr<CreatedChild> child =
      root_driver->driver->AddChild(std::move(args), false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  auto hierarchy = Inspect();
  ASSERT_EQ("root", hierarchy.node().name());
  ASSERT_EQ(2ul, hierarchy.children().size());

  ASSERT_NO_FATAL_FAILURE(CheckNode(hierarchy, {
                                                   .node_name = {"node_topology"},
                                                   .child_names = {"dev"},
                                               }));

  ASSERT_NO_FATAL_FAILURE(CheckNode(hierarchy, {.node_name = {"node_topology", "dev"},
                                                .child_names = {"second"},
                                                .str_properties = {
                                                    {"driver", root_driver_url},
                                                }}));

  ASSERT_NO_FATAL_FAILURE(
      CheckNode(hierarchy, {.node_name = {"node_topology", "dev", "second"},
                            .child_names = {},
                            .str_properties = {
                                {"offers", "fuchsia.package.RenamedA, fuchsia.package.RenamedB"},
                                {"symbols", "symbol-A, symbol-B"},
                                {"driver", "unbound"},
                            }}));

  ASSERT_NO_FATAL_FAILURE(CheckNode(hierarchy, {
                                                   .node_name = {"orphan_nodes"},
                                               }));

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start a driver and inspect the driver runner.
TEST_F(DriverRunnerTest, StartAndInspect) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  PrepareRealmForSecondDriverComponentStart();
  fdfw::NodeAddArgs args({
      .name = "second",
      .symbols =
          {
              {
                  fdfw::NodeSymbol({
                      .name = "symbol-A",
                      .address = 0x2301,
                  }),
                  fdfw::NodeSymbol({
                      .name = "symbol-B",
                      .address = 0x1985,
                  }),
              },
          },
      .offers2 =
          {
              {
                  fuchsia_driver_framework::Offer::WithZirconTransport(
                      fuchsia_component_decl::Offer::WithProtocol(fdecl::OfferProtocol({
                          .source_name = "fuchsia.package.ProtocolA",
                          .target_name = "fuchsia.package.RenamedA",
                      }))),
                  fuchsia_driver_framework::Offer::WithZirconTransport(
                      fuchsia_component_decl::Offer::WithProtocol(fdecl::OfferProtocol({
                          .source_name = "fuchsia.package.ProtocolB",
                          .target_name = "fuchsia.package.RenamedB",
                      }))),
              },
          },
  });
  std::shared_ptr<CreatedChild> child =
      root_driver->driver->AddChild(std::move(args), false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  auto hierarchy = Inspect();
  ASSERT_EQ("root", hierarchy.node().name());
  ASSERT_EQ(2ul, hierarchy.children().size());

  ASSERT_NO_FATAL_FAILURE(CheckNode(hierarchy, {
                                                   .node_name = {"node_topology"},
                                                   .child_names = {"dev"},
                                               }));

  ASSERT_NO_FATAL_FAILURE(CheckNode(hierarchy, {.node_name = {"node_topology", "dev"},
                                                .child_names = {"second"},
                                                .str_properties = {
                                                    {"driver", root_driver_url},
                                                }}));

  ASSERT_NO_FATAL_FAILURE(
      CheckNode(hierarchy, {.node_name = {"node_topology", "dev", "second"},
                            .child_names = {},
                            .str_properties = {
                                {"offers", "fuchsia.package.RenamedA, fuchsia.package.RenamedB"},
                                {"symbols", "symbol-A, symbol-B"},
                                {"driver", "unbound"},
                            }}));

  ASSERT_NO_FATAL_FAILURE(CheckNode(hierarchy, {
                                                   .node_name = {"orphan_nodes"},
                                               }));

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

TEST_F(DriverRunnerTest, TestTearDownNodeTreeWithManyChildren) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  std::vector<std::shared_ptr<CreatedChild>> children;
  for (size_t i = 0; i < 100; i++) {
    children.emplace_back(root_driver->driver->AddChild("child" + std::to_string(i), false, false));
    EXPECT_TRUE(RunLoopUntilIdle());
  }

  Unbind();
}

TEST_F(DriverRunnerTest, TestBindResultTracker) {
  bool callback_called = false;
  bool* callback_called_ptr = &callback_called;

  auto callback = [callback_called_ptr](
                      fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> results) {
    ASSERT_EQ(std::string_view("node_name"), results[0].node_name().get());
    ASSERT_EQ(std::string_view("driver_url"), results[0].driver_url().get());
    ASSERT_EQ(1ul, results.count());
    *callback_called_ptr = true;
  };

  BindResultTracker tracker(3, std::move(callback));
  ASSERT_EQ(false, callback_called);
  tracker.ReportNoBind();
  ASSERT_EQ(false, callback_called);
  tracker.ReportSuccessfulBind(std::string_view("node_name"), "driver_url");
  ASSERT_EQ(false, callback_called);
  tracker.ReportNoBind();
  ASSERT_EQ(true, callback_called);

  callback_called = false;
  auto callback_two =
      [callback_called_ptr](
          fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> results) {
        ASSERT_EQ(0ul, results.count());
        *callback_called_ptr = true;
      };

  BindResultTracker tracker_two(3, std::move(callback_two));
  ASSERT_EQ(false, callback_called);
  tracker_two.ReportNoBind();
  ASSERT_EQ(false, callback_called);
  tracker_two.ReportNoBind();
  ASSERT_EQ(false, callback_called);
  tracker_two.ReportNoBind();
  ASSERT_EQ(true, callback_called);

  callback_called = false;
  auto callback_three =
      [callback_called_ptr](
          fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> results) {
        ASSERT_EQ(std::string_view("node_name"), results[0].node_name().get());
        ASSERT_EQ(std::string_view("test_spec"),
                  results[0].composite_parents()[0].composite().spec().name().get());
        ASSERT_EQ(std::string_view("test_spec_2"),
                  results[0].composite_parents()[1].composite().spec().name().get());
        ASSERT_EQ(1ul, results.count());
        *callback_called_ptr = true;
      };

  BindResultTracker tracker_three(3, std::move(callback_three));
  ASSERT_EQ(false, callback_called);
  tracker_three.ReportNoBind();
  ASSERT_EQ(false, callback_called);
  tracker_three.ReportNoBind();
  ASSERT_EQ(false, callback_called);

  {
    tracker_three.ReportSuccessfulBind(std::string_view("node_name"), {},
                                       std::vector{
                                           fdfw::CompositeParent{{
                                               .composite = fdfw::CompositeInfo{{
                                                   .spec = fdfw::CompositeNodeSpec{{
                                                       .name = "test_spec",
                                                   }},
                                               }},
                                           }},
                                           fdfw::CompositeParent{{
                                               .composite = fdfw::CompositeInfo{{
                                                   .spec = fdfw::CompositeNodeSpec{{
                                                       .name = "test_spec_2",
                                                   }},
                                               }},
                                           }},
                                       });
  }

  ASSERT_EQ(true, callback_called);
}

// Start the root driver, add a child node, and verify that the child node's device controller is
// reachable.
TEST_F(DriverRunnerTest, ConnectToDeviceController) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  const char* kChildName = "node-1";
  std::shared_ptr<CreatedChild> created_child =
      root_driver->driver->AddChild(kChildName, true, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  auto device_controller = ConnectToDeviceController(kChildName);

  // Call one of the device controller's method in order to verify that the controller works.
  device_controller->GetTopologicalPath().Then(
      [](fidl::WireUnownedResult<fuchsia_device::Controller::GetTopologicalPath>& reply) {
        ASSERT_EQ(reply.status(), ZX_OK);
        ASSERT_TRUE(reply->is_ok());
        ASSERT_EQ(reply.value()->path.get(), "dev/node-1");
      });
  EXPECT_TRUE(RunLoopUntilIdle());
}

// Start the root driver, add a child node, and verify that calling the child's device controller's
// `ConnectToController` FIDL method works.
TEST_F(DriverRunnerTest, ConnectToControllerFidlMethod) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  const char* kChildName = "node-1";
  std::shared_ptr<CreatedChild> created_child =
      root_driver->driver->AddChild(kChildName, true, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  auto device_controller_1 = ConnectToDeviceController(kChildName);

  zx::result controller_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
  ASSERT_EQ(controller_endpoints.status_value(), ZX_OK);
  fidl::OneWayStatus result =
      device_controller_1->ConnectToController(std::move(controller_endpoints->server));
  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_EQ(result.status(), ZX_OK);

  fidl::WireClient<fuchsia_device::Controller> device_controller_2{
      std::move(controller_endpoints->client), dispatcher()};

  // Verify that the two device controllers connect to the same device server.
  // This is done by verifying the topological paths returned by the device controllers are the
  // same.
  std::string topological_path_1;
  device_controller_1->GetTopologicalPath().Then(
      [&](fidl::WireUnownedResult<fuchsia_device::Controller::GetTopologicalPath>& reply) {
        ASSERT_EQ(reply.status(), ZX_OK);
        ASSERT_TRUE(reply->is_ok());
        topological_path_1 = reply.value()->path.get();
      });

  std::string topological_path_2;
  device_controller_2->GetTopologicalPath().Then(
      [&](fidl::WireUnownedResult<fuchsia_device::Controller::GetTopologicalPath>& reply) {
        ASSERT_EQ(reply.status(), ZX_OK);
        ASSERT_TRUE(reply->is_ok());
        topological_path_2 = reply.value()->path.get();
      });

  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_EQ(topological_path_1, topological_path_2);
}

// Verify that device controller's Bind FIDL method works.
TEST_F(DriverRunnerTest, DeviceControllerBind) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("child", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  ASSERT_EQ(1u, driver_runner().bind_manager().NumOrphanedNodes());

  driver_index().set_match_callback([](auto args) -> zx::result<FakeDriverIndex::MatchResult> {
    EXPECT_EQ(args.driver_url_suffix().get(), second_driver_url);
    return zx::ok(FakeDriverIndex::MatchResult{
        .url = second_driver_url,
    });
  });

  PrepareRealmForDriverComponentStart("dev.child", second_driver_url);
  AssertNodeControllerBound(child);

  // Bind the driver.
  ASSERT_EQ(1u, driver_runner().bind_manager().NumOrphanedNodes());
  auto device_controller = ConnectToDeviceController("child");
  device_controller->Bind(fidl::StringView::FromExternal(second_driver_url))
      .Then([](fidl::WireUnownedResult<fuchsia_device::Controller::Bind>& reply) {
        ASSERT_EQ(reply.status(), ZX_OK);
      });
  ASSERT_TRUE(RunLoopUntilIdle());

  // Verify the driver was bound.
  ASSERT_EQ(0u, driver_runner().bind_manager().NumOrphanedNodes());
  auto [driver, controller] = StartSecondDriver();
  driver->CloseBinding();
  driver->DropNode();
  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren(
      {CreateChildRef("dev", "boot-drivers"), CreateChildRef("dev.child", "boot-drivers")});
}

TEST(CompositeServiceOfferTest, WorkingOffer) {
  const std::string_view kServiceName = "fuchsia.service";
  fidl::Arena<> arena;
  auto service = fdecl::wire::OfferService::Builder(arena);
  service.source_name(arena, kServiceName);
  service.target_name(arena, kServiceName);

  fidl::VectorView<fdecl::wire::NameMapping> mappings(arena, 2);
  mappings[0].source_name = fidl::StringView(arena, "instance-1");
  mappings[0].target_name = fidl::StringView(arena, "default");

  mappings[1].source_name = fidl::StringView(arena, "instance-1");
  mappings[1].target_name = fidl::StringView(arena, "instance-2");
  service.renamed_instances(mappings);

  fidl::VectorView<fidl::StringView> filters(arena, 2);
  filters[0] = fidl::StringView(arena, "default");
  filters[1] = fidl::StringView(arena, "instance-2");
  service.source_instance_filter(filters);

  auto offer = fdecl::wire::Offer::WithService(arena, service.Build());
  auto new_offer = CreateCompositeServiceOffer(arena, offer, "parent_node", false);
  ASSERT_TRUE(new_offer);

  ASSERT_EQ(2ul, new_offer->service().renamed_instances().count());
  // Check that the default instance got renamed.
  ASSERT_EQ(std::string("instance-1"),
            std::string(new_offer->service().renamed_instances()[0].source_name.get()));
  ASSERT_EQ(std::string("parent_node"),
            std::string(new_offer->service().renamed_instances()[0].target_name.get()));

  // Check that a non-default instance stayed the same.
  ASSERT_EQ(std::string("instance-1"),
            std::string(new_offer->service().renamed_instances()[1].source_name.get()));
  ASSERT_EQ(std::string("instance-2"),
            std::string(new_offer->service().renamed_instances()[1].target_name.get()));

  ASSERT_EQ(2ul, new_offer->service().source_instance_filter().count());
  // Check that the default filter got renamed.
  ASSERT_EQ(std::string("parent_node"),
            std::string(new_offer->service().source_instance_filter()[0].get()));

  // Check that a non-default filter stayed the same.
  ASSERT_EQ(std::string("instance-2"),
            std::string(new_offer->service().source_instance_filter()[1].get()));
}

TEST(CompositeServiceOfferTest, WorkingOfferPrimary) {
  const std::string_view kServiceName = "fuchsia.service";
  fidl::Arena<> arena;
  auto service = fdecl::wire::OfferService::Builder(arena);
  service.source_name(arena, kServiceName);
  service.target_name(arena, kServiceName);

  fidl::VectorView<fdecl::wire::NameMapping> mappings(arena, 2);
  mappings[0].source_name = fidl::StringView(arena, "instance-1");
  mappings[0].target_name = fidl::StringView(arena, "default");

  mappings[1].source_name = fidl::StringView(arena, "instance-1");
  mappings[1].target_name = fidl::StringView(arena, "instance-2");
  service.renamed_instances(mappings);

  fidl::VectorView<fidl::StringView> filters(arena, 2);
  filters[0] = fidl::StringView(arena, "default");
  filters[1] = fidl::StringView(arena, "instance-2");
  service.source_instance_filter(filters);

  auto offer = fdecl::wire::Offer::WithService(arena, service.Build());
  auto new_offer = CreateCompositeServiceOffer(arena, offer, "parent_node", true);
  ASSERT_TRUE(new_offer);

  ASSERT_EQ(3ul, new_offer->service().renamed_instances().count());
  // Check that the default instance stayed the same (because we're primary).
  ASSERT_EQ(std::string("instance-1"),
            std::string(new_offer->service().renamed_instances()[0].source_name.get()));
  ASSERT_EQ(std::string("default"),
            std::string(new_offer->service().renamed_instances()[0].target_name.get()));

  // Check that the default instance got renamed.
  ASSERT_EQ(std::string("instance-1"),
            std::string(new_offer->service().renamed_instances()[1].source_name.get()));
  ASSERT_EQ(std::string("parent_node"),
            std::string(new_offer->service().renamed_instances()[1].target_name.get()));

  // Check that a non-default instance stayed the same.
  ASSERT_EQ(std::string("instance-1"),
            std::string(new_offer->service().renamed_instances()[2].source_name.get()));
  ASSERT_EQ(std::string("instance-2"),
            std::string(new_offer->service().renamed_instances()[2].target_name.get()));

  ASSERT_EQ(3ul, new_offer->service().source_instance_filter().count());
  // Check that the default filter stayed the same (because we're primary).
  EXPECT_EQ(std::string("default"),
            std::string(new_offer->service().source_instance_filter()[0].get()));

  // Check that the default filter got renamed.
  EXPECT_EQ(std::string("parent_node"),
            std::string(new_offer->service().source_instance_filter()[1].get()));

  // Check that a non-default filter stayed the same.
  EXPECT_EQ(std::string("instance-2"),
            std::string(new_offer->service().source_instance_filter()[2].get()));
}

TEST(NodeTest, ToCollection) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
  InspectManager inspect(loop.dispatcher());
  constexpr uint32_t kProtocolId = 0;

  constexpr char kGrandparentName[] = "grandparent";
  std::shared_ptr<Node> grandparent = std::make_shared<Node>(
      kGrandparentName, std::vector<std::weak_ptr<Node>>{}, nullptr, loop.dispatcher(),
      inspect.CreateDevice(kGrandparentName, zx::vmo{}, kProtocolId));

  constexpr char kParentName[] = "parent";
  std::shared_ptr<Node> parent = std::make_shared<Node>(
      kParentName, std::vector<std::weak_ptr<Node>>{grandparent}, nullptr, loop.dispatcher(),
      inspect.CreateDevice(kParentName, zx::vmo{}, kProtocolId));

  constexpr char kChild1Name[] = "child1";
  std::shared_ptr<Node> child1 = std::make_shared<Node>(
      kChild1Name, std::vector<std::weak_ptr<Node>>{parent}, nullptr, loop.dispatcher(),
      inspect.CreateDevice(kChild1Name, zx::vmo{}, kProtocolId));

  constexpr char kChild2Name[] = "child2";
  std::shared_ptr<Node> child2 = std::make_shared<Node>(
      kChild2Name, std::vector<std::weak_ptr<Node>>{parent, child1}, nullptr, loop.dispatcher(),
      inspect.CreateDevice(kChild2Name, zx::vmo{}, kProtocolId), 0, NodeType::kComposite);

  // Test parentless
  EXPECT_EQ(ToCollection(*grandparent, fdfw::DriverPackageType::kBoot), Collection::kBoot);
  EXPECT_EQ(ToCollection(*grandparent, fdfw::DriverPackageType::kBase), Collection::kPackage);
  EXPECT_EQ(ToCollection(*grandparent, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*grandparent, fdfw::DriverPackageType::kUniverse),
            Collection::kFullPackage);

  // // Test single parent with grandparent collection set to none
  grandparent->set_collection(Collection::kNone);
  parent->set_collection(Collection::kNone);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBoot), Collection::kBoot);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBase), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  grandparent->set_collection(Collection::kNone);
  parent->set_collection(Collection::kBoot);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBoot), Collection::kBoot);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBase), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  grandparent->set_collection(Collection::kNone);
  parent->set_collection(Collection::kPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBoot), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBase), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  grandparent->set_collection(Collection::kNone);
  parent->set_collection(Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBoot), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBase), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  // Test single parent with parent collection set to none
  grandparent->set_collection(Collection::kBoot);
  parent->set_collection(Collection::kNone);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBoot), Collection::kBoot);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBase), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  grandparent->set_collection(Collection::kPackage);
  parent->set_collection(Collection::kNone);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBoot), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBase), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  grandparent->set_collection(Collection::kFullPackage);
  parent->set_collection(Collection::kNone);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBoot), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kBase), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child1, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  // Test multi parent
  grandparent->set_collection(Collection::kNone);
  parent->set_collection(Collection::kNone);
  child1->set_collection(Collection::kNone);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBoot), Collection::kBoot);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBase), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  grandparent->set_collection(Collection::kNone);
  parent->set_collection(Collection::kBoot);
  child1->set_collection(Collection::kNone);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBoot), Collection::kBoot);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBase), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  grandparent->set_collection(Collection::kNone);
  parent->set_collection(Collection::kNone);
  child1->set_collection(Collection::kPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBoot), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBase), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  grandparent->set_collection(Collection::kFullPackage);
  parent->set_collection(Collection::kFullPackage);
  child1->set_collection(Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBoot), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBase), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  // Test multi parent with one parent collection set to none
  grandparent->set_collection(Collection::kBoot);
  parent->set_collection(Collection::kNone);
  child1->set_collection(Collection::kBoot);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBoot), Collection::kBoot);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBase), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  grandparent->set_collection(Collection::kPackage);
  parent->set_collection(Collection::kNone);
  child1->set_collection(Collection::kBoot);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBoot), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBase), Collection::kPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);

  grandparent->set_collection(Collection::kFullPackage);
  parent->set_collection(Collection::kNone);
  child1->set_collection(Collection::kBoot);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBoot), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kBase), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kCached), Collection::kFullPackage);
  EXPECT_EQ(ToCollection(*child2, fdfw::DriverPackageType::kUniverse), Collection::kFullPackage);
}
