// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/main.h"

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/zx/resource.h>
#include <zircon/process.h>
#include <zircon/processargs.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <cstdio>
#include <cstdlib>
#include <limits>
#include <memory>

#include "src/devices/lib/log/log.h"
#include "src/sys/lib/stdout-to-debuglog/cpp/stdout-to-debuglog.h"

// Get the root job from the root job service.
zx::result<zx::job> get_root_job() {
  zx::result client_end = component::Connect<fuchsia_kernel::RootJob>();
  if (client_end.is_error()) {
    return client_end.take_error();
  }
  fidl::WireResult result = fidl::WireCall(client_end.value())->Get();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok(std::move(result.value().job));
}

// Get the root resource from the root resource service. Not receiving the
// startup handle is logged, but not fatal.  In test environments, it would not
// be present.
zx::result<zx::resource> get_root_resource() {
  zx::result client_end = component::Connect<fuchsia_boot::RootResource>();
  if (client_end.is_error()) {
    return client_end.take_error();
  }
  fidl::WireResult result = fidl::WireCall(client_end.value())->Get();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok(std::move(result.value().resource));
}

// Get the mexec resource from the mexec resource service. Not receiving the
// startup handle is logged, but not fatal.  In test environments, it would not
// be present.
zx::result<zx::resource> get_mexec_resource() {
  zx::result client_end = component::Connect<fuchsia_kernel::MexecResource>();
  if (client_end.is_error()) {
    return client_end.take_error();
  }
  fidl::WireResult result = fidl::WireCall(client_end.value())->Get();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok(std::move(result.value().resource));
}

// Sets the logging process name. Needed to redirect output
// to serial.
static void SetLoggingProcessName() {
  char process_name[ZX_MAX_NAME_LEN] = "";

  zx_status_t name_status =
      zx::process::self()->get_property(ZX_PROP_NAME, process_name, sizeof(process_name));
  if (name_status != ZX_OK) {
    process_name[0] = '\0';
  }
  driver_logger::GetLogger().AddTag(process_name);
}

int main(int argc, char** argv) {
  zx_status_t status = StdoutToDebuglog::Init();
  if (status != ZX_OK) {
    LOGF(INFO, "Failed to redirect stdout to debuglog, assuming test environment and continuing");
  }

  auto args_result = component::Connect<fuchsia_boot::Arguments>();
  if (args_result.is_error()) {
    LOGF(ERROR, "Failed to get boot arguments service handle: %s", args_result.status_string());
    return args_result.error_value();
  }

  auto config = driver_manager_config::Config::TakeFromStartupHandle();

  if (config.verbose()) {
    driver_logger::GetLogger().SetSeverity(std::numeric_limits<FuchsiaLogSeverity>::min());
  }

  SetLoggingProcessName();

  auto boot_args = fidl::WireSyncClient<fuchsia_boot::Arguments>{std::move(*args_result)};
  if (config.use_driver_framework_v2()) {
    return RunDfv2(std::move(config), std::move(boot_args));
  }
  return RunDfv1(std::move(config), std::move(boot_args));
}
