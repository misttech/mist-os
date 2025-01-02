// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/tests/driver_runner_test_fixture.h"

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

#include "src/devices/bin/driver_manager/testing/fake_driver_index.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"

namespace driver_runner {

namespace fdata = fuchsia_data;
namespace fdfw = fuchsia_driver_framework;
namespace fdh = fuchsia_driver_host;
namespace fio = fuchsia_io;
namespace fprocess = fuchsia_process;
namespace frunner = fuchsia_component_runner;
namespace fcomponent = fuchsia_component;
namespace fdecl = fuchsia_component_decl;

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

zx::result<fidl::ClientEnd<fuchsia_driver_loader::DriverHostLauncher>> DynamicLinkerFactory(
    driver_loader::Loader* loader) {
  auto [client_end, server_end] =
      fidl::Endpoints<fuchsia_driver_loader::DriverHostLauncher>::Create();
  loader->Connect(std::move(server_end));
  return zx::ok(std::move(client_end));
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

void TestRealm::AssertDestroyedChildren(const std::vector<fdecl::ChildRef>& expected) {
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
void TestRealm::CreateChild(CreateChildRequest& request, CreateChildCompleter::Sync& completer) {
  handles_ = std::move(request.args().numbered_handles());
  auto offers = request.args().dynamic_offers();
  create_child_handler_(
      std::move(request.collection()), std::move(request.decl()),
      offers.has_value() ? std::move(offers.value()) : std::vector<fdecl::Offer>{});
  completer.Reply(fidl::Response<fuchsia_component::Realm::CreateChild>(fit::ok()));
}
void TestRealm::DestroyChild(DestroyChildRequest& request, DestroyChildCompleter::Sync& completer) {
  destroyed_children_.push_back(std::move(request.child()));
  completer.Reply(fidl::Response<fuchsia_component::Realm::DestroyChild>(fit::ok()));
}
void TestRealm::OpenExposedDir(OpenExposedDirRequest& request,
                               OpenExposedDirCompleter::Sync& completer) {
  open_exposed_dir_handler_(std::move(request.child()), std::move(request.exposed_dir()));
  completer.Reply(fidl::Response<fuchsia_component::Realm::OpenExposedDir>(fit::ok()));
}
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

void DriverHostComponentStart(driver_runner::TestRealm& realm,
                              driver_manager::DriverHostRunner& driver_host_runner,
                              fidl::ClientEnd<fuchsia_io::Directory> driver_host_pkg) {
  fidl::Arena arena;

  fidl::VectorView<fdata::wire::DictionaryEntry> program_entries(arena, 1);
  program_entries[0].key.Set(arena, "binary");
  program_entries[0].value = fdata::wire::DictionaryValue::WithStr(arena, "bin/driver_host2");
  auto program_builder = fdata::wire::Dictionary::Builder(arena);
  program_builder.entries(program_entries);

  fidl::VectorView<frunner::wire::ComponentNamespaceEntry> ns_entries(arena, 1);
  ns_entries[0] = frunner::wire::ComponentNamespaceEntry::Builder(arena)
                      .path("/pkg")
                      .directory(std::move(driver_host_pkg))
                      .Build();

  auto start_info_builder = frunner::wire::ComponentStartInfo::Builder(arena);
  start_info_builder.resolved_url("fuchsia-boot:///driver_host2#meta/driver_host2.cm")
      .program(program_builder.Build())
      .ns(ns_entries)
      .numbered_handles(realm.TakeHandles(arena));

  auto controller_endpoints = fidl::Endpoints<frunner::ComponentController>::Create();
  TestTransaction transaction(false);
  {
    fidl::WireServer<frunner::ComponentRunner>::StartCompleter::Sync completer(&transaction);
    fidl::WireRequest<frunner::ComponentRunner::Start> request{
        start_info_builder.Build(), std::move(controller_endpoints.server)};
    static_cast<fidl::WireServer<frunner::ComponentRunner>&>(driver_host_runner)
        .Start(&request, completer);
  }
}

fidl::ClientEnd<fuchsia_component::Realm> DriverRunnerTest::ConnectToRealm() {
  auto realm_endpoints = fidl::Endpoints<fcomponent::Realm>::Create();
  realm_bindings_.AddBinding(dispatcher(), std::move(realm_endpoints.server), &realm_,
                             fidl::kIgnoreBindingClosure);
  return std::move(realm_endpoints.client);
}
FakeDriverIndex DriverRunnerTest::CreateDriverIndex() {
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
void DriverRunnerTest::SetupDriverRunner(FakeDriverIndex driver_index) {
  driver_index_.emplace(std::move(driver_index));
  driver_runner_.emplace(ConnectToRealm(), driver_index_->Connect(), inspect(), &LoaderFactory,
                         dispatcher(), false);
  SetupDevfs();
}

void DriverRunnerTest::SetupDriverRunnerWithDynamicLinker(
    async_dispatcher_t* loader_dispatcher,
    std::unique_ptr<driver_manager::DriverHostRunner> driver_host_runner) {
  driver_index_.emplace(CreateDriverIndex());
  auto load_driver_handler = [](zx::unowned_channel bootstrap_sender,
                                driver_loader::Loader::DriverStartAddr addr) {
    ASSERT_EQ(ZX_OK, bootstrap_sender->write(0, &addr, sizeof(addr), nullptr, 0));
  };
  dynamic_linker_ =
      driver_loader::Loader::Create(loader_dispatcher, std::move(load_driver_handler));
  driver_runner_.emplace(
      ConnectToRealm(), driver_index_->Connect(), inspect(), &LoaderFactory, dispatcher(), false,
      driver_manager::DriverRunner::DynamicLinkerArgs{
          [loader = dynamic_linker_.get()]() { return DynamicLinkerFactory(loader); },
          std::move(driver_host_runner)});
  SetupDevfs();
}

void DriverRunnerTest::SetupDriverRunner() { SetupDriverRunner(CreateDriverIndex()); }
void DriverRunnerTest::PrepareRealmForDriverComponentStart(const std::string& name,
                                                           const std::string& url) {
  realm().SetCreateChildHandler(
      [name, url](fdecl::CollectionRef collection, fdecl::Child decl, auto offers) {
        EXPECT_EQ("boot-drivers", collection.name());
        EXPECT_EQ(name, decl.name().value());
        EXPECT_EQ(url, decl.url().value());
      });
}
void DriverRunnerTest::PrepareRealmForSecondDriverComponentStart() {
  PrepareRealmForDriverComponentStart("dev.second", second_driver_url);
}
void DriverRunnerTest::PrepareRealmForStartDriverHost(bool use_next_vdso) {
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
    driver_host_bindings_.AddBinding(dispatcher(),
                                     fidl::ServerEnd<fdh::DriverHost>(object.TakeChannel()),
                                     &driver_host_, fidl::kIgnoreBindingClosure);
  });
}

void DriverRunnerTest::PrepareRealmForStartDriverHostDynamicLinker() {
  constexpr std::string_view kCollection = "driver-hosts";
  constexpr std::string_view kDriverHostName = "driver-host-new-";
  constexpr std::string_view kComponentUrl = "fuchsia-boot:///driver_host2#meta/driver_host2.cm";

  realm().SetCreateChildHandler(
      [kCollection, kDriverHostName, kComponentUrl](fdecl::CollectionRef collection,
                                                    fdecl::Child decl, auto offers) {
        EXPECT_EQ(kCollection, collection.name());
        EXPECT_EQ(kDriverHostName, decl.name().value().substr(0, kDriverHostName.size()));
        EXPECT_EQ(kComponentUrl, decl.url());
      });
  realm().SetOpenExposedDirHandler(
      [this, kCollection, kDriverHostName](fdecl::ChildRef child, auto exposed_dir) {
        EXPECT_EQ(kCollection, child.collection().value_or(""));
        EXPECT_EQ(kDriverHostName, child.name().substr(0, kDriverHostName.size()));
        driver_host_dir_.Bind(std::move(exposed_dir));
      });
  driver_host_dir_.SetOpenHandler([this](const std::string& path, auto object) {
    EXPECT_EQ(fidl::DiscoverableProtocolName<fdh::DriverHost>, path);
    driver_host_bindings_.AddBinding(dispatcher(),
                                     fidl::ServerEnd<fdh::DriverHost>(object.TakeChannel()),
                                     &driver_host_, fidl::kIgnoreBindingClosure);
  });
}

void DriverRunnerTest::StopDriverComponent(
    fidl::ClientEnd<frunner::ComponentController> component) {
  fidl::WireClient client(std::move(component), dispatcher());
  auto stop_result = client->Stop();
  ASSERT_EQ(ZX_OK, stop_result.status());
  EXPECT_TRUE(RunLoopUntilIdle());
}
DriverRunnerTest::StartDriverResult DriverRunnerTest::StartDriver(
    Driver driver, std::optional<StartDriverHandler> start_handler,
    fidl::ClientEnd<fuchsia_io::Directory> ns_pkg,
    fidl::ClientEnd<fuchsia_io::Directory> driver_host_pkg) {
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
    if (driver.use_dynamic_linker) {
      PrepareRealmForStartDriverHostDynamicLinker();
    } else {
      PrepareRealmForStartDriverHost(driver.use_next_vdso);
    }
  }

  fidl::Arena arena;

  fidl::VectorView<fdata::wire::DictionaryEntry> program_entries(arena, 5);
  program_entries[0].key.Set(arena, "binary");
  program_entries[0].value = fdata::wire::DictionaryValue::WithStr(arena, driver.binary);

  program_entries[1].key.Set(arena, "colocate");
  program_entries[1].value =
      fdata::wire::DictionaryValue::WithStr(arena, driver.colocate ? "true" : "false");

  program_entries[2].key.Set(arena, "host_restart_on_crash");
  program_entries[2].value =
      fdata::wire::DictionaryValue::WithStr(arena, driver.host_restart_on_crash ? "true" : "false");

  program_entries[3].key.Set(arena, "use_next_vdso");
  program_entries[3].value =
      fdata::wire::DictionaryValue::WithStr(arena, driver.use_next_vdso ? "true" : "false");

  program_entries[4].key.Set(arena, "use_dynamic_linker");
  program_entries[4].value =
      fdata::wire::DictionaryValue::WithStr(arena, driver.use_dynamic_linker ? "true" : "false");

  auto program_builder = fdata::wire::Dictionary::Builder(arena);
  program_builder.entries(program_entries);

  auto outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  EXPECT_EQ(ZX_OK, outgoing_endpoints.status_value());

  auto start_info_builder = frunner::wire::ComponentStartInfo::Builder(arena);

  fidl::VectorView<frunner::wire::ComponentNamespaceEntry> ns_entries = {};
  if (ns_pkg.is_valid()) {
    ns_entries.Allocate(arena, 1);
    ns_entries[0] = frunner::wire::ComponentNamespaceEntry::Builder(arena)
                        .path("/pkg")
                        .directory(std::move(ns_pkg))
                        .Build();
  }

  start_info_builder.resolved_url(driver.url)
      .program(program_builder.Build())
      .outgoing_dir(std::move(outgoing_endpoints->server))
      .ns(ns_entries)
      .numbered_handles(realm().TakeHandles(arena));

  auto controller_endpoints = fidl::Endpoints<frunner::ComponentController>::Create();
  TestTransaction transaction(driver.close);
  {
    fidl::WireServer<frunner::ComponentRunner>::StartCompleter::Sync completer(&transaction);
    fidl::WireRequest<frunner::ComponentRunner::Start> request{
        start_info_builder.Build(), std::move(controller_endpoints.server)};
    static_cast<fidl::WireServer<frunner::ComponentRunner>&>(driver_runner().runner_for_tests())
        .Start(&request, completer);
  }
  RunLoopUntilIdle();

  // The driver manager is waiting for the component framework to call the driver
  // host runner's component Start implementation. We need to call it
  // now to continue with starting the driver host and subsequently the driver.
  if (!driver.colocate && driver.use_dynamic_linker) {
    DriverHostComponentStart(realm(), *driver_runner().driver_host_runner_for_tests(),
                             std::move(driver_host_pkg));
    RunLoopUntilIdle();
  }

  return {std::move(started_driver), std::move(controller_endpoints.client)};
}
zx::result<DriverRunnerTest::StartDriverResult> DriverRunnerTest::StartRootDriver() {
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

zx::result<DriverRunnerTest::StartDriverResult> DriverRunnerTest::StartRootDriverDynamicLinking(
    test_utils::TestPkg::Config driver_host_config, test_utils::TestPkg::Config driver_config) {
  PrepareRealmForDriverComponentStart("dev", driver_runner::root_driver_url);

  auto start = driver_runner().StartRootDriver(driver_runner::root_driver_url);
  if (start.is_error()) {
    return start.take_error();
  }
  EXPECT_TRUE(RunLoopUntilIdle());

  auto pkg_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
  test_utils::TestPkg test_pkg(std::move(pkg_endpoints.server), driver_config);
  StartDriverHandler start_handler = [pkg_path = driver_config.module_open_path](
                                         driver_runner::TestDriver* driver,
                                         fdfw::DriverStartArgs start_args) {
    ValidateProgram(start_args.program(), pkg_path, "false" /* colocate */,
                    "false" /* host_restart_on_crash */, "false" /* use_next_vdso */,
                    "true" /* use_dynamic_linker */);
  };

  auto driver_host_pkg_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
  test_utils::TestPkg driver_host_test_pkg(std::move(driver_host_pkg_endpoints.server),
                                           driver_host_config);

  return zx::ok(StartDriver(
      {
          .url = driver_runner::root_driver_url,
          .binary = std::string(driver_config.module_open_path),
          .use_dynamic_linker = true,
      },
      std::move(start_handler), std::move(pkg_endpoints.client),
      std::move(driver_host_pkg_endpoints.client)));
}

void DriverRunnerTest::Unbind() {
  driver_host_bindings_.CloseAll(ZX_OK);
  EXPECT_TRUE(RunLoopUntilIdle());
}
void DriverRunnerTest::ValidateProgram(std::optional<::fuchsia_data::Dictionary>& program,
                                       std::string_view binary, std::string_view colocate,
                                       std::string_view host_restart_on_crash,
                                       std::string_view use_next_vdso,
                                       std::string_view use_dynamic_linker) {
  ZX_ASSERT(program.has_value());
  auto& entries_opt = program.value().entries();
  ZX_ASSERT(entries_opt.has_value());
  auto& entries = entries_opt.value();
  EXPECT_EQ(5u, entries.size());
  EXPECT_EQ("binary", entries[0].key());
  EXPECT_EQ(std::string(binary), entries[0].value()->str().value());
  EXPECT_EQ("colocate", entries[1].key());
  EXPECT_EQ(std::string(colocate), entries[1].value()->str().value());
  EXPECT_EQ("host_restart_on_crash", entries[2].key());
  EXPECT_EQ(std::string(host_restart_on_crash), entries[2].value()->str().value());
  EXPECT_EQ("use_next_vdso", entries[3].key());
  EXPECT_EQ(std::string(use_next_vdso), entries[3].value()->str().value());
  EXPECT_EQ("use_dynamic_linker", entries[4].key());
  EXPECT_EQ(std::string(use_dynamic_linker), entries[4].value()->str().value());
}
void DriverRunnerTest::AssertNodeBound(const std::shared_ptr<CreatedChild>& child) {
  auto& node = child->node;
  ASSERT_TRUE(node.has_value() && node.value().is_valid());
}
void DriverRunnerTest::AssertNodeNotBound(const std::shared_ptr<CreatedChild>& child) {
  auto& node = child->node;
  ASSERT_FALSE(node.has_value() && node.value().is_valid());
}
void DriverRunnerTest::AssertNodeControllerBound(const std::shared_ptr<CreatedChild>& child) {
  auto& controller = child->node_controller;
  ASSERT_TRUE(controller.has_value() && controller.value().is_valid());
}
void DriverRunnerTest::AssertNodeControllerNotBound(const std::shared_ptr<CreatedChild>& child) {
  auto& controller = child->node_controller;
  ASSERT_FALSE(controller.has_value() && controller.value().is_valid());
}
inspect::Hierarchy DriverRunnerTest::Inspect() {
  FakeContext context;
  auto inspector = driver_runner().Inspect()(context).take_value();
  return inspect::ReadFromInspector(inspector)(context).take_value();
}
void DriverRunnerTest::SetupDevfs() { driver_runner().root_node()->SetupDevfsForRootNode(devfs_); }
DriverRunnerTest::StartDriverResult DriverRunnerTest::StartSecondDriver(bool colocate,
                                                                        bool host_restart_on_crash,
                                                                        bool use_next_vdso) {
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
void TestDirectory::Bind(fidl::ServerEnd<fio::Directory> request) {
  bindings_.AddBinding(dispatcher_, std::move(request), this, fidl::kIgnoreBindingClosure);
}
void TestDirectory::Open(OpenRequest& request, OpenCompleter::Sync& completer) {
  open_handler_(request.path(), std::move(request.object()));
}
void TestDirectory::Open3(Open3Request& request, Open3Completer::Sync& completer) {
  open_handler_(request.path(), fidl::ServerEnd<fio::Node>(std::move(request.object())));
}
void TestDirectory::handle_unknown_method(fidl::UnknownMethodMetadata<fio::Directory>,
                                          fidl::UnknownMethodCompleter::Sync&) {}
void TestDriver::Stop(StopCompleter::Sync& completer) {
  stop_handler_();
  if (!dont_close_binding_in_stop_) {
    driver_binding_.Close(ZX_OK);
  }
}
std::shared_ptr<CreatedChild> TestDriver::AddChild(std::string_view child_name, bool owned,
                                                   bool expect_error,
                                                   const std::string& class_name) {
  fidl::Arena arena;
  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector_supports(fuchsia_device_fs::ConnectionType::kController)
                   .class_name(class_name)
                   .Build();
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, child_name)
                  .devfs_args(devfs)
                  .Build();
  return AddChild(fidl::ToNatural(args), owned, expect_error);
}
std::shared_ptr<CreatedChild> TestDriver::AddChild(fdfw::NodeAddArgs child_args, bool owned,
                                                   bool expect_error,
                                                   fit::function<void()> on_bind) {
  auto controller_endpoints = fidl::Endpoints<fdfw::NodeController>::Create();

  auto child_node_endpoints = fidl::CreateEndpoints<fdfw::Node>();
  ZX_ASSERT(ZX_OK == child_node_endpoints.status_value());

  fidl::ServerEnd<fdfw::Node> child_node_server = {};
  if (owned) {
    child_node_server = std::move(child_node_endpoints->server);
  }

  node_
      ->AddChild({std::move(child_args), std::move(controller_endpoints.server),
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
    void handle_unknown_event(fidl::UnknownEventMetadata<fdfw::NodeController> metadata) override {}

   private:
    std::shared_ptr<CreatedChild> child_;
    fit::function<void()> on_bind_;
  };

  std::shared_ptr<CreatedChild> child = std::make_shared<CreatedChild>();
  child->node_controller.emplace(std::move(controller_endpoints.client), dispatcher_,
                                 new ControllerEventHandler(child, std::move(on_bind)));
  if (owned) {
    child->node.emplace(std::move(child_node_endpoints->client), dispatcher_,
                        new NodeEventHandler(child));
  }

  return child;
}
fidl::VectorView<fprocess::wire::HandleInfo> TestRealm::TakeHandles(fidl::AnyArena& arena) {
  if (handles_.has_value()) {
    return fidl::ToWire(arena, std::move(handles_));
  }

  return fidl::VectorView<fprocess::wire::HandleInfo>(arena, 0);
}
fidl::WireClient<fuchsia_device::Controller> DriverRunnerTest::ConnectToDeviceController(
    std::string_view child_name) {
  fs::SynchronousVfs vfs(dispatcher());
  zx::result dev_res = devfs().Connect(vfs);
  EXPECT_EQ(dev_res.status_value(), ZX_OK);
  fidl::WireClient<fuchsia_io::Directory> dev{std::move(*dev_res), dispatcher()};
  auto controller_endpoints = fidl::Endpoints<fuchsia_device::Controller>::Create();

  auto device_controller_path = std::string(child_name) + "/device_controller";
  EXPECT_EQ(dev->Open(fuchsia_io::OpenFlags::kNotDirectory, {},
                      fidl::StringView::FromExternal(device_controller_path),
                      fidl::ServerEnd<fuchsia_io::Node>(controller_endpoints.server.TakeChannel()))
                .status(),
            ZX_OK);
  EXPECT_TRUE(RunLoopUntilIdle());

  return fidl::WireClient<fuchsia_device::Controller>{std::move(controller_endpoints.client),
                                                      dispatcher()};
}

}  // namespace driver_runner
