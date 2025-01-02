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
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fit/defer.h>
#include <lib/inspect/cpp/reader.h>
#include <lib/inspect/testing/cpp/inspect.h>

#include <bind/fuchsia/platform/cpp/bind.h>

#include "gmock/gmock.h"
#include "src/devices/bin/driver_manager/composite_node_spec_impl.h"
#include "src/devices/bin/driver_manager/testing/fake_driver_index.h"
#include "src/devices/bin/driver_manager/tests/driver_runner_test_fixture.h"

namespace driver_runner {

namespace frunner = fuchsia_component_runner;

using driver_manager::Collection;
using driver_manager::Node;
using testing::ElementsAre;

// TODO(https://fxbug.dev/363012744): this fixture enables each test case being run twice, once with
// the dynamic linker and once not (the legacy path). Once all test cases support the dynamic
// linker path, we can merge this with the |DriverRunnerTest| base class.
class DriverRunnerTest2 : public DriverRunnerTest, public ::testing::WithParamInterface<bool> {
 public:
  void SetUp() override { use_dynamic_linker_ = GetParam(); }

  void SetupDriverRunner(FakeDriverIndex fake_driver_index) {
    if (use_dynamic_linker_) {
      auto driver_host_runner =
          std::make_unique<driver_manager::DriverHostRunner>(dispatcher(), ConnectToRealm());
      DriverRunnerTest::SetupDriverRunnerWithDynamicLinker(
          dispatcher(), std::move(driver_host_runner), std::move(fake_driver_index));
    } else {
      DriverRunnerTest::SetupDriverRunner(std::move(fake_driver_index));
    }
  }

  void SetupDriverRunner() { SetupDriverRunner(CreateDriverIndex()); }

  zx::result<StartDriverResult> StartRootDriver() {
    if (use_dynamic_linker_) {
      return DriverRunnerTest::StartRootDriverDynamicLinking();
    } else {
      return DriverRunnerTest::StartRootDriver();
    }
  }

  StartDriverResult StartSecondDriver(bool colocate = false, bool host_restart_on_crash = false,
                                      bool use_next_vdso = false) {
    return DriverRunnerTest::StartSecondDriver(colocate, host_restart_on_crash, use_next_vdso,
                                               use_dynamic_linker());
  }

  // If |use_dynamic_linker| is not provided, it will be generated from the test configuration.
  void ValidateProgram(std::optional<::fuchsia_data::Dictionary>& program, std::string_view binary,
                       std::string_view colocate, std::string_view host_restart_on_crash,
                       std::string_view use_next_vdso,
                       std::optional<std::string_view> use_dynamic_linker = std::nullopt) {
    std::string use_dynamic_linker_str = use_dynamic_linker_ ? "true" : "false";
    if (use_dynamic_linker.has_value()) {
      use_dynamic_linker_str = use_dynamic_linker.value();
    }
    return DriverRunnerTest::ValidateProgram(program, binary, colocate, host_restart_on_crash,
                                             use_next_vdso, use_dynamic_linker_str);
  }

  bool use_dynamic_linker() const { return use_dynamic_linker_; }

 private:
  bool use_dynamic_linker_ = false;
};

// Start the root driver.
TEST_P(DriverRunnerTest2, StartRootDriver) {
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
TEST_P(DriverRunnerTest2, StartSecondDriver_NewDriverHost) {
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
TEST_P(DriverRunnerTest2, StartSecondDriver_SameDriverHost) {
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

  auto second_driver_config = kDefaultSecondDriverPkgConfig;
  std::string binary = std::string(second_driver_config.module_open_path);
  StartDriverHandler start_handler = [this, binary](TestDriver* driver,
                                                    fdfw::DriverStartArgs start_args) {
    auto& symbols = start_args.symbols().value();
    EXPECT_EQ(1u, symbols.size());
    EXPECT_EQ("sym", symbols[0].name().value());
    EXPECT_EQ(0xfeedu, symbols[0].address());
    ValidateProgram(start_args.program(), binary, "true", "false", "false");
  };
  auto [driver, controller] = StartDriverWithConfig(
      {
          .url = second_driver_url,
          .binary = binary,
          .colocate = true,
          .use_dynamic_linker = use_dynamic_linker(),
      },
      std::move(start_handler), second_driver_config);

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
        if (args.has_properties() && args.properties()[0].key.is_string_value() &&
            args.properties()[0].key.string_value().get() == "second_node_prop" &&
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
                  fdf::MakeProperty("second_node_prop", 0x2301u),
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
TEST_P(DriverRunnerTest2, BindAndRestartThroughRequest) {
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
  auto third_driver_config = kDefaultThirdDriverPkgConfig;
  std::string binary = std::string(third_driver_config.module_open_path);
  StartDriverHandler start_handler = [&](TestDriver* driver, fdfw::DriverStartArgs start_args) {
    EXPECT_FALSE(start_args.symbols().has_value());
    ValidateProgram(start_args.program(), binary, "false", "false", "false");
  };
  auto third_driver = StartDriverWithConfig(
      {
          .url = "fuchsia-boot:///#meta/third-driver.cm",
          .binary = binary,
          .use_dynamic_linker = use_dynamic_linker(),
      },
      std::move(start_handler), third_driver_config);

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
TEST_P(DriverRunnerTest2, StartSecondDriver_UnknownNode) {
  SetupDriverRunner();

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  std::shared_ptr<CreatedChild> child = root_driver->driver->AddChild("unknown-node", false, false);
  EXPECT_TRUE(RunLoopUntilIdle());

  StartDriver({.close = true, .use_dynamic_linker = use_dynamic_linker()});
  ASSERT_EQ(1u, driver_runner().bind_manager().NumOrphanedNodes());

  StopDriverComponent(std::move(root_driver->controller));
  realm().AssertDestroyedChildren({CreateChildRef("dev", "boot-drivers")});
}

// Start the root driver, and then add a child node that only binds to a base driver.
TEST_P(DriverRunnerTest2, StartSecondDriver_BindOrphanToBaseDriver) {
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
  driver_runner().TryBindAllAvailable();
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
TEST_P(DriverRunnerTest2, StartDriverChain_UnbindSecondNode) {
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

    auto driver_config = kDefaultDriverPkgConfig;
    std::string binary = std::string(driver_config.module_open_path);
    StartDriverHandler start_handler = [this, binary](TestDriver* driver,
                                                      fdfw::DriverStartArgs start_args) {
      EXPECT_FALSE(start_args.symbols().has_value());
      ValidateProgram(start_args.program(), binary, "false", "false", "false");
    };
    drivers.emplace_back(StartDriverWithConfig(
        {
            .url = "fuchsia-boot:///#meta/node-" + std::to_string(i) + "-driver.cm",
            .binary = binary,
            .use_dynamic_linker = use_dynamic_linker(),
        },
        std::move(start_handler), driver_config));
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

TEST_P(DriverRunnerTest2, CreateAndBindCompositeNodeSpec) {
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

  auto spec = std::make_unique<driver_manager::CompositeNodeSpecImpl>(
      driver_manager::CompositeNodeSpecCreateInfo{
          .name = name,
          .parents = fidl_spec.parents().value(),
      },
      dispatcher(), &driver_runner());
  fidl::Arena<> arena;

  driver_runner().composite_node_spec_manager().AddSpec(
      fidl::ToWire(arena, fidl_spec), std::move(spec),
      [](fit::result<fuchsia_driver_framework::CompositeNodeSpecError> result) {
        ASSERT_TRUE(result.is_ok());
      });
  EXPECT_TRUE(RunLoopUntilIdle());

  ASSERT_EQ(2u,
            driver_runner().composite_node_spec_manager().specs().at(name)->parent_nodes().size());

  ASSERT_FALSE(
      driver_runner().composite_node_spec_manager().specs().at(name)->parent_nodes().at(0));

  ASSERT_FALSE(
      driver_runner().composite_node_spec_manager().specs().at(name)->parent_nodes().at(1));

  auto root_driver = StartRootDriver();
  ASSERT_EQ(ZX_OK, root_driver.status_value());

  std::shared_ptr<CreatedChild> child_0 =
      root_driver->driver->AddChild("dev-group-0", false, false);
  std::shared_ptr<CreatedChild> child_1 =
      root_driver->driver->AddChild("dev-group-1", false, false);

  PrepareRealmForDriverComponentStart("dev.dev-group-1.test-group",
                                      "fuchsia-boot:///#meta/composite-driver.cm");
  EXPECT_TRUE(RunLoopUntilIdle());

  ASSERT_TRUE(driver_runner().composite_node_spec_manager().specs().at(name)->parent_nodes().at(0));

  ASSERT_TRUE(driver_runner().composite_node_spec_manager().specs().at(name)->parent_nodes().at(1));

  auto composite_driver_config = kDefaultCompositeDriverPkgConfig;
  std::string binary = std::string(composite_driver_config.module_open_path);
  StartDriverHandler start_handler = [this, binary](TestDriver* driver,
                                                    fdfw::DriverStartArgs start_args) {
    ValidateProgram(start_args.program(), binary, "true", "false", "false");
  };
  auto composite_driver = StartDriverWithConfig(
      {
          .url = "fuchsia-boot:///#meta/composite-driver.cm",
          .binary = binary,
          .colocate = true,
          .use_dynamic_linker = use_dynamic_linker(),
      },
      std::move(start_handler), composite_driver_config);

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

  driver_manager::BindResultTracker tracker(3, std::move(callback));
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

  driver_manager::BindResultTracker tracker_two(3, std::move(callback_two));
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

  driver_manager::BindResultTracker tracker_three(3, std::move(callback_three));
  ASSERT_EQ(false, callback_called);
  tracker_three.ReportNoBind();
  ASSERT_EQ(false, callback_called);
  tracker_three.ReportNoBind();
  ASSERT_EQ(false, callback_called);

  {
    tracker_three.ReportSuccessfulBind(std::string_view("node_name"),
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
        ASSERT_EQ(reply.value()->path.get(), "/dev/node-1");
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

  auto controller_endpoints = fidl::Endpoints<fuchsia_device::Controller>::Create();
  fidl::OneWayStatus result =
      device_controller_1->ConnectToController(std::move(controller_endpoints.server));
  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_EQ(result.status(), ZX_OK);

  fidl::WireClient<fuchsia_device::Controller> device_controller_2{
      std::move(controller_endpoints.client), dispatcher()};

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
  auto new_offer = driver_manager::CreateCompositeServiceOffer(arena, offer, "parent_node", false);
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
  auto new_offer = driver_manager::CreateCompositeServiceOffer(arena, offer, "parent_node", true);
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
      inspect.CreateDevice(kGrandparentName, kProtocolId));

  constexpr char kParentName[] = "parent";
  std::shared_ptr<Node> parent =
      std::make_shared<Node>(kParentName, std::vector<std::weak_ptr<Node>>{grandparent}, nullptr,
                             loop.dispatcher(), inspect.CreateDevice(kParentName, kProtocolId));

  constexpr char kChild1Name[] = "child1";
  std::shared_ptr<Node> child1 =
      std::make_shared<Node>(kChild1Name, std::vector<std::weak_ptr<Node>>{parent}, nullptr,
                             loop.dispatcher(), inspect.CreateDevice(kChild1Name, kProtocolId));

  constexpr char kChild2Name[] = "child2";
  std::shared_ptr<Node> child2 = std::make_shared<Node>(
      kChild2Name, std::vector<std::weak_ptr<Node>>{parent, child1}, nullptr, loop.dispatcher(),
      inspect.CreateDevice(kChild2Name, kProtocolId), 0, driver_manager::NodeType::kComposite);

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

// The tests are parameterized on whether to use the dynamic linker or not.
INSTANTIATE_TEST_SUITE_P(/* no prefix */, DriverRunnerTest2, testing::Values(true, false),
                         [](const testing::TestParamInfo<bool>& info) {
                           if (info.param) {
                             return "DynamicLinker";
                           }
                           return "Legacy";
                         });

}  // namespace driver_runner
