// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/node.h"

#include <fuchsia/component/cpp/fidl_test_base.h>
#include <fuchsia/io/cpp/fidl_test_base.h>

#include "src/devices/bin/driver_manager/v2/composite_node_spec_v2.h"
#include "src/devices/bin/driver_manager/v2/driver_host.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

class FakeDriverHost : public dfv2::DriverHost {
 public:
  using StartCallback = fit::callback<void(zx::result<>)>;
  void Start(fidl::ClientEnd<fuchsia_driver_framework::Node> client_end, std::string node_node,
             fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol> symbols,
             fuchsia_component_runner::wire::ComponentStartInfo start_info,
             fidl::ServerEnd<fuchsia_driver_host::Driver> driver, StartCallback cb) override {
    cb(zx::ok());
  }

  zx::result<uint64_t> GetProcessKoid() const override { return zx::error(ZX_ERR_NOT_SUPPORTED); }
};

class FakeNodeManager : public dfv2::NodeManager {
 public:
  void Bind(dfv2::Node& node, std::shared_ptr<dfv2::BindResultTracker> result_tracker) override {}

  zx::result<dfv2::DriverHost*> CreateDriverHost() override { return zx::ok(&driver_host_); }

  void DestroyDriverComponent(
      dfv2::Node& node,
      fit::callback<void(fidl::WireUnownedResult<fuchsia_component::Realm::DestroyChild>& result)>
          callback) override {}

 private:
  FakeDriverHost driver_host_;
};

class Dfv2NodeTest : public gtest::TestLoopFixture {
 public:
  void SetUp() override { TestLoopFixture::SetUp(); }

  std::shared_ptr<dfv2::Node> CreateNode(const char* name) {
    return std::make_shared<dfv2::Node>(name, std::vector<dfv2::Node*>(), &node_manager_,
                                        dispatcher(), inspect_.CreateDevice(name, zx::vmo(), 0));
  }

 private:
  InspectManager inspect_{dispatcher()};
  FakeNodeManager node_manager_;
};

TEST_F(Dfv2NodeTest, RemoveDuringFailedBind) {
  auto node = CreateNode("test");

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
  ASSERT_TRUE(node->has_driver_component());
  ASSERT_EQ(dfv2::NodeState::kRunning, node->node_state());

  node->Remove(dfv2::RemovalSet::kAll, nullptr);
  ASSERT_EQ(dfv2::NodeState::kWaitingOnDriver, node->node_state());

  node->CompleteBind(zx::error(ZX_ERR_NOT_FOUND));
  ASSERT_FALSE(node->has_driver_component());
  ASSERT_EQ(dfv2::NodeState::kStopping, node->node_state());
}
