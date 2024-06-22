// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "unowned_component.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>

#include "component.h"

zx::result<std::unique_ptr<profiler::Component>> profiler::UnownedComponent::Create(
    async_dispatcher_t* dispatcher, const std::optional<std::string>& moniker,
    const std::optional<std::string>& url) {
  std::unique_ptr component = std::make_unique<UnownedComponent>(dispatcher);
  component->moniker_ = moniker.value_or("");
  component->url_ = url.value_or("");
  component->needs_destruction_ = false;
  return zx::ok(std::move(component));
}

zx::result<> profiler::UnownedComponent::Attach(
    const fidl::SyncClient<fuchsia_sys2::RealmQuery>& client, std::string moniker) {
  fidl::Result<fuchsia_sys2::RealmQuery::GetInstance> result = client->GetInstance(moniker);
  if (result.is_error()) {
    FX_LOGS(WARNING) << "Failed to find moniker: " << moniker << ". " << result.error_value();
    return result.error_value().is_domain_error()
               ? zx::error(ZX_ERR_BAD_PATH)
               : zx::error(result.error_value().framework_error().status());
  }
  if (!result->instance().url()) {
    return zx::error(ZX_ERR_BAD_PATH);
  }

  if (!result->instance().resolved_info() ||
      !result->instance().resolved_info()->execution_info()) {
    return component_watcher_.WatchForMoniker(
        moniker, [this](std::string moniker, std::string url) {
          on_start_.value()(std::move(moniker), std::move(url));
        });
  }
  on_start_.value()(moniker, *result->instance().url());
  return zx::ok();
}

zx::result<> profiler::UnownedComponent::Start(ComponentWatcher::ComponentEventHandler on_start) {
  if (!on_start) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  on_start_ = std::move(on_start);
  if (zx::result res = component_watcher_.Watch(); res.is_error()) {
    return res;
  }

  zx::result<fidl::ClientEnd<fuchsia_sys2::RealmQuery>> client_end =
      component::Connect<fuchsia_sys2::RealmQuery>("/svc/fuchsia.sys2.RealmQuery.root");
  if (client_end.is_error()) {
    FX_LOGS(WARNING) << "Unable to connect to RealmQuery. Attaching to components isn't supported!";
    return client_end.take_error();
  }
  fidl::SyncClient realm_query_client{std::move(*client_end)};

  // If we don't have a moniker specified, that means we're waiting for a component with a matching
  // url to show up.
  if (moniker_.empty()) {
    FX_DCHECK(!url_.empty());  // This should have been checked earlier in configuration
    return component_watcher_.WatchForUrl(url_, [this](std::string moniker, std::string url) {
      on_start_.value()(std::move(moniker), std::move(url));
    });
  }

  // If the component exists, we'll attach to it, if not, we'll need to wait for it to launch
  fidl::Result<fuchsia_sys2::RealmQuery::GetInstance> result =
      realm_query_client->GetInstance(moniker_);
  if (result.is_error()) {
    if (result.error_value().is_domain_error() &&
        result.error_value().domain_error() == fuchsia_sys2::GetInstanceError::kInstanceNotFound) {
      FX_LOGS(INFO) << "Watching for: " << moniker_;
      return component_watcher_.WatchForMoniker(
          moniker_, [this, realm_query_client = std::move(realm_query_client)](
                        const std::string& moniker, const std::string& url) mutable {
            auto res = TraverseRealm(moniker_, [this, client = std::move(realm_query_client)](
                                                   const std::string& moniker) {
              return this->Attach(client, moniker);
            });
            if (res.is_error()) {
              FX_PLOGS(ERROR, res.status_value())
                  << "Failed to recursively attach to moniker: " << moniker;
            }
          });
    }
    FX_LOGS(INFO) << "Invalid moniker: " << moniker_;
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return TraverseRealm(moniker_,
                       [this, client = std::move(realm_query_client)](const std::string& moniker) {
                         return this->Attach(client, moniker);
                       });
}

zx::result<> profiler::UnownedComponent::Stop() { return component_watcher_.Reset(); }

zx::result<> profiler::UnownedComponent::Destroy() { return zx::ok(); }
