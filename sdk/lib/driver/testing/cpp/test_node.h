// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_TEST_NODE_H_
#define LIB_DRIVER_TESTING_CPP_TEST_NODE_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>

namespace fdf_testing {

class TestNode : public fidl::WireServer<fuchsia_driver_framework::NodeController>,
                 public fidl::WireServer<fuchsia_driver_framework::Node> {
 public:
  using ChildrenMap = std::unordered_map<std::string, TestNode>;

  TestNode(async_dispatcher_t* dispatcher, std::string name)
      : dispatcher_(dispatcher), name_(std::move(name)) {}

  ChildrenMap& children() { return children_; }
  const std::string& name() const { return name_; }

  zx::result<fidl::ClientEnd<fuchsia_driver_framework::Node>> CreateNodeChannel();

  bool HasNode() { return node_binding_.has_value(); }

 private:
  void AddChild(AddChildRequestView request, AddChildCompleter::Sync& completer) override;

  void Remove(RemoveCompleter::Sync& completer) override { Remove(); }

  void Remove();

  std::optional<fidl::ServerBinding<fuchsia_driver_framework::Node>> node_binding_;
  std::optional<fidl::ServerBinding<fuchsia_driver_framework::NodeController>> controller_binding_;

  async_dispatcher_t* dispatcher_;
  std::string name_;
  std::optional<std::reference_wrapper<TestNode>> parent_;
  ChildrenMap children_;
};

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_TEST_NODE_H_
