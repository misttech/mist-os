// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.component/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>

/*
 * The startup component exists to start the `core` realm, by binding to it.
 * This arrangement solves the problem that in some builds (e.g., bringup)
 * `core` is not included. It would be more straightforward to have
 * `core` in a direct `eager` lineage from the root component, but that would
 * cause the system to crash in product configurations that don't include
 * `core` because failure to start an `eager` child is fatal to a parent.
 *
 * NOTE: this component also starts `session_manager` because it is an eager child
 * of `core`.
 */

namespace {
void start_core() {
  zx::result client_end =
      component::Connect<fuchsia_component::Binder>("/svc/fuchsia.component.CoreBinder");
  std::ignore = client_end.status_value();
}
}  // namespace

int main() { start_core(); }
