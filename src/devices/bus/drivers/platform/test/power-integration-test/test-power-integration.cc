// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.test/cpp/fidl.h>
#include <fidl/fuchsia.test.drivers.power/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/syslog/cpp/macros.h>

#include <unordered_set>

#include <fbl/unique_fd.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace power_integration_test {

class PowerIntegrationTest : public gtest::TestLoopFixture {};

TEST_F(PowerIntegrationTest, MetadataPassing) {
  auto builder = component_testing::RealmBuilder::Create();
  driver_test_realm::Setup(builder);

  auto decl = builder.GetComponentDecl("driver_test_realm");

  fuchsia::component::decl::OfferProtocol proto_offer;
  proto_offer.set_source(
      fuchsia::component::decl::Ref::WithParent(fuchsia::component::decl::ParentRef{}));
  proto_offer.set_source_name("fuchsia.power.broker.Topology");
  proto_offer.set_target(fuchsia::component::decl::Ref::WithCollection(
      fuchsia::component::decl::CollectionRef{.name = "realm_builder"}));
  proto_offer.set_target_name("fuchsia.power.broker.Topology");
  proto_offer.set_dependency_type(::fuchsia::component::decl::DependencyType::STRONG);

  decl.mutable_offers()->emplace_back(
      fuchsia::component::decl::Offer::WithProtocol(std::move(proto_offer)));
  builder.ReplaceComponentDecl("driver_test_realm", std::move(decl));

  async::Loop loop{&kAsyncLoopConfigNoAttachToCurrentThread};
  loop.StartThread();

  builder.AddChild(
      "power-broker", "#meta/mock-power-broker.cm",
      component_testing::ChildOptions{.startup_mode = component_testing::StartupMode::LAZY});

  builder.AddRoute(component_testing::Route{
      .capabilities = {component_testing::Protocol{.name = "fuchsia.logger.LogSink"}},
      .source = {component_testing::ParentRef{}},
      .targets = {component_testing::ChildRef{"power-broker"}},
  });

  builder.AddRoute(component_testing::Route{
      .capabilities = {component_testing::Protocol{.name = "fuchsia.power.broker.Topology"}},
      .source = {component_testing::ChildRef{"power-broker"}},
      .targets = {component_testing::ChildRef{"driver_test_realm"}},
  });

  builder.AddRoute(component_testing::Route{
      .capabilities = {component_testing::Protocol{
          .name = "fuchsia.test.drivers.power.GetPowerElements"}},
      .source = {component_testing::ChildRef{"power-broker"}},
      .targets = {component_testing::ParentRef{}},
  });

  auto test_realm = builder.Build(dispatcher());

  // connect to the realm protocol so we can control the test realm
  fidl::SyncClient<fuchsia_driver_test::Realm> realm_protocol(
      std::move(test_realm.component().Connect<fuchsia_driver_test::Realm>().value()));

  // now actually start the realm
  auto realm_args = fuchsia_driver_test::RealmArgs();
  realm_args.root_driver() = "fuchsia-boot:///#meta/platform-bus.cm";
  realm_args.offers() = {
      {fuchsia_driver_test::Offer{{.protocol_name = "fuchsia.power.broker.Topology",
                                   .collection = fuchsia_driver_test::Collection::kBootDrivers}}}};
  auto start_result = realm_protocol->Start(std::move(realm_args));
  ASSERT_TRUE(start_result.is_ok());

  auto get_elements_result =
      test_realm.component().Connect<fuchsia_test_drivers_power::GetPowerElements>(
          "fuchsia.test.drivers.power.GetPowerElements");
  ASSERT_TRUE(get_elements_result.is_ok());
  fidl::WireSyncClient<fuchsia_test_drivers_power::GetPowerElements> elements_client(
      std::move(get_elements_result.value()));

  std::unordered_set<std::string> expected_elements{"pe-fake-parent", "pe-fake-child"};

  while (expected_elements.size() > 0) {
    auto resp = elements_client->GetElements();
    fidl::VectorView<fidl::StringView> added_elements = resp->elements;
    for (fidl::StringView e : added_elements) {
      std::string element_name(e.data(), e.size());
      ASSERT_NE(expected_elements.end(), expected_elements.find(element_name));
      expected_elements.erase(element_name);
      FX_LOGS(INFO) << "Found element named '" << element_name << "'";
    }
  }
}
}  // namespace power_integration_test
