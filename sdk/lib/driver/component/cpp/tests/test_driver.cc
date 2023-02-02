// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/tests/test_driver.h>

zx::result<> TestDriver::Start() {
  node_client_.Bind(std::move(node()), dispatcher());
  return zx::ok();
}

void TestDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
  // Delay the completion to simulate an async workload.
  async::PostDelayedTask(
      dispatcher(), [completer = std::move(completer)]() mutable { completer(zx::ok()); },
      zx::sec(1));
}

void TestDriver::CreateChildNodeSync() {
  auto node_controller = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT(node_controller.is_ok());

  fidl::Arena arena;
  auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, "child").Build();
  auto result = node_client_.sync()->AddChild(args, std::move(node_controller->server), {});
  ZX_ASSERT(result.ok());
  ZX_ASSERT(result->is_ok());
  sync_added_child_ = true;
}

void TestDriver::CreateChildNodeAsync() {
  auto node_controller = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT(node_controller.is_ok());
  fidl::Arena arena;
  auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, "child").Build();

  node_client_->AddChild(args, std::move(node_controller->server), {})
      .Then([this](fidl::WireUnownedResult<fuchsia_driver_framework::Node::AddChild>& result) {
        ZX_ASSERT_MSG(result.ok(), "%s", result.FormatDescription().c_str());
        ZX_ASSERT_MSG(result->is_ok(), "%s", result.FormatDescription().c_str());
        async_added_child_ = true;
      });
}

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<TestDriver>);
