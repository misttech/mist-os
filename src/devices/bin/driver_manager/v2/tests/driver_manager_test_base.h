// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V2_TESTS_DRIVER_MANAGER_TEST_BASE_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V2_TESTS_DRIVER_MANAGER_TEST_BASE_H_

#include "src/devices/bin/driver_manager/v2/node.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

class TestNodeManagerBase : public dfv2::NodeManager {
 public:
  void Bind(dfv2::Node& node, std::shared_ptr<dfv2::BindResultTracker> result_tracker) override {}

  void DestroyDriverComponent(
      dfv2::Node& node,
      fit::callback<void(fidl::WireUnownedResult<fuchsia_component::Realm::DestroyChild>& result)>
          callback) override {}

  zx::result<dfv2::DriverHost*> CreateDriverHost() override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
};

class DriverManagerTestBase : public gtest::TestLoopFixture {
 public:
  void SetUp() override;

  virtual dfv2::NodeManager* GetNodeManager() = 0;

 protected:
  std::shared_ptr<dfv2::Node> CreateNode(const std::string name);
  std::shared_ptr<dfv2::Node> CreateCompositeNode(std::string_view name,
                                                  std::vector<std::weak_ptr<dfv2::Node>> parents,
                                                  bool is_legacy, uint32_t primary_index = 0);

  std::shared_ptr<dfv2::Node> root() const { return root_; }

  Devfs* devfs() const { return devfs_.get(); }

 private:
  InspectManager inspect_{dispatcher()};

  std::unique_ptr<Devfs> devfs_;
  std::shared_ptr<dfv2::Node> root_;
  std::optional<Devnode> root_devnode_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V2_TESTS_DRIVER_MANAGER_TEST_BASE_H_
