// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "component.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>

namespace {
struct Moniker {
  std::string parent;
  std::string collection;
  std::string name;
};

zx::result<Moniker> SplitMoniker(const std::string& moniker) {
  // A valid moniker for launching in a dynamic collection looks like:
  // parent_moniker/collection:name
  size_t leaf_divider = moniker.find_last_of('/');
  if (leaf_divider == std::string::npos) {
    return zx::error(ZX_ERR_BAD_PATH);
  }

  std::string parent_moniker = moniker.substr(0, leaf_divider);
  const std::string leaf = moniker.substr(leaf_divider + 1);

  size_t collection_divider = leaf.find_last_of(':');
  if (collection_divider == std::string::npos) {
    return zx::error(ZX_ERR_BAD_PATH);
  }
  std::string collection = leaf.substr(0, collection_divider);
  std::string name = leaf.substr(collection_divider + 1);

  return zx::ok(Moniker{parent_moniker, collection, name});
}
}  // namespace

zx::result<std::unique_ptr<profiler::Component>> profiler::Component::Create(
    async_dispatcher_t* dispatcher, const std::string& url, const std::string& moniker) {
  std::unique_ptr component = std::make_unique<Component>(dispatcher);
  auto client_end = component::Connect<fuchsia_sys2::LifecycleController>(
      "/svc/fuchsia.sys2.LifecycleController.root");
  if (client_end.is_error()) {
    return client_end.take_error();
  }

  component->lifecycle_controller_client_ = fidl::SyncClient{std::move(*client_end)};
  zx::result split = SplitMoniker(moniker);
  if (split.is_error()) {
    split.take_error();
  }
  component->parent_moniker_ = split->parent;
  component->collection_ = split->collection;
  component->name_ = split->name;
  component->moniker_ = moniker;

  fidl::Result<fuchsia_sys2::LifecycleController::CreateInstance> create_res =
      component->lifecycle_controller_client_->CreateInstance({{
          .parent_moniker = component->parent_moniker_,
          .collection = {component->collection_},
          .decl = {{
              .name = component->name_,
              .url = url,
              .startup = fuchsia_component_decl::StartupMode::kLazy,
          }},
      }});

  if (create_res.is_error()) {
    FX_LOGS(ERROR) << "Failed to create  " << moniker << ": " << create_res.error_value();
    return zx::error(ZX_ERR_BAD_STATE);
  }
  return zx::ok(std::move(component));
}

zx::result<> profiler::Component::Start(ComponentWatcher::ComponentEventHandler on_start) {
  if (on_start) {
    if (zx::result<> watch_result =
            component_watcher_.WatchForMoniker(moniker_, std::move(on_start));
        watch_result.is_error()) {
      return watch_result;
    }
    if (zx::result res = component_watcher_.Watch(); res.is_error()) {
      return res;
    }
  }
  zx::result<fidl::Endpoints<fuchsia_component::Binder>> binder_endpoints =
      fidl::CreateEndpoints<fuchsia_component::Binder>();
  if (binder_endpoints.is_error()) {
    return binder_endpoints.take_error();
  }
  fidl::Result<fuchsia_sys2::LifecycleController::StartInstance> start_res =
      lifecycle_controller_client_->StartInstance({{
          .moniker = moniker_,
          .binder = std::move(binder_endpoints->server),
      }});

  if (start_res.is_error()) {
    FX_LOGS(ERROR) << "Failed to start component: " << start_res.error_value();
    return zx::error(ZX_ERR_UNAVAILABLE);
  }
  destroyed_ = false;
  return zx::ok();
}

zx::result<> profiler::Component::Stop() {
  if (auto stop_res = lifecycle_controller_client_->StopInstance({{
          .moniker = moniker_,
      }});
      stop_res.is_error()) {
    return zx::error(ZX_ERR_BAD_STATE);
  }
  return zx::ok();
}

zx::result<> profiler::Component::Destroy() {
  if (auto destroy_res = lifecycle_controller_client_->DestroyInstance({{
          .parent_moniker = parent_moniker_,
          .child = {{.name = name_, .collection = collection_}},
      }});
      destroy_res.is_error()) {
    return zx::error(ZX_ERR_BAD_STATE);
  }
  destroyed_ = true;
  return zx::ok();
}

profiler::Component::~Component() {
  if (!destroyed_) {
    (void)Destroy();
  }
}
