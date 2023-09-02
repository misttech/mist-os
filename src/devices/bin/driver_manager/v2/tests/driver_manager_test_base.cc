// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/tests/driver_manager_test_base.h"

void DriverManagerTestBase::SetUp() {
  TestLoopFixture::SetUp();
  devfs_ = std::make_unique<Devfs>(root_devnode_);
  root_ = CreateNode("root");
  root_->AddToDevfsForTesting(root_devnode_.value());
}

std::shared_ptr<dfv2::Node> DriverManagerTestBase::CreateNode(const std::string name) {
  auto node =
      std::make_shared<dfv2::Node>(name, std::vector<std::weak_ptr<dfv2::Node>>(), GetNodeManager(),
                                   dispatcher(), inspect_.CreateDevice(name, zx::vmo(), 0));
  node->AddToDevfsForTesting(root_devnode_.value());
  node->devfs_device().publish();
  return node;
}

std::shared_ptr<dfv2::Node> DriverManagerTestBase::CreateCompositeNode(
    std::string_view name, std::vector<std::weak_ptr<dfv2::Node>> parents, bool is_legacy,
    uint32_t primary_index) {
  std::vector<std::string> parent_names;
  parent_names.reserve(parents.size());
  for (auto& parent : parents) {
    parent_names.push_back(parent.lock()->name());
  }
  return dfv2::Node::CreateCompositeNode(name, parents, std::move(parent_names), {},
                                         GetNodeManager(), dispatcher(), is_legacy, primary_index)
      .value();
}
