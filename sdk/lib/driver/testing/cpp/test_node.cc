// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/test_node.h>

namespace fdf_testing {

TestNode::TestNode(async_dispatcher_t* dispatcher, std::string name)
    : dispatcher_(dispatcher),
      name_(std::move(name)),
      checker_(dispatcher, "|fdf_testing::TestNode| is thread-unsafe.") {}

TestNode::~TestNode() { std::lock_guard guard(checker_); }

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
  std::lock_guard guard(checker_);
  if (node_binding_.has_value()) {
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }

  node_binding_.emplace(dispatcher_, std::move(server_end), this,
                        [this](fidl::UnbindInfo) { RemoveFromParent(); });
  return zx::ok();
}

void TestNode::AddChild(AddChildRequestView request, AddChildCompleter::Sync& completer) {
  std::lock_guard guard(checker_);
  std::string name{request->args.name().get()};
  auto [it, inserted] = children_.try_emplace(name, dispatcher_, name);
  if (!inserted) {
    completer.ReplyError(fuchsia_driver_framework::NodeError::kNameAlreadyExists);
    return;
  }
  TestNode& node = it->second;
  node.SetParent(this, std::move(request->controller));
  if (request->node) {
    zx::result result = node.Serve(std::move(request->node));
    ZX_ASSERT_MSG(result.is_ok(), "|Serve| failed with %s", result.status_string());
  }

  completer.ReplySuccess();
}

void TestNode::SetParent(TestNode* parent,
                         fidl::ServerEnd<fuchsia_driver_framework::NodeController> controller) {
  std::lock_guard guard(checker_);
  parent_ = *parent;
  controller_binding_.emplace(dispatcher_, std::move(controller), this,
                              fidl::kIgnoreBindingClosure);
}

void TestNode::RemoveFromParent() {
  std::lock_guard guard(checker_);
  children_.clear();
  node_binding_.reset();
  controller_binding_.reset();

  if (!parent_.has_value()) {
    return;
  }
  // After this call we are destructed, so don't access anything else.
  parent_.value().get().RemoveChild(name_);
}

void TestNode::RemoveChild(const std::string& name) {
  std::lock_guard guard(checker_);
  size_t count = children_.erase(name);
  ZX_ASSERT_MSG(count == 1, "Should've removed 1 child, removed %ld", count);
}

}  // namespace fdf_testing
