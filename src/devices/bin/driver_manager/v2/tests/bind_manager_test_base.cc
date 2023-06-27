// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/tests/bind_manager_test_base.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/cpp/bind.h>

namespace fdi = fuchsia_driver_index;

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

void TestDriverIndex::MatchDriver(MatchDriverRequestView request,
                                  MatchDriverCompleter::Sync& completer) {
  std::optional<uint32_t> id;
  for (auto& property : request->args.properties()) {
    if (property.key.is_int_value() && property.key.int_value() == BIND_PLATFORM_DEV_INSTANCE_ID) {
      id = property.value.int_value();
    }
  }
  ASSERT_TRUE(id.has_value());
  match_request_count_++;
  completers_[id.value()].push(completer.ToAsync());
}

void TestDriverIndex::WaitForBaseDrivers(WaitForBaseDriversCompleter::Sync& completer) {
  completer.Reply();
}

void TestDriverIndex::AddCompositeNodeSpec(AddCompositeNodeSpecRequestView request,
                                           AddCompositeNodeSpecCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

zx::result<fidl::ClientEnd<fdi::DriverIndex>> TestDriverIndex::Connect() {
  auto endpoints = fidl::CreateEndpoints<fdi::DriverIndex>();
  if (endpoints.is_error()) {
    return zx::error(endpoints.status_value());
  }
  fidl::BindServer(dispatcher_, std::move(endpoints->server), this);
  return zx::ok(std::move(endpoints->client));
}

void TestDriverIndex::ReplyWithMatch(uint32_t id, zx::result<fdi::MatchedDriver> result) {
  ASSERT_FALSE(completers_[id].empty());
  auto completer = std::move(completers_[id].front());
  completers_[id].pop();
  match_request_count_--;

  if (result.is_error()) {
    completer.ReplyError(result.status_value());
    return;
  }
  fidl::Arena arena;
  completer.ReplySuccess(fidl::ToWire(arena, result.value()));
}

void TestDriverIndex::VerifyRequestCount(uint32_t id, size_t expected_count) {
  ASSERT_EQ(expected_count, completers_[id].size());
}

void TestBindManagerBridge::AddSpecToDriverIndex(
    fuchsia_driver_framework::wire::CompositeNodeSpec spec, AddToIndexCallback callback) {
  auto name = std::string(spec.name().get());
  auto response = fdi::DriverIndexAddCompositeNodeSpecResponse(
      fdi::MatchedCompositeInfo{{
          .composite_name = name,
          .driver_info = fdi::MatchedDriverInfo{{.driver_url = "fuchsia-boot:///#meta/test.cm"}},
      }},
      specs_.at(name).fidl_info.node_names().value());
  callback(zx::ok(response));
}

void TestBindManagerBridge::AddCompositeNodeSpec(std::string composite,
                                                 std::vector<std::string> parent_names,
                                                 std::vector<fdf::ParentSpec> parents,
                                                 std::unique_ptr<dfv2::CompositeNodeSpecV2> spec) {
  auto composite_info = fdi::MatchedCompositeInfo{{
      .composite_name = composite,
      .driver_info = fdi::MatchedDriverInfo{{.driver_url = "fuchsia-boot:///#meta/test.cm"}},
  }};
  auto fidl_spec_info = fdi::MatchedCompositeNodeSpecInfo{{
      .name = composite,
      .composite = composite_info,
      .num_nodes = parents.size(),
      .node_names = parent_names,
      .primary_index = 0,
  }};
  specs_.emplace(composite, CompositeNodeSpecData{
                                .spec = spec.get(),
                                .fidl_info = fidl_spec_info,
                            });

  fidl::Arena arena;
  auto fidl_data = fdf::CompositeNodeSpec{{.name = composite, .parents = std::move(parents)}};
  auto result = composite_manager_.AddSpec(fidl::ToWire(arena, fidl_data), std::move(spec));
  ASSERT_TRUE(result.is_ok());
}

void BindManagerTestBase::SetUp() {
  TestLoopFixture::SetUp();

  devfs_.emplace(root_devnode_);
  root_ = CreateNode("root", false);
  root_->AddToDevfsForTesting(root_devnode_.value());

  driver_index_ = std::make_unique<TestDriverIndex>(dispatcher());
  auto client = driver_index_->Connect();
  ASSERT_TRUE(client.is_ok());

  bridge_ = std::make_unique<TestBindManagerBridge>(
      fidl::WireClient<fdi::DriverIndex>(std::move(client.value()), dispatcher()));

  bind_manager_ = std::make_unique<TestBindManager>(bridge_.get(), &node_manager_, dispatcher());
  node_manager_.set_bind_manager(bind_manager_.get());
  bridge_->set_bind_manager(bind_manager_.get());

  ASSERT_EQ(0u, bind_manager_->NumOrphanedNodes());
  VerifyNoOngoingBind();
}

void BindManagerTestBase::TearDown() {
  nodes_.clear();
  TestLoopFixture::TearDown();
}

BindManagerTestBase::BindManagerData BindManagerTestBase::CurrentBindManagerData() const {
  return BindManagerTestBase::BindManagerData{
      .driver_index_request_count = driver_index_->NumOfMatchRequests(),
      .orphan_nodes_count = bind_manager_->NumOrphanedNodes(),
      .pending_bind_count = bind_manager_->GetPendingRequests().size(),
      .pending_orphan_rebind_count = bind_manager_->GetPendingOrphanRebindCallbacks().size(),
  };
}

void BindManagerTestBase::VerifyBindManagerData(BindManagerTestBase::BindManagerData expected) {
  ASSERT_EQ(expected.driver_index_request_count, driver_index_->NumOfMatchRequests());
  ASSERT_EQ(expected.orphan_nodes_count, bind_manager_->NumOrphanedNodes());
  ASSERT_EQ(expected.pending_bind_count, bind_manager_->GetPendingRequests().size());
  ASSERT_EQ(expected.pending_orphan_rebind_count,
            bind_manager_->GetPendingOrphanRebindCallbacks().size());
}

std::shared_ptr<dfv2::Node> BindManagerTestBase::CreateNode(const std::string name,
                                                            bool enable_multibind) {
  std::shared_ptr new_node =
      std::make_shared<dfv2::Node>(name, std::vector<dfv2::Node*>(), &node_manager_, dispatcher(),
                                   inspect_.CreateDevice(name, zx::vmo(), 0));
  new_node->AddToDevfsForTesting(root_devnode_.value());
  new_node->set_can_multibind_composites(enable_multibind);
  return new_node;
}

void BindManagerTestBase::AddAndBindNode(std::string name, bool enable_multibind) {
  // This function should only be called for a new node.
  ASSERT_EQ(nodes_.find(name), nodes_.end());

  auto node = CreateNode(name, enable_multibind);
  auto instance_id = GetOrAddInstanceId(name);
  node->set_properties({fdf::MakeProperty(arena_, BIND_PLATFORM_DEV_INSTANCE_ID, instance_id)});
  nodes_.emplace(name, node);
  InvokeBind(name);
}

// This function should only be called when there's no ongoing bind.
// Adds a new node and invoke Bind(). Then complete the bind request with
// no matches. The ongoing bind flag should reset to false and the node
// should be added in the orphaned nodes.
void BindManagerTestBase::AddAndOrphanNode(std::string name, bool enable_multibind) {
  VerifyNoOngoingBind();

  size_t current_orphan_count = bind_manager_->NumOrphanedNodes();

  // Invoke bind for a new node in the bind manager.
  AddAndBindNode(name, enable_multibind);
  ASSERT_TRUE(bind_manager_->IsBindOngoing());
  ASSERT_EQ(current_orphan_count, bind_manager_->NumOrphanedNodes());

  // Driver index completes the request with no matches for the node. The ongoing
  // bind flag should reset to false and the node should be added in the orphaned nodes.
  DriverIndexReplyWithNoMatch(name);
  VerifyNoOngoingBind();
  ASSERT_EQ(current_orphan_count + 1, bind_manager_->NumOrphanedNodes());
}

void BindManagerTestBase::InvokeBind(std::string name) {
  ASSERT_NE(nodes_.find(name), nodes_.end());
  auto tracker = std::make_shared<dfv2::BindResultTracker>(
      1, [](fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> info) {});
  bind_manager_->Bind(*nodes_[name], "", tracker);
  RunLoopUntilIdle();
}

void BindManagerTestBase::InvokeBind_EXPECT_BIND_START(std::string name) {
  VerifyNoOngoingBind();
  InvokeBind(name);
  ASSERT_TRUE(bind_manager_->IsBindOngoing());
}

void BindManagerTestBase::InvokeBind_EXPECT_QUEUED(std::string name) {
  auto expected_data = CurrentBindManagerData();
  expected_data.pending_bind_count += 1;
  InvokeBind(name);
  VerifyBindManagerData(expected_data);
}

void BindManagerTestBase::AddAndBindNode_EXPECT_BIND_START(std::string name,
                                                           bool enable_multibind) {
  VerifyNoOngoingBind();
  // Bind process should begin and send a match request to the Driver Index.
  AddAndBindNode(name, enable_multibind);
  ASSERT_TRUE(bind_manager_->IsBindOngoing());
}

void BindManagerTestBase::AddAndBindNode_EXPECT_QUEUED(std::string name, bool enable_multibind) {
  ASSERT_TRUE(bind_manager_->IsBindOngoing());
  auto expected_data = CurrentBindManagerData();
  expected_data.pending_bind_count += 1;

  // The bind request should be queued. There should be no new driver index MatchDriver
  // requests or orphaned nodes.
  AddAndBindNode(name, enable_multibind);
  ASSERT_TRUE(bind_manager_->IsBindOngoing());
  VerifyBindManagerData(expected_data);
}

void BindManagerTestBase::AddLegacyComposite(std::string composite,
                                             std::vector<std::string> fragment_names) {
  fuchsia_device_manager::CompositeDeviceDescriptor descriptor;
  for (auto& name : fragment_names) {
    fuchsia_device_manager::DeviceFragment fragment;
    fragment.name() = name;
    fragment.parts().emplace_back();
    fragment.parts()[0].match_program().emplace_back();
    fragment.parts()[0].match_program()[0] = fuchsia_device_manager::BindInstruction BI_MATCH_IF(
        EQ, BIND_PLATFORM_DEV_INSTANCE_ID, GetOrAddInstanceId(name));
    descriptor.fragments().push_back(fragment);
  }

  descriptor.props().emplace_back();
  descriptor.props()[0].id() = BIND_PLATFORM_DEV_INSTANCE_ID;
  descriptor.props()[0].value() = GetOrAddInstanceId(composite);

  bind_manager_->GetLegacyCompositeManager().AddCompositeDevice(composite, descriptor);
  RunLoopUntilIdle();
}

void BindManagerTestBase::AddLegacyComposite_EXPECT_QUEUED(
    std::string composite, std::vector<std::string> fragment_names) {
  ASSERT_TRUE(bind_manager_->IsBindOngoing());
  auto expected_data = CurrentBindManagerData();
  expected_data.pending_orphan_rebind_count += 1;
  AddLegacyComposite(composite, fragment_names);
  VerifyBindManagerData(expected_data);
}

void BindManagerTestBase::AddCompositeNodeSpec(std::string composite,
                                               std::vector<std::string> parents) {
  std::vector<fdf::ParentSpec> parent_specs;
  parent_specs.reserve(parents.size());
  for (auto& parent : parents) {
    auto instance_id = GetOrAddInstanceId(parent);
    parent_specs.push_back(fdf::ParentSpec{
        {.bind_rules = {fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID,
                                                instance_id)},
         .properties = {fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, instance_id)}}});
  }

  auto spec = std::make_unique<dfv2::CompositeNodeSpecV2>(
      CompositeNodeSpecCreateInfo{
          .name = composite,
          .size = parents.size(),
      },
      dispatcher(), &node_manager_);

  bridge_->AddCompositeNodeSpec(composite, std::move(parents), std::move(parent_specs),
                                std::move(spec));
  RunLoopUntilIdle();
}

void BindManagerTestBase::InvokeTryBindAllAvailable() {
  bind_manager_->TryBindAllAvailable();
  RunLoopUntilIdle();
}

void BindManagerTestBase::InvokeTryBindAllAvailable_EXPECT_BIND_START() {
  VerifyNoOngoingBind();
  InvokeTryBindAllAvailable();
  ASSERT_TRUE(bind_manager_->IsBindOngoing());
}

void BindManagerTestBase::InvokeTryBindAllAvailable_EXPECT_QUEUED() {
  ASSERT_TRUE(bind_manager_->IsBindOngoing());

  auto expected_data = CurrentBindManagerData();
  expected_data.pending_orphan_rebind_count += 1;

  InvokeTryBindAllAvailable();
  ASSERT_TRUE(bind_manager_->IsBindOngoing());
  VerifyBindManagerData(expected_data);
}

void BindManagerTestBase::DriverIndexReplyWithDriver(std::string node) {
  ASSERT_NE(instance_ids_.find(node), instance_ids_.end());
  auto driver_info = fdi::MatchedDriverInfo{{.driver_url = "fuchsia-boot:///#meta/test.cm"}};
  driver_index_->ReplyWithMatch(instance_ids_[node],
                                zx::ok(fdi::MatchedDriver::WithDriver(driver_info)));
  RunLoopUntilIdle();
}

void BindManagerTestBase::DriverIndexReplyWithComposite(
    std::string node, std::vector<std::pair<std::string, size_t>> matched_specs) {
  std::vector<fdi::MatchedCompositeNodeSpecInfo> fidl_specs;
  fidl_specs.reserve(matched_specs.size());
  for (auto& [name, index] : matched_specs) {
    auto match_info = bridge_->specs().at(name).fidl_info;
    match_info.node_index() = index;
    fidl_specs.push_back(match_info);
  }

  driver_index_->ReplyWithMatch(
      instance_ids_[node],
      zx::ok(fdi::MatchedDriver::WithParentSpec(
          fdi::MatchedCompositeNodeParentInfo{{.specs = std::move(fidl_specs)}})));
  RunLoopUntilIdle();
}

void BindManagerTestBase::DriverIndexReplyWithNoMatch(std::string node) {
  ASSERT_NE(instance_ids_.find(node), instance_ids_.end());
  driver_index_->ReplyWithMatch(instance_ids_[node], zx::error(ZX_ERR_NOT_FOUND));
  RunLoopUntilIdle();
}

void BindManagerTestBase::VerifyNoOngoingBind() {
  ASSERT_EQ(false, bind_manager_->IsBindOngoing());
  ASSERT_TRUE(bind_manager_->GetPendingRequests().empty());
  ASSERT_TRUE(bind_manager_->GetPendingOrphanRebindCallbacks().empty());
}

void BindManagerTestBase::VerifyNoQueuedBind() {
  ASSERT_TRUE(bind_manager_->GetPendingRequests().empty());
  ASSERT_TRUE(bind_manager_->GetPendingOrphanRebindCallbacks().empty());
}

void BindManagerTestBase::VerifyOrphanedNodes(std::vector<std::string> expected_nodes) {
  ASSERT_EQ(expected_nodes.size(), bind_manager_->NumOrphanedNodes());
  for (const auto& node : expected_nodes) {
    ASSERT_NE(bind_manager_->GetOrphanedNodes().find(node),
              bind_manager_->GetOrphanedNodes().end());
  }
}

void BindManagerTestBase::VerifyBindOngoingWithRequests(
    std::vector<std::pair<std::string, size_t>> expected_requests) {
  ASSERT_TRUE(bind_manager_->IsBindOngoing());
  size_t expected_count = 0;
  for (auto& [name, count] : expected_requests) {
    driver_index_->VerifyRequestCount(GetOrAddInstanceId(name), count);
    expected_count += count;
  }
  ASSERT_EQ(expected_count, driver_index_->NumOfMatchRequests());
}

void BindManagerTestBase::VerifyPendingBindRequestCount(size_t expected) {
  ASSERT_EQ(expected, bind_manager_->GetPendingRequests().size());
}

void BindManagerTestBase::VerifyLegacyCompositeFragmentIsBound(bool expect_bound,
                                                               std::string composite,
                                                               std::string fragment_name) {
  auto& assemblers = bind_manager_->GetLegacyCompositeManager().assemblers();
  auto composite_itr =
      std::find_if(assemblers.begin(), assemblers.end(),
                   [&composite](const auto& it) { return it->name() == composite; });
  ASSERT_NE(composite_itr, assemblers.end());

  auto& fragments = (*composite_itr)->fragments();
  auto fragment_itr =
      std::find_if(fragments.begin(), fragments.end(),
                   [&fragment_name](const auto& it) { return it.name() == fragment_name; });
  ASSERT_NE(fragment_itr, fragments.end());
  EXPECT_EQ(expect_bound, fragment_itr->bound_node() != nullptr);
}

void BindManagerTestBase::VerifyLegacyCompositeBuilt(bool expect_built, std::string composite) {
  auto& assemblers = bind_manager_->GetLegacyCompositeManager().assemblers();
  auto composite_itr =
      std::find_if(assemblers.begin(), assemblers.end(),
                   [&composite](const auto& it) { return it->name() == composite; });
  ASSERT_NE(composite_itr, assemblers.end());
  EXPECT_EQ(expect_built, (*composite_itr)->is_assembled());
}

void BindManagerTestBase::VerifyCompositeNodeExists(bool expected, std::string spec_name) {
  EXPECT_EQ(expected,
            bridge_->specs().at(spec_name).spec->completed_composite_node() != std::nullopt);
}

uint32_t BindManagerTestBase::GetOrAddInstanceId(std::string node_name) {
  if (instance_ids_.find(node_name) != instance_ids_.end()) {
    return instance_ids_[node_name];
  }

  uint32_t instance_id = static_cast<uint32_t>(instance_ids_.size());
  instance_ids_[node_name] = instance_id;
  return instance_id;
}

TEST_F(BindManagerTestBase, TestAddNode) {
  AddAndOrphanNode("test-1");
  ASSERT_EQ(1u, nodes().size());
  ASSERT_EQ(1u, instance_ids().size());

  auto test_node_1 = nodes()["test-1"];
  ASSERT_TRUE(test_node_1);
  ASSERT_EQ(1u, test_node_1->properties().size());
  ASSERT_EQ(static_cast<uint32_t>(BIND_PLATFORM_DEV_INSTANCE_ID),
            test_node_1->properties()[0].key.int_value());
  ASSERT_EQ(static_cast<uint32_t>(0), test_node_1->properties()[0].value.int_value());

  AddAndBindNode("test-2");
  ASSERT_EQ(2u, nodes().size());
  ASSERT_EQ(2u, instance_ids().size());

  auto test_node_2 = nodes()["test-2"];
  ASSERT_TRUE(test_node_2);
  ASSERT_EQ(1u, test_node_2->properties().size());
  ASSERT_EQ(static_cast<uint32_t>(BIND_PLATFORM_DEV_INSTANCE_ID),
            test_node_2->properties()[0].key.int_value());
  ASSERT_EQ(static_cast<uint32_t>(1), test_node_2->properties()[0].value.int_value());

  // Complete the outstanding request.
  DriverIndexReplyWithNoMatch("test-2");
}
