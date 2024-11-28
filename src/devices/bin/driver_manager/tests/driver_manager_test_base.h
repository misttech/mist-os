// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_TESTS_DRIVER_MANAGER_TEST_BASE_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_TESTS_DRIVER_MANAGER_TEST_BASE_H_

#include "src/devices/bin/driver_manager/node.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

class TestNodeManagerBase : public driver_manager::NodeManager {
 public:
  void Bind(driver_manager::Node& node,
            std::shared_ptr<driver_manager::BindResultTracker> result_tracker) override {}

  void DestroyDriverComponent(
      driver_manager::Node& node,
      fit::callback<void(fidl::WireUnownedResult<fuchsia_component::Realm::DestroyChild>& result)>
          callback) override {}

  zx::result<driver_manager::DriverHost*> CreateDriverHost(bool use_next_vdso) override {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
};

class DriverManagerTestBase : public gtest::TestLoopFixture {
 public:
  void SetUp() override;

  virtual driver_manager::NodeManager* GetNodeManager() = 0;

 protected:
  std::shared_ptr<driver_manager::Node> CreateNode(const std::string name);

  // Creates a DFv2 node and add it to the given parent.
  std::shared_ptr<driver_manager::Node> CreateNode(const std::string name,
                                                   std::weak_ptr<driver_manager::Node> parent);

  std::shared_ptr<driver_manager::Node> CreateCompositeNode(
      std::string_view name, std::vector<std::weak_ptr<driver_manager::Node>> parents,
      const std::vector<fuchsia_driver_framework::NodePropertyEntry2>& parent_properties,
      uint32_t primary_index = 0);

  std::shared_ptr<driver_manager::Node> root() const { return root_; }

  driver_manager::Devfs* devfs() const { return devfs_.get(); }

 private:
  driver_manager::InspectManager inspect_{dispatcher()};

  std::unique_ptr<driver_manager::Devfs> devfs_;
  std::shared_ptr<driver_manager::Node> root_;
  std::optional<driver_manager::Devnode> root_devnode_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_TESTS_DRIVER_MANAGER_TEST_BASE_H_
