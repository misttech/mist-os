// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.component.test/cpp/wire.h>
#include <fidl/fuchsia.driver.test/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>

int main() {
  fuchsia_logging::SetTags({"platform_driver_test_realm"});

  auto client_end = component::Connect<fuchsia_driver_test::Realm>();
  if (!client_end.is_ok()) {
    FX_LOG_KV(ERROR, "Failed to connect to Realm FIDL", FX_KV("error", client_end.error_value()));
    return 1;
  }
  fidl::WireSyncClient client{std::move(*client_end)};

  fidl::Arena arena;
  fuchsia_driver_test::wire::RealmArgs args(arena);
  args.set_root_driver(arena,
                       fidl::StringView("fuchsia-boot:///platform-bus#meta/platform-bus.cm"));

  auto expose = fuchsia_component_test::wire::Capability::WithService(
      arena, fuchsia_component_test::wire::Service::Builder(arena)
                 .name("fuchsia.hardware.ramdisk.Service")
                 .Build());
  args.set_dtr_exposes(
      arena, fidl::VectorView<fuchsia_component_test::wire::Capability>::FromExternal(&expose, 1));

  auto wire_result = client->Start(std::move(args));
  if (wire_result.status() != ZX_OK) {
    FX_LOG_KV(ERROR, "Failed to call to Realm:Start", FX_KV("status", wire_result.status()));
    return 1;
  }
  if (wire_result->is_error()) {
    FX_LOG_KV(ERROR, "Realm:Start failed", FX_KV("error", wire_result->error_value()));
    return 1;
  }

  return 0;
}
