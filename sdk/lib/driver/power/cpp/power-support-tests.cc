// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/markers.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fidl/cpp/wire/string_view.h>
#include <lib/fidl/cpp/wire_natural_conversions.h>
#include <lib/zx/event.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/syscalls/object.h>

#include <fbl/ref_ptr.h>
#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/test_loop_fixture.h>
#include <src/storage/lib/vfs/cpp/pseudo_dir.h>
#include <src/storage/lib/vfs/cpp/service.h>
#include <src/storage/lib/vfs/cpp/synchronous_vfs.h>

#include "power-support.h"

namespace power_lib_test {
class PowerLibTest : public gtest::TestLoopFixture {};

class FakeTokenServer : public fidl::WireServer<fuchsia_hardware_power::PowerTokenProvider> {
 public:
  explicit FakeTokenServer(std::string element_name, zx::event event)
      : element_name_(std::move(element_name)), event_(std::move(event)) {}
  void GetToken(GetTokenCompleter::Sync& completer) override {
    zx::event dupe;
    ASSERT_EQ(event_.duplicate(ZX_RIGHT_SAME_RIGHTS, &dupe), ZX_OK);
    completer.ReplySuccess(std::move(dupe), fidl::StringView::FromExternal(element_name_));
  }
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_power::PowerTokenProvider> md,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_UNAVAILABLE);
  }
  ~FakeTokenServer() override = default;

 private:
  std::string element_name_;
  zx::event event_;
};

class PowerElement {
 public:
  explicit PowerElement(
      fidl::ServerEnd<fuchsia_power_broker::ElementControl> ec,
      std::optional<fidl::ServerEnd<fuchsia_power_broker::Lessor>> lessor,
      std::optional<fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>> current_level,
      std::optional<fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>> required_level)
      : element_control_(std::move(ec)),
        lessor_(std::move(lessor)),
        current_level_(std::move(current_level)),
        required_level_(std::move(required_level)) {}

 private:
  fidl::ServerEnd<fuchsia_power_broker::ElementControl> element_control_;
  std::optional<fidl::ServerEnd<fuchsia_power_broker::Lessor>> lessor_;
  std::optional<fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>> current_level_;
  std::optional<fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>> required_level_;
};

class TopologyServer : public fidl::Server<fuchsia_power_broker::Topology> {
 public:
  void AddElement(fuchsia_power_broker::ElementSchema& req,
                  AddElementCompleter::Sync& completer) override {
    // Store a copy of the dependencies represented in this request
    for (auto& it : req.dependencies().value()) {
      zx::event token_copy;
      it.requires_token().duplicate(ZX_RIGHT_SAME_RIGHTS, &token_copy);

      fuchsia_power_broker::LevelDependency dep = {{
          .dependency_type = it.dependency_type(),
          .dependent_level = it.dependent_level(),
          .requires_token = std::move(token_copy),
          .requires_level = it.requires_level(),
      }};
      received_deps_.emplace_back(std::move(dep));
    }

    // Make channels to return to client
    auto element_control = fidl::CreateEndpoints<fuchsia_power_broker::ElementControl>();

    if (req.level_control_channels().has_value()) {
      PowerElement element{std::move(element_control->server), std::move(req.lessor_channel()),
                           std::move(req.level_control_channels().value().current()),
                           std::move(req.level_control_channels().value().required())};

      clients_.emplace_back(std::move(element));
    } else {
      PowerElement element{std::move(element_control->server), std::move(req.lessor_channel()),
                           std::nullopt, std::nullopt};

      clients_.emplace_back(std::move(element));
    }
    fuchsia_power_broker::TopologyAddElementResponse result{
        {.element_control_channel = std::move(element_control->client)},
    };
    fit::success<fuchsia_power_broker::TopologyAddElementResponse> success(std::move(result));

    completer.Reply(std::move(success));
  }
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Topology> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}
  std::vector<fuchsia_power_broker::LevelDependency> received_deps_;

 private:
  std::vector<PowerElement> clients_;
};

class AddInstanceResult {
 public:
  fbl::RefPtr<fs::Service> token_service;
  fbl::RefPtr<fs::PseudoDir> service_instance;
  std::shared_ptr<FakeTokenServer> token_handler;
  std::shared_ptr<zx::event> token;
};

AddInstanceResult AddServiceInstance(
    std::string parent_name, async_dispatcher_t* dispatcher,
    fidl::ServerBindingGroup<fuchsia_hardware_power::PowerTokenProvider>* bindings) {
  zx::event raw_event;
  zx::event::create(0, &raw_event);
  std::shared_ptr<zx::event> event = std::make_shared<zx::event>(std::move(raw_event));

  zx::event event_copy;
  event->duplicate(ZX_RIGHT_SAME_RIGHTS, &event_copy);
  std::shared_ptr<FakeTokenServer> token_handler =
      std::make_shared<FakeTokenServer>(parent_name, std::move(event_copy));
  fbl::RefPtr<fs::PseudoDir> service_instance = fbl::MakeRefCounted<fs::PseudoDir>();
  fbl::RefPtr<fs::Service> token_server = fbl::MakeRefCounted<fs::Service>(
      [token_handler, dispatcher,
       bindings](fidl::ServerEnd<fuchsia_hardware_power::PowerTokenProvider> chan) {
        bindings->AddBinding(dispatcher, std::move(chan), token_handler.get(),
                             fidl::kIgnoreBindingClosure);
        return ZX_OK;
      });

  // Build up the directory structure so that we have an entry for the service
  // and inside that an entry for the instance.
  service_instance->AddEntry(fuchsia_hardware_power::PowerTokenService::TokenProvider::Name,
                             token_server);
  return AddInstanceResult{
      .token_service = std::move(token_server),
      .service_instance = std::move(service_instance),
      .token_handler = std::move(token_handler),
      .token = std::move(event),
  };
}

/// Add an element which has no dependencies
TEST_F(PowerLibTest, AddElementNoDep) {
  // Create the dependency configuration and create a
  // map<parent_name, vec<level_deps> used for call validation later.
  std::string parent_name = "element_first_parent";
  fuchsia_hardware_power::PowerLevel one = {{.level = 0, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 1, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 2, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {{}}}};

  std::unordered_map<fuchsia_hardware_power::ParentElement, zx::event,
                     fdf_power::ParentElementHasher>
      tokens;

  // Make the fake power broker
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread();
  std::unique_ptr<TopologyServer> fake_power_broker = std::make_unique<TopologyServer>();
  fidl::Endpoints<fuchsia_power_broker::Topology> endpoints =
      fidl::CreateEndpoints<fuchsia_power_broker::Topology>().value();

  fidl::ServerBindingRef<fuchsia_power_broker::Topology> bindings =
      fidl::BindServer<fuchsia_power_broker::Topology>(
          loop.dispatcher(), std::move(endpoints.server), std::move(fake_power_broker),
          [](TopologyServer* impl, fidl::UnbindInfo info,
             fidl::ServerEnd<fuchsia_power_broker::Topology> chan) {
            // Check that we have the right number of dependencies received
            ASSERT_EQ(static_cast<size_t>(0), impl->received_deps_.size());
          });
  // Call add element
  fidl::Arena arena;
  zx::event invalid, invalid2;
  auto call_result =
      fdf_power::AddElement(endpoints.client, fidl::ToWire(arena, df_config), std::move(tokens),
                            invalid.borrow(), invalid2.borrow(), std::nullopt, std::nullopt);
  ASSERT_TRUE(call_result.is_ok());
  loop.Shutdown();
  loop.JoinThreads();
}

/// Add an element which has a has multiple level dependencies on a single
/// parent element. Verifies the dependency token is correct.
TEST_F(PowerLibTest, AddElementSingleDep) {
  // Create the dependency configuration and create a
  // map<parent_name, vec<level_deps> used for call validation later.
  std::string parent_name = "element_first_parent";
  fuchsia_hardware_power::PowerLevel one = {{.level = 0, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 1, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 2, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  fuchsia_hardware_power::LevelTuple one_to_one = {{
      .child_level = 1,
      .parent_level = 1,
  }};
  fuchsia_hardware_power::LevelTuple three_to_two = {{
      .child_level = 3,
      .parent_level = 2,
  }};

  fuchsia_hardware_power::PowerDependency power_dep = {{
      .child = "n/a",
      .parent = fuchsia_hardware_power::ParentElement::WithName(parent_name),
      .level_deps = {{one_to_one, three_to_two}},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {{power_dep}}}};

  zx::event parent_token;
  ASSERT_EQ(ZX_OK, zx::event::create(0, &parent_token));

  // map of level dependencies we'll use later for validation
  std::unordered_map<uint8_t, uint8_t> child_to_parent_levels{
      {one_to_one.child_level().value(), one_to_one.parent_level().value()},
      {three_to_two.child_level().value(), three_to_two.parent_level().value()}};

  // Create the map of dependency names to zx::event tokens
  // Make a copy of the token
  zx::event token_copy;
  ASSERT_EQ(ZX_OK, parent_token.duplicate(ZX_RIGHT_SAME_RIGHTS, &token_copy));
  std::unordered_map<fuchsia_hardware_power::ParentElement, zx::event,
                     fdf_power::ParentElementHasher>
      tokens;
  tokens.insert(std::make_pair(fuchsia_hardware_power::ParentElement::WithName(parent_name),
                               std::move(token_copy)));

  // Make the fake power broker
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread();
  std::unique_ptr<TopologyServer> fake_power_broker = std::make_unique<TopologyServer>();
  fidl::Endpoints<fuchsia_power_broker::Topology> endpoints =
      fidl::CreateEndpoints<fuchsia_power_broker::Topology>().value();

  fidl::ServerBindingRef<fuchsia_power_broker::Topology> bindings =
      fidl::BindServer<fuchsia_power_broker::Topology>(
          loop.dispatcher(), std::move(endpoints.server), std::move(fake_power_broker),

          [parent_token = std::move(parent_token),
           child_to_parent_levels = std::move(child_to_parent_levels)](
              TopologyServer* impl, fidl::UnbindInfo info,
              fidl::ServerEnd<fuchsia_power_broker::Topology> chan) mutable {
            // Check that we have the right number of dependencies received
            ASSERT_EQ(impl->received_deps_.size(), static_cast<size_t>(2));

            // Since both power levels dependended on the same parent power element
            // that both access tokens match the one we made in the test
            zx_info_handle_basic_t orig_info, copy_info;
            parent_token.get_info(ZX_INFO_HANDLE_BASIC, &orig_info, sizeof(zx_info_handle_basic_t),
                                  nullptr, nullptr);
            for (fuchsia_power_broker::LevelDependency& dep : impl->received_deps_) {
              dep.requires_token().get_info(ZX_INFO_HANDLE_BASIC, &copy_info,
                                            sizeof(zx_info_handle_basic_t), nullptr, nullptr);
              auto entry = child_to_parent_levels.extract(dep.dependent_level());
              ASSERT_EQ(entry.mapped(), dep.requires_level());
              ASSERT_EQ(copy_info.koid, orig_info.koid);
            }
            ASSERT_EQ(child_to_parent_levels.size(), static_cast<size_t>(0));
          });

  // Call add element
  fidl::Arena arena;
  zx::event invalid1, invalid2;
  auto call_result =
      fdf_power::AddElement(endpoints.client, fidl::ToWire(arena, df_config), std::move(tokens),
                            invalid1.borrow(), invalid2.borrow(), std::nullopt, std::nullopt);

  ASSERT_TRUE(call_result.is_ok());

  loop.Shutdown();
  loop.JoinThreads();
}

/// Add an element that has dependencies on two different parent elements.
/// Validates that dependency tokens are the right ones.
TEST_F(PowerLibTest, AddElementDoubleDep) {
  // Create the dependency configuration and create a
  // map<parent_name, vec<level_deps> used for call validation later.
  fuchsia_hardware_power::ParentElement parent_name_first =
      fuchsia_hardware_power::ParentElement::WithName("element_first_parent");
  fuchsia_hardware_power::ParentElement parent_name_second =
      fuchsia_hardware_power::ParentElement::WithName("element_second_parent");
  fuchsia_hardware_power::PowerLevel one = {{.level = 0, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 1, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 2, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  uint16_t dep_one_level = 1;
  fuchsia_hardware_power::LevelTuple one_to_one = {{
      .child_level = dep_one_level,
      .parent_level = 1,
  }};

  fuchsia_hardware_power::PowerDependency power_dep_one = {{
      .child = "n/a",
      .parent = parent_name_first,
      .level_deps = {{one_to_one}},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  uint16_t dep_two_level = 3;
  fuchsia_hardware_power::LevelTuple three_to_two = {{
      .child_level = dep_two_level,
      .parent_level = 2,
  }};

  fuchsia_hardware_power::PowerDependency power_dep_two = {{
      .child = "n/a",
      .parent = parent_name_second,
      .level_deps = {{three_to_two}},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {{power_dep_one, power_dep_two}}}};

  zx::event parent_token_one;
  ASSERT_EQ(ZX_OK, zx::event::create(0, &parent_token_one));

  zx::event parent_token_two;
  ASSERT_EQ(ZX_OK, zx::event::create(0, &parent_token_two));

  // map of level dependencies we'll use later for validation
  std::unordered_map<uint16_t, uint16_t> child_to_parent_levels{
      {one_to_one.child_level().value(), one_to_one.parent_level().value()},
      {three_to_two.child_level().value(), three_to_two.parent_level().value()}};

  // Create the map of dependency names to zx::event tokens
  // Make a copy of the token
  std::unordered_map<fuchsia_hardware_power::ParentElement, zx::event,
                     fdf_power::ParentElementHasher>
      tokens;
  {
    zx::event token_one_copy;
    ASSERT_EQ(ZX_OK, parent_token_one.duplicate(ZX_RIGHT_SAME_RIGHTS, &token_one_copy));

    zx::event token_two_copy;
    ASSERT_EQ(ZX_OK, parent_token_two.duplicate(ZX_RIGHT_SAME_RIGHTS, &token_two_copy));

    tokens.insert(std::make_pair(parent_name_first, std::move(token_one_copy)));
    tokens.insert(std::make_pair(parent_name_second, std::move(token_two_copy)));
  }

  // Make the fake power broker
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread();
  std::unique_ptr<TopologyServer> fake_power_broker = std::make_unique<TopologyServer>();
  fidl::Endpoints<fuchsia_power_broker::Topology> endpoints =
      fidl::CreateEndpoints<fuchsia_power_broker::Topology>().value();

  fidl::ServerBindingRef<fuchsia_power_broker::Topology> bindings =
      fidl::BindServer<fuchsia_power_broker::Topology>(
          loop.dispatcher(), std::move(endpoints.server), std::move(fake_power_broker),
          [parent_token_one = std::move(parent_token_one),
           parent_token_two = std::move(parent_token_two),
           child_to_parent_levels = std::move(child_to_parent_levels),
           dep_one_level = dep_one_level](TopologyServer* impl, fidl::UnbindInfo info,
                                          fidl::ServerEnd<fuchsia_power_broker::Topology>) mutable {
            // Check that we have the right number of dependencies received
            ASSERT_EQ(impl->received_deps_.size(), static_cast<size_t>(2));

            // Since both power levels dependended on the same parent power element
            // that both access tokens match the one we made in the test
            zx_info_handle_basic_t parent_one_info, parent_two_info, copy_info;
            parent_token_one.get_info(ZX_INFO_HANDLE_BASIC, &parent_one_info,
                                      sizeof(zx_info_handle_basic_t), nullptr, nullptr);
            parent_token_two.get_info(ZX_INFO_HANDLE_BASIC, &parent_two_info,
                                      sizeof(zx_info_handle_basic_t), nullptr, nullptr);
            for (fuchsia_power_broker::LevelDependency& dep : impl->received_deps_) {
              dep.requires_token().get_info(ZX_INFO_HANDLE_BASIC, &copy_info,
                                            sizeof(zx_info_handle_basic_t), nullptr, nullptr);
              // Since each dependency has a different dependent level, use the dependent
              // level to differentiate which access token to check against. Delightfully
              // basic since we know we only have two dependencies.
              if (dep.dependent_level() == dep_one_level) {
                ASSERT_EQ(copy_info.koid, parent_one_info.koid);
              } else {
                ASSERT_EQ(copy_info.koid, parent_two_info.koid);
              }
              auto entry = child_to_parent_levels.extract(dep.dependent_level());
              ASSERT_EQ(entry.mapped(), dep.requires_level());
            }

            ASSERT_EQ(child_to_parent_levels.size(), static_cast<size_t>(0));
          });

  // Call add element
  fidl::Arena arena;
  zx::event invalid1, invalid2;
  auto call_result =
      fdf_power::AddElement(endpoints.client, fidl::ToWire(arena, df_config), std::move(tokens),
                            invalid1.borrow(), invalid2.borrow(), std::nullopt, std::nullopt);

  ASSERT_TRUE(call_result.is_ok());

  loop.Shutdown();
  loop.JoinThreads();
}

/// Check that a power element with two levels, dependent on two different
/// parent levels is converted correctly from configuration format to the
/// format used by Power Framework.
TEST_F(PowerLibTest, LevelDependencyWithSingleParent) {
  fuchsia_hardware_power::ParentElement parent =
      fuchsia_hardware_power::ParentElement::WithName("element_first_parent");
  fuchsia_hardware_power::PowerLevel one = {{.level = 0, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 1, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 2, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  fuchsia_hardware_power::LevelTuple one_to_one = {{
      .child_level = 1,
      .parent_level = 1,
  }};
  fuchsia_hardware_power::LevelTuple three_to_two = {{
      .child_level = 3,
      .parent_level = 2,
  }};

  std::unordered_map<uint16_t, uint16_t> child_to_parent_levels{
      {one_to_one.child_level().value(), one_to_one.parent_level().value()},
      {three_to_two.child_level().value(), three_to_two.parent_level().value()}};

  fuchsia_hardware_power::PowerDependency power_dep = {{
      .child = "n/a",
      .parent = parent,
      .level_deps = {{one_to_one, three_to_two}},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {{power_dep}}}};
  fidl::Arena test;
  auto output = fdf_power::LevelDependencyFromConfig(fidl::ToWire(test, df_config)).value();

  // we expect that "element_first_parent" will have two entries
  // one for each of the level deps we've expressed
  std::vector<fuchsia_power_broker::LevelDependency>& deps = output[parent];
  ASSERT_EQ(static_cast<size_t>(2), deps.size());

  // Check that the translated dependencies match the ones we put in
  for (auto& dep : deps) {
    ASSERT_EQ(dep.dependency_type(), fuchsia_power_broker::DependencyType::kActive);
    uint8_t parent_level =
        static_cast<uint8_t>(child_to_parent_levels.extract(dep.dependent_level()).mapped());
    ASSERT_EQ(dep.requires_level(), parent_level);
  }

  // Check that we took out all the mappings
  ASSERT_EQ(child_to_parent_levels.size(), static_cast<size_t>(0));
}

/// Check that power levels are take out of the driver config format correctly.
TEST_F(PowerLibTest, ExtractPowerLevelsFromConfig) {
  fuchsia_hardware_power::ParentElement parent =
      fuchsia_hardware_power::ParentElement::WithName("element_first_parent");
  fuchsia_hardware_power::PowerLevel one = {{.level = 0, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 0, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 0, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  fuchsia_hardware_power::PowerDependency power_dep = {{
      .child = "n/a",
      .parent = parent,
      .level_deps = {},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {{power_dep}}}};

  fidl::Arena test;
  auto converted = fdf_power::PowerLevelsFromConfig(fidl::ToWire(test, df_config));

  ASSERT_EQ(static_cast<size_t>(3), converted.size());
}

/// Get the dependency tokens for an element that has no dependencies.
/// This should result in no tokens and no errors.
TEST_F(PowerLibTest, GetTokensNoTokens) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread();

  // create a namespace that has a directory for the PowerTokenService, but
  // no entries
  fbl::RefPtr<fs::PseudoDir> empty_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  fs::SynchronousVfs vfs(loop.dispatcher());
  fidl::Endpoints<fuchsia_io::Directory> dir_endpoints =
      fidl::CreateEndpoints<fuchsia_io::Directory>().value();
  vfs.ServeDirectory(std::move(empty_dir), std::move(dir_endpoints.server));

  fuchsia_hardware_power::PowerLevel one = {{.level = 1, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 2, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 3, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  // Specify no dependencies
  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {}}};

  fidl::Arena test;
  fit::result<fdf_power::Error, std::unordered_map<fuchsia_hardware_power::ParentElement, zx::event,
                                                   fdf_power::ParentElementHasher>>
      result = fdf_power::GetDependencyTokens(fidl::ToWire(test, df_config),
                                              std::move(dir_endpoints.client));

  // Should be no tokens, but also no errors
  ASSERT_EQ(result.value().size(), static_cast<size_t>(0));
  loop.Shutdown();
  loop.JoinThreads();
}

/// Get the tokens for an element that has one level dependent on one other
/// power element.
TEST_F(PowerLibTest, GetTokensOneDepOneLevel) {
  std::string parent_name = "parentOne";
  fuchsia_hardware_power::ParentElement parent =
      fuchsia_hardware_power::ParentElement::WithName(parent_name);
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread();
  fidl::ServerBindingGroup<fuchsia_hardware_power::PowerTokenProvider> bindings;

  fbl::RefPtr<fs::PseudoDir> power_token_service = fbl::MakeRefCounted<fs::PseudoDir>();

  AddInstanceResult instance_data = AddServiceInstance(parent_name, loop.dispatcher(), &bindings);

  fbl::RefPtr<fs::PseudoDir> svc_instances_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  ASSERT_EQ(svc_instances_dir->AddEntry("instance_one", instance_data.service_instance), ZX_OK);
  ASSERT_EQ(power_token_service->AddEntry(fuchsia_hardware_power::PowerTokenService::Name,
                                          svc_instances_dir),
            ZX_OK);

  fs::SynchronousVfs vfs(loop.dispatcher());
  fidl::Endpoints<fuchsia_io::Directory> dir_endpoints =
      fidl::CreateEndpoints<fuchsia_io::Directory>().value();
  vfs.ServeDirectory(std::move(power_token_service), std::move(dir_endpoints.server));

  // Specify the power element
  fuchsia_hardware_power::PowerLevel one = {{.level = 0, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 1, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 2, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  // Create a dependency between a level on this element and a level on the
  // parent
  fuchsia_hardware_power::PowerDependency power_dep = {{
      .child = "n/a",
      .parent = parent,
      .level_deps = {{
          {{
              .child_level = 1,
              .parent_level = 2,
          }},
      }},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {{power_dep}}}};

  // With the given configuration, get the dependency tokens. We expect this to
  // call into the PowerTokenProvider we built above which calls into our
  // FakeTokenServer instance.
  fidl::Arena test;
  fit::result<fdf_power::Error, fdf_power::TokenMap> result = fdf_power::GetDependencyTokens(
      fidl::ToWire(test, df_config), std::move(dir_endpoints.client));

  ASSERT_TRUE(result.is_ok());
  fdf_power::TokenMap token_map = std::move(result.value());
  ASSERT_EQ(token_map.size(), static_cast<size_t>(1));

  // Check that the zx::event defined in the test and the one return by
  // `GetTokens` are pointing at the same kernel object
  zx_info_handle_basic_t info1, info2;
  instance_data.token->get_info(ZX_INFO_HANDLE_BASIC, &info1, sizeof(zx_info_handle_basic_t),
                                nullptr, nullptr);
  token_map.begin()->second.get_info(ZX_INFO_HANDLE_BASIC, &info2, sizeof(zx_info_handle_basic_t),
                                     nullptr, nullptr);
  ASSERT_EQ(info1.koid, info2.koid);

  loop.Shutdown();
  loop.JoinThreads();
}

/// Get tokens for a power element which two levels that have dependencies on
/// the same parent element.
TEST_F(PowerLibTest, GetTokensOneDepTwoLevels) {
  std::string parent_name = "parentOne";
  fuchsia_hardware_power::ParentElement parent =
      fuchsia_hardware_power::ParentElement::WithName(parent_name);
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread();
  fidl::ServerBindingGroup<fuchsia_hardware_power::PowerTokenProvider> bindings;

  fbl::RefPtr<fs::PseudoDir> namespace_svc_dir = fbl::MakeRefCounted<fs::PseudoDir>();

  fbl::RefPtr<fs::PseudoDir> svc_instances_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  AddInstanceResult instance_data = AddServiceInstance(parent_name, loop.dispatcher(), &bindings);
  ASSERT_EQ(svc_instances_dir->AddEntry("instance_one", instance_data.service_instance), ZX_OK);

  ASSERT_EQ(namespace_svc_dir->AddEntry(fuchsia_hardware_power::PowerTokenService::Name,
                                        svc_instances_dir),
            ZX_OK);

  fs::SynchronousVfs vfs(loop.dispatcher());
  fidl::Endpoints<fuchsia_io::Directory> dir_endpoints =
      fidl::CreateEndpoints<fuchsia_io::Directory>().value();
  vfs.ServeDirectory(std::move(namespace_svc_dir), std::move(dir_endpoints.server));

  // Specify the power element
  fuchsia_hardware_power::PowerLevel one = {{.level = 0, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 1, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 2, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  // Create a dependency between a level on this element and a level on the
  // parent
  fuchsia_hardware_power::PowerDependency power_dep = {{
      .child = "n/a",
      .parent = parent,
      .level_deps = {{
          {{
              .child_level = 1,
              .parent_level = 2,
          }},
          {{
              .child_level = 2,
              .parent_level = 4,
          }},
      }},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {{power_dep}}}};

  // With the given configuration, get the dependency tokens. We expect this to
  // call into the PowerTokenProvider we built above which calls into our
  // FakeTokenServer instance.
  fidl::Arena test;
  fit::result<fdf_power::Error, fdf_power::TokenMap> result = fdf_power::GetDependencyTokens(
      fidl::ToWire(test, df_config), std::move(dir_endpoints.client));

  ASSERT_TRUE(result.is_ok());
  fdf_power::TokenMap token_map = std::move(result.value());
  ASSERT_EQ(token_map.size(), static_cast<size_t>(1));

  // Check that the zx::event defined in the test and the one return by
  // `GetTokens` are pointing at the same kernel object
  zx_info_handle_basic_t info1, info2;
  instance_data.token->get_info(ZX_INFO_HANDLE_BASIC, &info1, sizeof(zx_info_handle_basic_t),
                                nullptr, nullptr);
  token_map.begin()->second.get_info(ZX_INFO_HANDLE_BASIC, &info2, sizeof(zx_info_handle_basic_t),
                                     nullptr, nullptr);
  ASSERT_EQ(info1.koid, info2.koid);

  loop.Shutdown();
  loop.JoinThreads();
}

/// Check GetTokens against an element which has two levels, each of which
/// has a dependency on two different parents.
TEST_F(PowerLibTest, GetTokensTwoDepTwoLevels) {
  std::string parent_name1 = "parentOne";
  fuchsia_hardware_power::ParentElement parent_element1 =
      fuchsia_hardware_power::ParentElement::WithName(parent_name1);
  std::string parent_name2 = "parentTwo";
  fuchsia_hardware_power::ParentElement parent_element2 =
      fuchsia_hardware_power::ParentElement::WithName(parent_name2);
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread();
  fidl::ServerBindingGroup<fuchsia_hardware_power::PowerTokenProvider> bindings;

  fbl::RefPtr<fs::PseudoDir> power_token_service = fbl::MakeRefCounted<fs::PseudoDir>();

  AddInstanceResult instance_one_data =
      AddServiceInstance(parent_name1, loop.dispatcher(), &bindings);
  AddInstanceResult instance_two_data =
      AddServiceInstance(parent_name2, loop.dispatcher(), &bindings);

  // Create the directory holding the service instances
  fbl::RefPtr<fs::PseudoDir> svc_instances_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  ASSERT_EQ(svc_instances_dir->AddEntry("instance_two", instance_one_data.service_instance), ZX_OK);
  ASSERT_EQ(svc_instances_dir->AddEntry("instance_one", instance_two_data.service_instance), ZX_OK);
  ASSERT_EQ(power_token_service->AddEntry(fuchsia_hardware_power::PowerTokenService::Name,
                                          svc_instances_dir),
            ZX_OK);

  fs::SynchronousVfs vfs(loop.dispatcher());
  fidl::Endpoints<fuchsia_io::Directory> dir_endpoints =
      fidl::CreateEndpoints<fuchsia_io::Directory>().value();
  vfs.ServeDirectory(std::move(power_token_service), std::move(dir_endpoints.server));

  // Specify the power element
  fuchsia_hardware_power::PowerLevel one = {{.level = 0, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 1, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 2, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  // Create a dependency between a level on this element and a level on the
  // parent
  fuchsia_hardware_power::PowerDependency power_dep_one = {{
      .child = "n/a",
      .parent = parent_element1,
      .level_deps = {{
          {{
              .child_level = 1,
              .parent_level = 2,
          }},
      }},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  // Create a dependency between a level on this element and a level on the
  // parent
  fuchsia_hardware_power::PowerDependency power_dep_two = {{
      .child = "n/a",
      .parent = parent_element2,
      .level_deps = {{
          {{
              .child_level = 2,
              .parent_level = 4,
          }},
      }},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {{power_dep_one, power_dep_two}}}};

  // With the given configuration, get the dependency tokens. We expect this to
  // call into the PowerTokenProvider we built above which calls into our
  // FakeTokenServer instance.
  fidl::Arena test;
  fit::result<fdf_power::Error, fdf_power::TokenMap> result = fdf_power::GetDependencyTokens(
      fidl::ToWire(test, df_config), std::move(dir_endpoints.client));

  ASSERT_TRUE(result.is_ok());
  fdf_power::TokenMap token_map = std::move(result.value());
  ASSERT_EQ(token_map.size(), static_cast<size_t>(2));

  // Check that the zx::event defined in the test and the one return by
  // `GetTokens` are pointing at the same kernel object
  zx_info_handle_basic_t info1, info2;
  instance_one_data.token->get_info(ZX_INFO_HANDLE_BASIC, &info1, sizeof(zx_info_handle_basic_t),
                                    nullptr, nullptr);
  token_map.at(parent_element1)
      .get_info(ZX_INFO_HANDLE_BASIC, &info2, sizeof(zx_info_handle_basic_t), nullptr, nullptr);
  ASSERT_EQ(info1.koid, info2.koid);

  instance_two_data.token->get_info(ZX_INFO_HANDLE_BASIC, &info1, sizeof(zx_info_handle_basic_t),
                                    nullptr, nullptr);
  token_map.at(parent_element2)
      .get_info(ZX_INFO_HANDLE_BASIC, &info2, sizeof(zx_info_handle_basic_t), nullptr, nullptr);
  ASSERT_EQ(info1.koid, info2.koid);

  loop.Shutdown();
  loop.JoinThreads();
}

/// Check GetTokens with a power elements with a single level which depends
/// on two different parent power elements.
TEST_F(PowerLibTest, GetTokensOneLevelTwoDeps) {
  std::string parent_name1 = "parentOne";
  fuchsia_hardware_power::ParentElement parent_element1 =
      fuchsia_hardware_power::ParentElement::WithName(parent_name1);
  std::string parent_name2 = "parenttwo";
  fuchsia_hardware_power::ParentElement parent_element2 =
      fuchsia_hardware_power::ParentElement::WithName(parent_name2);
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread();
  fidl::ServerBindingGroup<fuchsia_hardware_power::PowerTokenProvider> bindings;

  fbl::RefPtr<fs::PseudoDir> power_token_service = fbl::MakeRefCounted<fs::PseudoDir>();

  AddInstanceResult instance_one_data =
      AddServiceInstance(parent_name1, loop.dispatcher(), &bindings);
  AddInstanceResult instance_two_data =
      AddServiceInstance(parent_name2, loop.dispatcher(), &bindings);

  // Create the directory holding the service instances
  fbl::RefPtr<fs::PseudoDir> svc_instances_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  ASSERT_EQ(svc_instances_dir->AddEntry("instance_two", instance_two_data.service_instance), ZX_OK);
  ASSERT_EQ(svc_instances_dir->AddEntry("instance_one", instance_one_data.service_instance), ZX_OK);
  ASSERT_EQ(power_token_service->AddEntry(fuchsia_hardware_power::PowerTokenService::Name,
                                          svc_instances_dir),
            ZX_OK);

  fs::SynchronousVfs vfs(loop.dispatcher());
  fidl::Endpoints<fuchsia_io::Directory> dir_endpoints =
      fidl::CreateEndpoints<fuchsia_io::Directory>().value();
  vfs.ServeDirectory(std::move(power_token_service), std::move(dir_endpoints.server));

  // Specify the power element
  fuchsia_hardware_power::PowerLevel one = {{.level = 0, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 1, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 2, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  // Create a dependency between a level on this element and a level on the
  // parent
  fuchsia_hardware_power::PowerDependency power_dep_one = {{
      .child = "n/a",
      .parent = parent_element1,
      .level_deps = {{
          {{
              .child_level = 1,
              .parent_level = 2,
          }},
      }},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  // Create a dependency between a level on this element and a level on the
  // parent
  fuchsia_hardware_power::PowerDependency power_dep_two = {{
      .child = "n/a",
      .parent = parent_element2,
      .level_deps = {{
          {{
              .child_level = 1,
              .parent_level = 6,
          }},
      }},
      .strength = fuchsia_hardware_power::RequirementType::kActive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {{power_dep_one, power_dep_two}}}};

  // With the given configuration, get the dependency tokens. We expect this to
  // call into the PowerTokenProvider we built above which calls into our
  // FakeTokenServer instance.
  fidl::Arena test;
  fit::result<fdf_power::Error, fdf_power::TokenMap> result = fdf_power::GetDependencyTokens(
      fidl::ToWire(test, df_config), std::move(dir_endpoints.client));

  ASSERT_TRUE(result.is_ok());
  fdf_power::TokenMap token_map = std::move(result.value());
  ASSERT_EQ(token_map.size(), static_cast<size_t>(2));

  // Check that the zx::event defined in the test and the one return by
  // `GetTokens` are pointing at the same kernel object
  zx_info_handle_basic_t info1, info2;
  instance_one_data.token->get_info(ZX_INFO_HANDLE_BASIC, &info1, sizeof(zx_info_handle_basic_t),
                                    nullptr, nullptr);
  token_map.at(parent_element1)
      .get_info(ZX_INFO_HANDLE_BASIC, &info2, sizeof(zx_info_handle_basic_t), nullptr, nullptr);
  ASSERT_EQ(info1.koid, info2.koid);

  instance_two_data.token->get_info(ZX_INFO_HANDLE_BASIC, &info1, sizeof(zx_info_handle_basic_t),
                                    nullptr, nullptr);
  token_map.at(parent_element2)
      .get_info(ZX_INFO_HANDLE_BASIC, &info2, sizeof(zx_info_handle_basic_t), nullptr, nullptr);
  ASSERT_EQ(info1.koid, info2.koid);

  loop.Shutdown();
  loop.JoinThreads();
}

class SystemActivityGovernor : public fidl::Server<fuchsia_power_system::ActivityGovernor> {
 public:
  SystemActivityGovernor(zx::event exec_state_passive, zx::event wake_handling_active)
      : exec_state_passive_(std::move(exec_state_passive)),
        wake_handling_active_(std::move(wake_handling_active)) {}

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override {
    fuchsia_power_system::PowerElements elements;
    zx::event execution_element, wake_handling_element;
    exec_state_passive_.duplicate(ZX_RIGHT_SAME_RIGHTS, &execution_element);
    wake_handling_active_.duplicate(ZX_RIGHT_SAME_RIGHTS, &wake_handling_element);

    fuchsia_power_system::ExecutionState exec_state = {
        {.passive_dependency_token = std::move(execution_element)}};

    fuchsia_power_system::WakeHandling wake_handling = {
        {.active_dependency_token = std::move(wake_handling_element)}};

    elements = {
        {.execution_state = std::move(exec_state), .wake_handling = std::move(wake_handling)}};

    completer.Reply({{std::move(elements)}});
  }

  void RegisterListener(RegisterListenerRequest& req,
                        RegisterListenerCompleter::Sync& completer) override {}

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  zx::event exec_state_passive_;
  zx::event wake_handling_active_;
};

/// Test getting dependency tokens for an element that depends on SAG's power
/// elements.
TEST_F(PowerLibTest, TestSagElements) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread();

  zx::event exec_passive, wake_active;
  zx::event::create(0, &exec_passive);
  zx::event::create(0, &wake_active);
  zx::event exec_passive_dupe, wake_active_dupe;
  exec_passive.duplicate(ZX_RIGHT_SAME_RIGHTS, &exec_passive_dupe);
  wake_active.duplicate(ZX_RIGHT_SAME_RIGHTS, &wake_active_dupe);

  SystemActivityGovernor sag_server(std::move(exec_passive_dupe), std::move(wake_active_dupe));

  fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor> bindings;
  fbl::RefPtr<fs::Service> sag = fbl::MakeRefCounted<fs::Service>(
      [&](fidl::ServerEnd<fuchsia_power_system::ActivityGovernor> chan) {
        bindings.AddBinding(loop.dispatcher(), std::move(chan), &sag_server,
                            fidl::kIgnoreBindingClosure);
        return ZX_OK;
      });

  fbl::RefPtr<fs::PseudoDir> svcs_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  svcs_dir->AddEntry("fuchsia.power.system.ActivityGovernor", sag);
  fs::SynchronousVfs vfs(loop.dispatcher());

  fidl::Endpoints<fuchsia_io::Directory> dir_endpoints =
      fidl::CreateEndpoints<fuchsia_io::Directory>().value();
  vfs.ServeDirectory(std::move(svcs_dir), std::move(dir_endpoints.server));

  fuchsia_hardware_power::ParentElement parent = fuchsia_hardware_power::ParentElement::WithSag(
      fuchsia_hardware_power::SagElement::kExecutionState);
  fuchsia_hardware_power::PowerLevel one = {{.level = 0, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 1, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 2, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  fuchsia_hardware_power::LevelTuple one_to_one = {{
      .child_level = 1,
      .parent_level = 1,
  }};
  fuchsia_hardware_power::LevelTuple three_to_two = {{
      .child_level = 3,
      .parent_level = 2,
  }};

  std::unordered_map<uint16_t, uint16_t> child_to_parent_levels{
      {one_to_one.child_level().value(), one_to_one.parent_level().value()},
      {three_to_two.child_level().value(), three_to_two.parent_level().value()}};

  fuchsia_hardware_power::PowerDependency power_dep = {{
      .child = "n/a",
      .parent = parent,
      .level_deps = {{one_to_one, three_to_two}},
      .strength = fuchsia_hardware_power::RequirementType::kPassive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {{power_dep}}}};

  fidl::Arena test;
  fit::result<fdf_power::Error, fdf_power::TokenMap> call_result = fdf_power::GetDependencyTokens(
      fidl::ToWire(test, df_config), std::move(dir_endpoints.client));

  EXPECT_TRUE(call_result.is_ok());
  loop.Shutdown();
  loop.JoinThreads();

  fdf_power::TokenMap map = std::move(call_result.value());
  EXPECT_EQ(size_t(1), map.size());
  zx_info_handle_basic_t info1, info2;
  exec_passive.get_info(ZX_INFO_HANDLE_BASIC, &info1, sizeof(zx_info_handle_basic_t), nullptr,
                        nullptr);
  map.at(parent).get_info(ZX_INFO_HANDLE_BASIC, &info2, sizeof(zx_info_handle_basic_t), nullptr,
                          nullptr);
  EXPECT_EQ(info1.koid, info2.koid);
}

/// Test GetTokens when a power element has dependencies on driver/named power
/// elements and SAG power elements.
TEST_F(PowerLibTest, TestDriverAndSagElements) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread();

  zx::event exec_passive, wake_active;
  zx::event::create(0, &exec_passive);
  zx::event::create(0, &wake_active);
  zx::event exec_passive_dupe, wake_active_dupe;
  exec_passive.duplicate(ZX_RIGHT_SAME_RIGHTS, &exec_passive_dupe);
  wake_active.duplicate(ZX_RIGHT_SAME_RIGHTS, &wake_active_dupe);

  SystemActivityGovernor sag_server(std::move(exec_passive_dupe), std::move(wake_active_dupe));

  fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor> bindings;
  fbl::RefPtr<fs::Service> sag = fbl::MakeRefCounted<fs::Service>(
      [&](fidl::ServerEnd<fuchsia_power_system::ActivityGovernor> chan) {
        bindings.AddBinding(loop.dispatcher(), std::move(chan), &sag_server,
                            fidl::kIgnoreBindingClosure);
        return ZX_OK;
      });

  fbl::RefPtr<fs::PseudoDir> svcs_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  svcs_dir->AddEntry("fuchsia.power.system.ActivityGovernor", sag);
  fs::SynchronousVfs vfs(loop.dispatcher());

  // Now let's add dependencies on non-SAG elements
  std::string driver_parent_name = "driver_parent";
  fuchsia_hardware_power::ParentElement driver_parent =
      fuchsia_hardware_power::ParentElement::WithName(driver_parent_name);

  fidl::ServerBindingGroup<fuchsia_hardware_power::PowerTokenProvider> token_service_bindings;
  // Service dir which contains the service instances
  fbl::RefPtr<fs::PseudoDir> power_token_service = fbl::MakeRefCounted<fs::PseudoDir>();
  AddInstanceResult instance_data =
      AddServiceInstance(driver_parent_name, loop.dispatcher(), &token_service_bindings);
  ASSERT_EQ(ZX_OK, power_token_service->AddEntry("one", instance_data.service_instance));
  svcs_dir->AddEntry(fuchsia_hardware_power::PowerTokenService::Name, power_token_service);

  fidl::Endpoints<fuchsia_io::Directory> dir_endpoints =
      fidl::CreateEndpoints<fuchsia_io::Directory>().value();

  vfs.ServeDirectory(std::move(svcs_dir), std::move(dir_endpoints.server));

  fuchsia_hardware_power::ParentElement parent = fuchsia_hardware_power::ParentElement::WithSag(
      fuchsia_hardware_power::SagElement::kExecutionState);
  fuchsia_hardware_power::PowerLevel one = {{.level = 0, .name = "one", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel two = {{.level = 1, .name = "two", .transitions = {}}};
  fuchsia_hardware_power::PowerLevel three = {{.level = 2, .name = "three", .transitions = {}}};

  fuchsia_hardware_power::PowerElement pe = {{
      .name = "the_element",
      .levels = {{one, two, three}},
  }};

  fuchsia_hardware_power::LevelTuple one_to_one = {{
      .child_level = 1,
      .parent_level = 1,
  }};
  fuchsia_hardware_power::LevelTuple three_to_two = {{
      .child_level = 3,
      .parent_level = 2,
  }};

  fuchsia_hardware_power::PowerDependency power_dep = {{
      .child = "n/a",
      .parent = parent,
      .level_deps = {{one_to_one, three_to_two}},
      .strength = fuchsia_hardware_power::RequirementType::kPassive,
  }};

  fuchsia_hardware_power::LevelTuple two_to_two = {{
      .child_level = 2,
      .parent_level = 2,
  }};

  fuchsia_hardware_power::PowerDependency driver_power_dep = {{
      .child = "n/a",
      .parent = driver_parent,
      .level_deps = {{two_to_two}},
      .strength = fuchsia_hardware_power::RequirementType::kPassive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration df_config = {
      {.element = pe, .dependencies = {{power_dep, driver_power_dep}}}};

  fidl::Arena test;
  fit::result<fdf_power::Error, fdf_power::TokenMap> call_result = fdf_power::GetDependencyTokens(
      fidl::ToWire(test, df_config), std::move(dir_endpoints.client));

  EXPECT_TRUE(call_result.is_ok());

  loop.Shutdown();
  loop.JoinThreads();

  fdf_power::TokenMap map = std::move(call_result.value());
  EXPECT_EQ(size_t(2), map.size());
  zx_info_handle_basic_t info1, info2;
  exec_passive.get_info(ZX_INFO_HANDLE_BASIC, &info1, sizeof(zx_info_handle_basic_t), nullptr,
                        nullptr);
  map.at(parent).get_info(ZX_INFO_HANDLE_BASIC, &info2, sizeof(zx_info_handle_basic_t), nullptr,
                          nullptr);
  EXPECT_EQ(info1.koid, info2.koid);

  instance_data.token->get_info(ZX_INFO_HANDLE_BASIC, &info1, sizeof(zx_info_handle_basic_t),
                                nullptr, nullptr);
  map.at(driver_parent)
      .get_info(ZX_INFO_HANDLE_BASIC, &info2, sizeof(zx_info_handle_basic_t), nullptr, nullptr);
  EXPECT_EQ(info1.koid, info2.koid);
}

// TODO(https://fxbug.dev/328527466) This dependency is invalid because it has
// no level deps add a test that checks we return a proper error
}  // namespace power_lib_test
