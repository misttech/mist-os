// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "component.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>

#include <string>
#include <vector>

#include "unowned_component.h"

namespace {
// Reads a manifest from a |ManifestBytesIterator| producing a vector of bytes.
zx::result<std::vector<uint8_t>> DrainManifestBytesIterator(
    fidl::ClientEnd<fuchsia_sys2::ManifestBytesIterator> iterator_client_end) {
  fidl::SyncClient iterator(std::move(iterator_client_end));
  std::vector<uint8_t> result;

  while (true) {
    auto next_res = iterator->Next();
    if (next_res.is_error()) {
      return zx::error(next_res.error_value().status());
    }

    if (next_res->infos().empty()) {
      break;
    }

    result.insert(result.end(), next_res->infos().begin(), next_res->infos().end());
  }

  return zx::ok(std::move(result));
}

}  // namespace

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

  fidl::Result<fuchsia_sys2::LifecycleController::ResolveInstance> resolve_res =
      component->lifecycle_controller_client_->ResolveInstance({{component->moniker_}});

  if (resolve_res.is_error()) {
    FX_LOGS(ERROR) << "Failed to resolve " << moniker << ": " << resolve_res.error_value();
    return zx::error(ZX_ERR_BAD_STATE);
  }

  return zx::ok(std::move(component));
}

zx::result<> profiler::Component::TraverseRealm(
    std::string moniker, const fit::function<zx::result<>(const std::string&)>& f) {
  if (zx::result self_res = f(moniker); self_res.is_error()) {
    return self_res;
  }
  auto manifest = GetResolvedDeclaration(moniker);
  if (manifest.is_error()) {
    // If this instance isn't launched yet, that's okay. We'll register to be notified when it does
    // launch. Skip it for now.
    return zx::ok();
  }

  if (manifest->children()) {
    for (const auto& child : *manifest->children()) {
      if (!child.name()) {
        return zx::error(ZX_ERR_BAD_PATH);
      }
      std::string child_moniker = moniker + "/" + *child.name();
      if (zx::result descendents_res = TraverseRealm(child_moniker, f);
          descendents_res.is_error()) {
        return descendents_res;
      }
    }
  }
  return zx::ok();
}

zx::result<> profiler::Component::Start(ComponentWatcher::ComponentEventHandler on_start) {
  if (!on_start) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  on_start_ = std::move(on_start);
  zx::result<> watch_result = TraverseRealm(moniker_, [this](std::string moniker) {
    return component_watcher_.WatchForMoniker(
        moniker, [this](std::string moniker, std::string url) { on_start_.value()(moniker, url); });
  });

  if (watch_result.is_error()) {
    return watch_result;
  }

  if (zx::result res = component_watcher_.Watch(); res.is_error()) {
    return res;
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

zx::result<fidl::Box<fuchsia_component_decl::Component>>
profiler::Component::GetResolvedDeclaration(const std::string& moniker) {
  zx::result<fidl::ClientEnd<fuchsia_sys2::RealmQuery>> client_end =
      component::Connect<fuchsia_sys2::RealmQuery>("/svc/fuchsia.sys2.RealmQuery.root");
  if (client_end.is_error()) {
    FX_LOGS(WARNING) << "Unable to connect to RealmQuery. Component interaction is disabled";
    return client_end.take_error();
  }
  fidl::SyncClient realm_query_client{std::move(*client_end)};
  fidl::Result<::fuchsia_sys2::RealmQuery::GetResolvedDeclaration> declaration =
      realm_query_client->GetResolvedDeclaration({moniker});

  if (declaration.is_error()) {
    return zx::error(ZX_ERR_BAD_PATH);
  }

  auto drain_res = DrainManifestBytesIterator(std::move(declaration->iterator()));
  if (drain_res.is_error()) {
    return fit::error(drain_res.error_value());
  }

  auto unpersist_res = fidl::InplaceUnpersist<fuchsia_component_decl::wire::Component>(
      cpp20::span<uint8_t>(drain_res->begin(), drain_res->size()));
  if (unpersist_res.is_error()) {
    return zx::error(unpersist_res.error_value().status());
  }
  return zx::ok(fidl::ToNatural(*unpersist_res));
}

profiler::Component::~Component() {
  if (!destroyed_) {
    (void)Destroy();
  }
}
