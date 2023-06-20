// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/exceptions/handler/component_lookup.h"

#include <fuchsia/sys2/cpp/fidl.h>
#include <lib/fpromise/result.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include "src/developer/forensics/utils/errors.h"
#include "src/developer/forensics/utils/fidl_oneshot.h"

namespace forensics {
namespace exceptions {
namespace handler {
namespace {

::fpromise::promise<ComponentInfo> GetInfo(async_dispatcher_t* dispatcher,
                                           std::shared_ptr<sys::ServiceDirectory> services,
                                           zx::duration timeout, zx_koid_t thread_koid) {
  namespace sys = fuchsia::sys2;
  return OneShotCall<sys::CrashIntrospect, &sys::CrashIntrospect::FindComponentByThreadKoid>(
             dispatcher, services, timeout, thread_koid)
      .or_else([](Error& error) { return ::fpromise::error(); })
      .and_then([](const sys::CrashIntrospect_FindComponentByThreadKoid_Result& result)
                    -> ::fpromise::result<ComponentInfo> {
        if (result.is_err()) {
          // RESOURCE_NOT_FOUND most likely means a thread from a process outside a component,
          // which is not an error.
          if (result.err() != fuchsia::component::Error::RESOURCE_NOT_FOUND) {
            FX_LOGS(WARNING) << "Failed FindComponentByThreadKoid, error: "
                             << static_cast<int>(result.err());
          }
          return ::fpromise::error();
        }

        const sys::ComponentCrashInfo& info = result.response().info;
        std::string moniker = (info.has_moniker()) ? info.moniker() : "";
        if (!moniker.empty() && moniker[0] == '/') {
          moniker = moniker.substr(1);
        }
        return ::fpromise::ok(ComponentInfo{
            .url = (info.has_url()) ? info.url() : "",
            .realm_path = "",
            .moniker = moniker,
        });
      });
}

}  // namespace

::fpromise::promise<ComponentInfo> GetComponentInfo(async_dispatcher_t* dispatcher,
                                                    std::shared_ptr<sys::ServiceDirectory> services,
                                                    const zx::duration timeout,
                                                    zx_koid_t thread_koid) {
  auto get_info = GetInfo(dispatcher, services, timeout, thread_koid);
  return get_info.then([](::fpromise::result<ComponentInfo>& result)
                           -> ::fpromise::result<ComponentInfo> {
    if (result.is_error()) {
      FX_LOGS(INFO) << "Failed FindComponentByThreadKoid, crash will lack component attribution";
      return ::fpromise::error();
    }

    return ::fpromise::ok(result.take_value());
  });
}

}  // namespace handler
}  // namespace exceptions
}  // namespace forensics
