// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdio/namespace.h>
#include <lib/fit/defer.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <zircon/status.h>

#include <cerrno>

#include "args.h"
#include "name_tokens.h"
#include "src/bringup/bin/device-name-provider/device_name_provider_config.h"
#include "src/bringup/bin/netsvc/netifc-discover.h"

// Copies a word from the wordlist starting at |dest| and then adds |sep| at the end.
// Returns a pointer to the character after the separator.
char* append_word(char* dest, uint16_t num, char sep) {
  const char* word = dictionary[num % TOKEN_DICTIONARY_SIZE];
  memcpy(dest, word, strlen(word));
  dest += strlen(word);
  *dest = sep;
  dest++;
  return dest;
}

void device_id_get_words(const unsigned char mac[6], char out[HOST_NAME_MAX]) {
  char* dest = out;
  dest = append_word(dest, static_cast<uint16_t>(mac[0] | ((mac[4] << 8) & 0xF00)), '-');
  dest = append_word(dest, static_cast<uint16_t>(mac[1] | ((mac[5] << 8) & 0xF00)), '-');
  dest = append_word(dest, static_cast<uint16_t>(mac[2] | ((mac[4] << 4) & 0xF00)), '-');
  dest = append_word(dest, static_cast<uint16_t>(mac[3] | ((mac[5] << 4) & 0xF00)), 0);
}

const char hex_chars[17] = "0123456789abcdef";

// Copies 4 hex characters of hex value of the bits of |num|.
// Then writes |sep| to the character after.
// Returns a pointer to the character after the separator.
char* append_hex(char* dest, uint16_t num, char sep) {
  for (uint8_t i = 0; i < 4; i++) {
    uint16_t left = num >> ((3 - i) * 4);
    *dest = hex_chars[left & 0x0F];
    dest++;
  }
  *dest = sep;
  dest++;
  return dest;
}

#define PREFIX_LEN 9
const char mac_prefix[PREFIX_LEN] = "fuchsia-";

void device_id_get_mac(const unsigned char mac[6], char out[HOST_NAME_MAX]) {
  char* dest = out;
  // Prepend with 'fs-'
  // Prepended with mac_prefix
  for (uint8_t i = 0; i < PREFIX_LEN; i++) {
    dest[i] = mac_prefix[i];
  }
  dest = dest + PREFIX_LEN - 1;
  dest = append_hex(dest, static_cast<uint16_t>((mac[0] << 8) | mac[1]), '-');
  dest = append_hex(dest, static_cast<uint16_t>((mac[2] << 8) | mac[3]), '-');
  dest = append_hex(dest, static_cast<uint16_t>((mac[4] << 8) | mac[5]), 0);
}

void device_id_get(const unsigned char mac[6], char out[HOST_NAME_MAX], uint32_t generation) {
  if (generation == 1) {
    device_id_get_mac(mac, out);
  } else {  // Style 0
    device_id_get_words(mac, out);
  }
}

class DeviceNameProviderServer final : public fidl::WireServer<fuchsia_device::NameProvider> {
  const char* name;
  const size_t size;

 public:
  DeviceNameProviderServer(const char* device_name, size_t size) : name(device_name), size(size) {}
  void GetDeviceName(GetDeviceNameCompleter::Sync& completer) override {
    completer.ReplySuccess(fidl::StringView::FromExternal(name, size));
  }
};

int main(int argc, char** argv) {
  // TODO(https://fxbug.dev/42073486): Remove this once the elf runner no longer
  // fools libc into block-buffering stdout.
  setlinebuf(stdout);

  fdio_flat_namespace_t* ns;
  if (zx_status_t status = fdio_ns_export_root(&ns); status != ZX_OK) {
    printf("device-name-provider: FATAL: fdio_ns_export_root() = %s\n",
           zx_status_get_string(status));
    return -1;
  }
  auto free = fit::defer([ns]() { fdio_ns_free_flat_ns(ns); });
  fidl::ClientEnd<fuchsia_io::Directory> svc_root;
  for (size_t i = 0; i < ns->count; ++i) {
    if (std::string_view{ns->path[i]} == "/svc") {
      svc_root = decltype(svc_root){zx::channel{std::exchange(ns->handle[i], ZX_HANDLE_INVALID)}};
      break;
    }
  }
  if (!svc_root.is_valid()) {
    printf("device-name-provider: FATAL: did not find /svc in namespace\n");
    return -1;
  }

  auto config = device_name_provider_config::Config::TakeFromStartupHandle();
  DeviceNameProviderArgs args;
  const char* errmsg = nullptr;
  int err = ParseArgs(argc, argv, config, svc_root, &errmsg, &args);
  if (err) {
    printf("device-name-provider: FATAL: ParseArgs(_) = %d; %s\n", err, errmsg);
    return err;
  }

  char device_name[HOST_NAME_MAX];
  if (!args.nodename.empty()) {
    strlcpy(device_name, args.nodename.c_str(), sizeof(device_name));
  } else {
    zx::result status = netifc_discover(args.devdir, args.interface);
    if (status.is_error()) {
      strlcpy(device_name, fuchsia_device::wire::kDefaultDeviceName, sizeof(device_name));
      printf("device-name-provider: using default name \"%s\": netifc_discover(\"%s\", ...) = %s\n",
             device_name, args.devdir.c_str(), status.status_string());
    } else {
      const NetdeviceInterface& interface = status.value();
      device_id_get(interface.mac.x, device_name, args.namegen);
      printf("device-name-provider: generated device name: %s\n", device_name);
    }
  }

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  async_dispatcher_t* dispatcher = loop.dispatcher();
  if (dispatcher == nullptr) {
    printf("device-name-provider: FATAL: loop.dispatcher() = nullptr\n");
    return -1;
  }

  component::OutgoingDirectory outgoing(dispatcher);
  if (zx::result result = outgoing.ServeFromStartupInfo(); result.is_error()) {
    printf("device-name-provider: FATAL: outgoing.ServeFromStartupInfo() = %s\n",
           result.status_string());
    return -1;
  }

  DeviceNameProviderServer server(device_name, strnlen(device_name, sizeof(device_name)));

  if (zx::result result = outgoing.AddUnmanagedProtocol<fuchsia_device::NameProvider>(
          [dispatcher, server](fidl::ServerEnd<fuchsia_device::NameProvider> server_end) mutable {
            fidl::BindServer(dispatcher, std::move(server_end), &server);
          });
      result.is_error()) {
    printf("device-name-provider: FATAL: outgoing.AddUnmanagedProtocol = %s\n",
           result.status_string());
    return -1;
  }

  zx_status_t status = loop.Run();
  printf("device-name-provider: loop.Run() = %s\n", zx_status_get_string(status));
  return status;
}
