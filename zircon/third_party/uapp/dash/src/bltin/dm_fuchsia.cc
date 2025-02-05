// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.power.statecontrol/cpp/fidl.h>
#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <zircon/status.h>

static int print_dm_help() {
  printf(
      "poweroff             - power off the system\n"
      "shutdown             - power off the system\n"
      "suspend              - suspend the system to RAM\n"
      "reboot               - reboot the system\n"
      "reboot-bootloader/rb - reboot the system into bootloader\n"
      "reboot-recovery/rr   - reboot the system into recovery\n"
      "kerneldebug          - send a command to the kernel\n"
      "ktraceoff            - stop kernel tracing\n"
      "ktraceon             - start kernel tracing\n");
  return 0;
}

static int send_kernel_debug_command(const char* command, size_t length) {
  if (length > fuchsia_kernel::wire::kDebugCommandMax) {
    fprintf(stderr, "error: kernel debug command longer than %u bytes: '%.*s'\n",
            fuchsia_kernel::wire::kDebugCommandMax, (int)length, command);
    return -1;
  }

  auto client_end = component::Connect<fuchsia_kernel::DebugBroker>();
  if (client_end.is_error()) {
    return client_end.status_value();
  }

  auto result = fidl::WireSyncClient(std::move(*client_end))
                    ->SendDebugCommand(fidl::StringView::FromExternal(command, length));
  if (result.status() != ZX_OK || result->status != ZX_OK) {
    return -1;
  }

  return 0;
}

static int send_kernel_tracing_enabled(bool enabled) {
  auto client_end = component::Connect<fuchsia_kernel::DebugBroker>();
  if (client_end.is_error()) {
    return client_end.status_value();
  }

  auto result = fidl::WireSyncClient(std::move(*client_end))->SetTracingEnabled(enabled);
  if (result.status() != ZX_OK || result->status != ZX_OK) {
    return -1;
  }

  return 0;
}

template <typename Func>
static int send_statecontrol_admin_command(Func f) {
  auto client_end = component::Connect<fuchsia_hardware_power_statecontrol::Admin>();
  if (client_end.is_error()) {
    return client_end.status_value();
  }
  auto client = fidl::WireSyncClient(std::move(*client_end));
  auto response = f(std::move(client));

  if (response.status() != ZX_OK) {
    printf("Command failed: %s (%d)\n", response.status_string(), response.status());
    return -1;
  }

  if (response->is_error()) {
    printf("Command failed: %d\n", response->error_value());
  }

  return 0;
}

static bool command_cmp(const char* long_command, const char* short_command, const char* input,
                        int* command_length) {
  const size_t input_length = strlen(input);

  // Ensure that the first command_length chars of input match and that it is
  // either the whole input or there is a space after the command, we don't want
  // partial command matching.
  if (short_command) {
    const size_t short_length = strlen(short_command);
    if (input_length >= short_length && strncmp(short_command, input, short_length) == 0 &&
        ((input_length == short_length) || input[short_length] == ' ')) {
      *command_length = short_length;
      return true;
    }
  }

  const size_t long_length = strlen(long_command);
  if (input_length >= long_length && strncmp(long_command, input, long_length) == 0 &&
      ((input_length == long_length) || input[long_length] == ' ')) {
    *command_length = long_length;
    return true;
  }
  return false;
}

__BEGIN_CDECLS
int zxc_dm(int argc, char** argv) {
  if (argc != 2) {
    printf("usage: dm <command>\n");
    return -1;
  }

  // Handle service backed commands.
  int command_length = 0;
  if (command_cmp("kerneldebug", NULL, argv[1], &command_length)) {
    return send_kernel_debug_command(argv[1] + command_length, strlen(argv[1]) - command_length);
  } else if (command_cmp("ktraceon", NULL, argv[1], &command_length)) {
    return send_kernel_tracing_enabled(true);

  } else if (command_cmp("ktraceoff", NULL, argv[1], &command_length)) {
    return send_kernel_tracing_enabled(false);

  } else if (command_cmp("help", NULL, argv[1], &command_length)) {
    return print_dm_help();

  } else if (command_cmp("dump", NULL, argv[1], &command_length)) {
    printf("`dm dump` is deprecated. Please use `driver dump` instead\n");
    return -1;

  } else if (command_cmp("drivers", NULL, argv[1], &command_length)) {
    printf("`dm drivers` is deprecated. Please use `driver list --verbose` instead\n");
    return -1;

  } else if (command_cmp("devprops", NULL, argv[1], &command_length)) {
    printf("`dm devprops` is deprecated. Please use `driver list-devices --verbose` instead\n");
    return -1;

  } else if (command_cmp("reboot", NULL, argv[1], &command_length)) {
    return send_statecontrol_admin_command(
        [](fidl::WireSyncClient<fuchsia_hardware_power_statecontrol::Admin> client) {
          fidl::Arena arena;
          auto builder = fuchsia_hardware_power_statecontrol::wire::RebootOptions::Builder(arena);
          std::vector<fuchsia_hardware_power_statecontrol::RebootReason2> reasons = {
              fuchsia_hardware_power_statecontrol::RebootReason2::kUserRequest};
          auto vector_view =
              fidl::VectorView<fuchsia_hardware_power_statecontrol::RebootReason2>::FromExternal(
                  reasons);
          builder.reasons(vector_view);
          return client->PerformReboot(builder.Build());
        });

  } else if (command_cmp("reboot-bootloader", "rb", argv[1], &command_length)) {
    return send_statecontrol_admin_command(
        [](fidl::WireSyncClient<fuchsia_hardware_power_statecontrol::Admin> client) {
          return client->RebootToBootloader();
        });

  } else if (command_cmp("reboot-recovery", "rr", argv[1], &command_length)) {
    return send_statecontrol_admin_command(
        [](fidl::WireSyncClient<fuchsia_hardware_power_statecontrol::Admin> client) {
          return client->RebootToRecovery();
        });

  } else if (command_cmp("suspend", NULL, argv[1], &command_length)) {
#ifdef ENABLE_SUSPEND
    return send_statecontrol_admin_command(
        [](fidl::WireSyncClient<fuchsia_hardware_power_statecontrol::Admin> client) {
          return client->SuspendToRam();
        });
#else
    printf("Suspend is not supported on this device\n");
    return -1;
#endif

  } else if (command_cmp("poweroff", NULL, argv[1], &command_length) ||
             command_cmp("shutdown", NULL, argv[1], &command_length)) {
    return send_statecontrol_admin_command(
        [](fidl::WireSyncClient<fuchsia_hardware_power_statecontrol::Admin> client) {
          return client->Poweroff();
        });

  } else {
    printf("Unknown command '%s'\n\n", argv[1]);
    printf("Valid commands:\n");
    print_dm_help();
  }

  return -1;
}
__END_CDECLS

static char* join(char* buffer, size_t buffer_length, int argc, char** argv) {
  size_t total_length = 0u;
  for (int i = 0; i < argc; ++i) {
    if (i > 0) {
      if (total_length + 1 > buffer_length)
        return NULL;
      buffer[total_length++] = ' ';
    }
    const char* arg = argv[i];
    size_t arg_length = strlen(arg);
    if (total_length + arg_length + 1 > buffer_length)
      return NULL;
    strncpy(buffer + total_length, arg, buffer_length - total_length - 1);
    total_length += arg_length;
  }
  return buffer + total_length;
}

__BEGIN_CDECLS
int zxc_k(int argc, char** argv) {
  if (argc <= 1) {
    printf("usage: k <command>\n");
    return -1;
  }

  char buffer[256];
  size_t command_length = 0u;

  // If we detect someone trying to use the LK poweroff/reboot,
  // divert it to devmgr backed one instead.
  if (!strcmp(argv[1], "poweroff") || !strcmp(argv[1], "reboot") ||
      !strcmp(argv[1], "reboot-bootloader")) {
    return zxc_dm(argc, argv);
  }

  char* command_end = join(buffer, sizeof(buffer), argc - 1, &argv[1]);
  if (!command_end) {
    fprintf(stderr, "error: kernel debug command too long\n");
    return -1;
  }
  command_length = command_end - buffer;

  return send_kernel_debug_command(buffer, command_length);
}
__END_CDECLS
