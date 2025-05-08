// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <zircon/status.h>

extern "C" int zxc_dm(int, char**);

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
  const zx_status_t fidl_status = result.status();
  if (fidl_status != ZX_OK) {
    fprintf(stderr,
            "error: unable to send kernel debug command (%s), is kernel debugging disabled?\n",
            zx_status_get_string(fidl_status));
    return -1;
  }

  // We got a reply.
  const zx_status_t command_status = result->status;
  if (command_status != ZX_OK) {
    // If the command status was ZX_ERR_NOT_SUPPORTED, then the kernel probably has debugging
    // syscalls disabled.
    const char* hint =
        (command_status == ZX_ERR_NOT_SUPPORTED) ? ", is kernel debugging disabled?" : "";
    fprintf(stderr, "error: %s%s\n", zx_status_get_string(command_status), hint);
    return -1;
  }

  return 0;
}

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
  // divert it to power backed one instead.
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
