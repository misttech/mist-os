// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.component/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>

/*
 * The startup component exists to start the `appmgr` component that exists
 * inside the `core` realm. This arrangement solves the problem that in some
 * builds `appmgr` is not included. It would be more straightforward to have
 * `appmgr` in a direct `eager` lineage from the root component, but that would
 * cause the system to crash in product configurations that don't include
 * `appmgr` because failure to start an `eager` child is fatal to a parent.
 * startup is not a child of the `core` component because that component is
 * itself stored in pkgfs, which may not be included in the build.
 *
 * NOTE: this component also starts `session_manager` for the same reasons
 * stated above.
 *
 * startup works by using a capability routed to it from `appmgr`. startup
 * connects to this capability, tries to send a request, and exits. Success or
 * failure of the request is irrelevant, startup is just making the component
 * manager resolve and start `appmgr`, if it is present.
 */

namespace {
void start_core() {
  zx::result client_end =
      component::Connect<fuchsia_component::Binder>("/svc/fuchsia.component.CoreBinder");
  std::ignore = client_end.status_value();
}
}  // namespace

int main() { start_core(); }
