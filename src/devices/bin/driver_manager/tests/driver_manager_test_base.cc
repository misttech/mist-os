// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/tests/driver_manager_test_base.h"

void DriverManagerTestBase::SetUp() {
  TestLoopFixture::SetUp();
  devfs_ = std::make_unique<driver_manager::Devfs>(root_devnode_);
  root_ = CreateNode("root");
  root_->AddToDevfsForTesting(root_devnode_.value());
}

std::shared_ptr<driver_manager::Node> DriverManagerTestBase::CreateNode(const std::string name) {
  auto node = std::make_shared<driver_manager::Node>(
      name, std::vector<std::weak_ptr<driver_manager::Node>>(), GetNodeManager(), dispatcher(),
      inspect_.CreateDevice(name, 0));
  node->AddToDevfsForTesting(root_devnode_.value());
  node->devfs_device().publish();
  return node;
}

std::shared_ptr<driver_manager::Node> DriverManagerTestBase::CreateNode(
    const std::string name, std::weak_ptr<driver_manager::Node> parent) {
  std::vector<std::weak_ptr<driver_manager::Node>> parents = {parent};
  auto node = std::make_shared<driver_manager::Node>(name, parents, GetNodeManager(), dispatcher(),
                                                     inspect_.CreateDevice(name, 0));
  node->AddToDevfsForTesting(root_devnode_.value());
  node->devfs_device().publish();
  node->AddToParents();
  return node;
}

std::shared_ptr<driver_manager::Node> DriverManagerTestBase::CreateCompositeNode(
    std::string_view name, std::vector<std::weak_ptr<driver_manager::Node>> parents,
    const std::vector<fuchsia_driver_framework::NodePropertyEntry2>& parent_properties,
    uint32_t primary_index) {
  std::vector<std::string> parent_names;
  parent_names.reserve(parents.size());
  for (auto& parent : parents) {
    parent_names.push_back(parent.lock()->name());
  }
  return driver_manager::Node::CreateCompositeNode(name, parents, std::move(parent_names),
                                                   parent_properties, GetNodeManager(),
                                                   dispatcher(), primary_index)
      .value();
}
