// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.test/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>

int main() {
  fuchsia_logging::SetTags({"platform_driver_test_realm"});

  auto client_end = component::Connect<fuchsia_driver_test::Realm>();
  if (!client_end.is_ok()) {
    FX_SLOG(ERROR, "Failed to connect to Realm FIDL", KV("error", client_end.error_value()));
    return 1;
  }
  fidl::WireSyncClient client{std::move(*client_end)};

  fidl::Arena arena;
  fuchsia_driver_test::wire::RealmArgs args(arena);
  args.set_root_driver(arena,
                       fidl::StringView("fuchsia-boot:///platform-bus#meta/platform-bus.cm"));
  auto wire_result = client->Start(std::move(args));
  if (wire_result.status() != ZX_OK) {
    FX_SLOG(ERROR, "Failed to call to Realm:Start", KV("status", wire_result.status()));
    return 1;
  }
  if (wire_result->is_error()) {
    FX_SLOG(ERROR, "Realm:Start failed", KV("error", wire_result->error_value()));
    return 1;
  }

  // Wait for ramctl and nand-ctl to be bound. Errors may be logged if the drivers are in the
  // process of binding while the DriverTestRealm is shutting down which can trip the high-severity
  // log checker in tests.
  zx::result ramctl = device_watcher::RecursiveWaitForFile("/dev/sys/platform/00:00:2d/ramctl");
  if (ramctl.is_error()) {
    FX_SLOG(ERROR, "Failed to wait for ramctl", KV("status", ramctl.status_value()));
  }
  zx::result nand_ctl = device_watcher::RecursiveWaitForFile("/dev/sys/platform/00:00:2e/nand-ctl");
  if (nand_ctl.is_error()) {
    FX_SLOG(ERROR, "Failed to wait for nand-ctl", KV("status", nand_ctl.status_value()));
  }
  return 0;
}
