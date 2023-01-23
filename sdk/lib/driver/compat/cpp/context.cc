// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/compat/cpp/context.h>

namespace compat {

void Context::ConnectAndCreate(fdf::DriverContext* driver_context, async_dispatcher_t* dispatcher,
                               fit::callback<void(zx::result<std::unique_ptr<Context>>)> callback) {
  auto context = std::make_unique<Context>();

  // Connect to our parent.
  auto result = component::ConnectAtMember<fuchsia_driver_compat::Service::Device>(
      driver_context->incoming()->svc_dir());
  if (result.is_error()) {
    return callback(result.take_error());
  }
  context->parent_device_.Bind(std::move(result.value()), dispatcher);

  // Get the topological path.
  auto context_ptr = context.get();
  context_ptr->parent_device_->GetTopologicalPath().Then(
      [context = std::move(context), callback = std::move(callback)](auto& result) mutable {
        context->parent_topological_path_ = std::move(result->path());
        callback(zx::ok(std::move(context)));
      });
}

std::string Context::TopologicalPath(std::string_view relative_child_path) const {
  std::string path = parent_topological_path_;
  path.append("/").append(relative_child_path);
  return path;
}

}  // namespace compat
