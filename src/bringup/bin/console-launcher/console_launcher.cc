// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/bringup/bin/console-launcher/console_launcher.h"

#include <fcntl.h>
#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/spawn.h>
#include <lib/fdio/watcher.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zircon-internal/paths.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/processargs.h>
#include <zircon/status.h>

namespace console_launcher {

zx::result<ConsoleLauncher> ConsoleLauncher::Create() {
  ConsoleLauncher launcher;

  // TODO(https://fxbug.dev/42109210): Remove all uses of the root job.
  zx::result client_end = component::Connect<fuchsia_kernel::RootJob>();
  if (client_end.is_error()) {
    FX_PLOGS(ERROR, client_end.status_value())
        << "failed to connect to " << fidl::DiscoverableProtocolName<fuchsia_kernel::RootJob>;
    return client_end.take_error();
  }
  const fidl::WireResult result = fidl::WireCall(client_end.value())->Get();
  if (!result.ok()) {
    FX_PLOGS(ERROR, result.status()) << "failed to get root job";
    return zx::error(result.status());
  }
  if (zx_status_t status = zx::job::create(result.value().job, 0u, &launcher.shell_job_);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to create shell job";
    return zx::error(status);
  }
  constexpr char name[] = "zircon-shell";
  if (zx_status_t status = launcher.shell_job_.set_property(ZX_PROP_NAME, name, sizeof(name));
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to set shell job name";
    return zx::error(status);
  }

  return zx::ok(std::move(launcher));
}

zx::result<Arguments> GetArguments(const fidl::ClientEnd<fuchsia_boot::Arguments>& client,
                                   const console_launcher_config::Config& config) {
  Arguments ret;

  {
    fuchsia_boot::wire::BoolPair bool_keys[]{{
                                                 .key = "console.shell",
                                                 .defaultval = false,
                                             },
                                             {
                                                 .key = "netsvc.disable",
                                                 .defaultval = true,
                                             },
                                             {
                                                 .key = "netsvc.netboot",
                                                 .defaultval = false,
                                             },
                                             {
                                                 .key = "console.use_virtio_console",
                                                 .defaultval = false,
                                             }};
    const fidl::WireResult result = fidl::WireCall(client)->GetBools(
        fidl::VectorView<fuchsia_boot::wire::BoolPair>::FromExternal(bool_keys));
    if (!result.ok()) {
      FX_PLOGS(ERROR, result.status()) << "failed to get boot bools";
      return zx::error(result.status());
    }
    const fidl::WireResponse response = result.value();
    const bool console_shell = response.values[0];
    ret.run_shell = console_shell;
    ret.virtcon_disabled = config.virtcon_disabled();
    const bool netsvc_disable = response.values[1];
    const bool netsvc_netboot = response.values[2];
    const bool netboot = !netsvc_disable && netsvc_netboot;
    ret.virtual_console_need_debuglog = netboot;
    ret.use_virtio_console = response.values[3];
  }

  fidl::StringView vars[]{
      "TERM",
      "zircon.autorun.boot",
      "zircon.autorun.system",
  };
  const fidl::WireResult resp =
      fidl::WireCall(client)->GetStrings(fidl::VectorView<fidl::StringView>::FromExternal(vars));
  if (!resp.ok()) {
    FX_PLOGS(ERROR, resp.status()) << "failed to get boot strings";
    return zx::error(resp.status());
  }

  if (resp->values[0].is_null()) {
    ret.term += "uart";
  } else {
    ret.term += resp->values[0].get();
  }
  if (!resp->values[1].is_null()) {
    ret.autorun_boot = resp->values[1].get();
  }
  if (!resp->values[2].is_null()) {
    ret.autorun_system = resp->values[2].get();
  }

  return zx::ok(ret);
}

zx::result<zx::process> ConsoleLauncher::LaunchShell(
    fidl::ClientEnd<fuchsia_io::Directory> root, fidl::ClientEnd<fuchsia_ldsvc::Loader> loader,
    fidl::ClientEnd<fuchsia_hardware_pty::Device> stdio, const std::string& term,
    const std::optional<std::string>& cmd) const {
  const char* argv[] = {ZX_SHELL_DEFAULT, nullptr, nullptr, nullptr};
  if (cmd.has_value()) {
    argv[1] = "-c";
    argv[2] = cmd.value().c_str();
  }
  const char* environ[] = {term.c_str(), nullptr};

  fdio_spawn_action_t actions[] = {
      // Add an action to set the new process name.
      {
          .action = FDIO_SPAWN_ACTION_SET_NAME,
          .name =
              {
                  .data = "sh:console",
              },
      },

      // Add an action to mount the pseudo directory in the shell's namespace.
      {
          .action = FDIO_SPAWN_ACTION_ADD_NS_ENTRY,
          .ns =
              {
                  .prefix = "/",
                  .handle = root.TakeChannel().release(),
              },
      },

      // Add an action to transfer the STDIO handle.
      {
          .action = FDIO_SPAWN_ACTION_ADD_HANDLE,
          .h =
              {
                  .id = PA_HND(PA_FD, FDIO_FLAG_USE_FOR_STDIO),
                  .handle = stdio.TakeChannel().release(),
              },
      },

      // Add an action to transfer the loader service handle.
      {
          .action = FDIO_SPAWN_ACTION_ADD_HANDLE,
          .h =
              {
                  .id = PA_HND(PA_LDSVC_LOADER, 0),
                  .handle = loader.TakeChannel().release(),
              },
      },
  };

  constexpr uint32_t flags = FDIO_SPAWN_CLONE_ALL & ~FDIO_SPAWN_CLONE_STDIO &
                             ~FDIO_SPAWN_CLONE_NAMESPACE & ~FDIO_SPAWN_DEFAULT_LDSVC;

  FX_LOGS(INFO) << "launching " << argv[0] << " (" << actions[0].name.data << ")";
  char err_msg[FDIO_SPAWN_ERR_MSG_MAX_LENGTH];
  zx::process process;
  if (zx_status_t status =
          fdio_spawn_etc(shell_job_.get(), flags, argv[0], argv, environ, std::size(actions),
                         actions, process.reset_and_get_address(), err_msg);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to launch console shell: " << err_msg;
    return zx::error(status);
  }
  return zx::ok(std::move(process));
}

zx_status_t WaitForExit(zx::process process) {
  if (zx_status_t status = process.wait_one(ZX_PROCESS_TERMINATED, zx::time::infinite(), nullptr);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to wait for console shell termination";
    return status;
  }
  zx_info_process_t proc_info;
  if (zx_status_t status =
          process.get_info(ZX_INFO_PROCESS, &proc_info, sizeof(proc_info), nullptr, nullptr);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to get console shell termination cause";
    return status;
  }
  const bool started = (proc_info.flags & ZX_INFO_PROCESS_FLAG_STARTED) != 0;
  const bool exited = (proc_info.flags & ZX_INFO_PROCESS_FLAG_EXITED) != 0;
  FX_LOGS(INFO) << "console shell exited (started=" << started << " exited=" << exited
                << ", return_code=" << proc_info.return_code << ")";
  return ZX_OK;
}

}  // namespace console_launcher
