// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "unowned_component.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>

#include "component.h"

zx::result<std::unique_ptr<profiler::Component>> profiler::UnownedComponent::Create(
    async_dispatcher_t* dispatcher, const std::string& moniker) {
  // Ensure the requested moniker exists
  zx::result<fidl::ClientEnd<fuchsia_sys2::RealmQuery>> client_end =
      component::Connect<fuchsia_sys2::RealmQuery>("/svc/fuchsia.sys2.RealmQuery.root");
  if (client_end.is_error()) {
    FX_LOGS(WARNING) << "Unable to connect to RealmQuery. Attaching by moniker isn't supported!";
    return client_end.take_error();
  }
  fidl::SyncClient realm_query_client{std::move(*client_end)};
  fidl::Result<::fuchsia_sys2::RealmQuery::GetInstance> result =
      realm_query_client->GetInstance(moniker);
  if (result.is_error()) {
    FX_LOGS(WARNING) << "Failed to find moniker: " << moniker << ". " << result.error_value();
    if (result.error_value().is_domain_error()) {
      return zx::error(ZX_ERR_BAD_PATH);
    } else {
      return zx::error(result.error_value().framework_error().status());
    }
  }

  if (!result->instance().url()) {
    return zx::error(ZX_ERR_BAD_PATH);
  }

  std::unique_ptr component = std::make_unique<UnownedComponent>(dispatcher);
  component->moniker_ = moniker;
  component->url_ = *result->instance().url();
  return zx::ok(std::move(component));
}

zx::result<> profiler::UnownedComponent::Start(ComponentWatcher::ComponentEventHandler on_start) {
  if (!on_start) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  on_start_ = std::move(on_start);

  zx::result<fidl::ClientEnd<fuchsia_sys2::RealmQuery>> client_end =
      component::Connect<fuchsia_sys2::RealmQuery>("/svc/fuchsia.sys2.RealmQuery.root");
  if (client_end.is_error()) {
    FX_LOGS(WARNING) << "Unable to connect to RealmQuery. Attaching by moniker isn't supported!";
    return client_end.take_error();
  }
  fidl::SyncClient realm_query_client{std::move(*client_end)};

  zx::result<> attach_to_children = TraverseRealm(
      moniker_,
      [this,
       realm_query_client = std::move(realm_query_client)](std::string moniker) -> zx::result<> {
        fidl::Result<::fuchsia_sys2::RealmQuery::GetInstance> result =
            realm_query_client->GetInstance(moniker);
        if (result.is_error()) {
          FX_LOGS(WARNING) << "Failed to find moniker: " << moniker << ". " << result.error_value();
          if (result.error_value().is_domain_error()) {
            return zx::error(ZX_ERR_BAD_PATH);
          } else {
            return zx::error(result.error_value().framework_error().status());
          }
        }
        if (!result->instance().url()) {
          return zx::error(ZX_ERR_BAD_PATH);
        }

        if (!result->instance().resolved_info() ||
            !result->instance().resolved_info()->execution_info()) {
          return component_watcher_.WatchForMoniker(
              moniker,
              [this](std::string moniker, std::string url) { on_start_.value()(moniker, url); });
        }
        on_start_.value()(moniker, *result->instance().url());
        return zx::ok();
      });
  if (attach_to_children.is_error()) {
    return attach_to_children;
  }
  return zx::ok();
}

zx::result<> profiler::UnownedComponent::Stop() { return zx::ok(); }

zx::result<> profiler::UnownedComponent::Destroy() { return zx::ok(); }
