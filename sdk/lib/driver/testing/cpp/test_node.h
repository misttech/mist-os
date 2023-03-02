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

  TestNode(async_dispatcher_t* dispatcher, std::string name);

  ~TestNode() override;

  ChildrenMap& children() { return children_; }
  const std::string& name() const { return name_; }

  // Create a channel pair, serve the server end, and return the client end.
  // This method is thread-unsafe. Must be called from the same context as the dispatcher.
  zx::result<fidl::ClientEnd<fuchsia_driver_framework::Node>> CreateNodeChannel();

  // Serve the given server end.
  // This method is thread-unsafe. Must be called from the same context as the dispatcher.
  zx::result<> Serve(fidl::ServerEnd<fuchsia_driver_framework::Node> server_end);

  bool HasNode() {
    std::lock_guard guard(checker_);
    return node_binding_.has_value();
  }

  async_dispatcher_t* dispatcher() const { return dispatcher_; }

 private:
  void AddChild(AddChildRequestView request, AddChildCompleter::Sync& completer) override;

  void Remove(RemoveCompleter::Sync& completer) override { RemoveFromParent(); }

  void SetParent(TestNode* parent,
                 fidl::ServerEnd<fuchsia_driver_framework::NodeController> controller);

  void RemoveFromParent();

  void RemoveChild(const std::string& name);

  std::optional<fidl::ServerBinding<fuchsia_driver_framework::Node>> node_binding_
      __TA_GUARDED(checker_);
  std::optional<fidl::ServerBinding<fuchsia_driver_framework::NodeController>> controller_binding_
      __TA_GUARDED(checker_);

  async_dispatcher_t* dispatcher_;
  std::string name_ __TA_GUARDED(checker_);
  std::optional<std::reference_wrapper<TestNode>> parent_ __TA_GUARDED(checker_);
  ChildrenMap children_ __TA_GUARDED(checker_);
  async::synchronization_checker checker_;
};

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_TEST_NODE_H_
