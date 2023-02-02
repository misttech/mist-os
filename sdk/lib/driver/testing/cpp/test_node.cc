// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/test_node.h>

namespace fdf_testing {

zx::result<fidl::ClientEnd<fuchsia_driver_framework::Node>> TestNode::CreateNodeChannel() {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }

  if (zx::result result = Serve(std::move(endpoints->server)); result.is_error()) {
    return result.take_error();
  }

  return zx::ok(std::move(endpoints->client));
}

zx::result<> TestNode::Serve(fidl::ServerEnd<fuchsia_driver_framework::Node> server_end) {
  if (node_binding_.has_value()) {
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }

  node_binding_.emplace(dispatcher_, std::move(server_end), this,
                        [this](fidl::UnbindInfo) { Remove(); });
  return zx::ok();
}

void TestNode::AddChild(AddChildRequestView request, AddChildCompleter::Sync& completer) {
  std::string name{request->args.name().get()};
  auto [it, inserted] = children_.try_emplace(name, dispatcher_, name);
  if (!inserted) {
    completer.ReplyError(fuchsia_driver_framework::NodeError::kNameAlreadyExists);
    return;
  }
  TestNode& node = it->second;
  node.parent_ = *this;
  node.controller_binding_.emplace(dispatcher_, std::move(request->controller), &node,
                                   fidl::kIgnoreBindingClosure);
  if (request->node) {
    node.node_binding_.emplace(dispatcher_, std::move(request->node), &node,
                               [this](fidl::UnbindInfo) { Remove(); });
  }

  completer.ReplySuccess();
}

void TestNode::Remove() {
  children_.clear();
  node_binding_.reset();
  controller_binding_.reset();

  if (!parent_.has_value()) {
    return;
  }
  // After this call we are destructed, so don't access anything else.
  size_t count = parent_.value().get().children_.erase(name_);
  ZX_ASSERT_MSG(count == 1, "Should've removed 1 child, removed %ld", count);
}

}  // namespace fdf_testing
