// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/processargs/processargs.h>
#include <lib/stdcompat/string_view.h>
#include <lib/zircon-internal/unique-backtrace.h>
#include <unistd.h>

#include "zircon.h"

namespace ld {
namespace {

using namespace std::literals;

constexpr std::string_view kLdDebugPrefix = "\0LD_DEBUG="sv;
constexpr std::string_view kLdDebugPrefixFirst = kLdDebugPrefix.substr(1);

constexpr bool HasLdDebug(std::string_view env) {
  // This should be constexpr, but substr isn't until C++20.
  std::string_view debug;
  if (cpp20::starts_with(env, kLdDebugPrefixFirst)) {
    debug = env.substr(kLdDebugPrefixFirst.size());
  } else if (size_t found = env.find(kLdDebugPrefix); found != std::string_view::npos) {
    debug = env.substr(found + kLdDebugPrefix.size());
  }
  return !debug.empty() && debug.front() != '\0';
}

void TakeLogHandle(StartupData& startup, zx::handle handle) {
  zx_info_handle_basic_t info;
  zx_status_t status = handle.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) [[unlikely]] {
    CRASH_WITH_UNIQUE_BACKTRACE();
  }

  switch (info.type) {
    case ZX_OBJ_TYPE_DEBUGLOG:
      startup.debuglog = zx::debuglog{handle.release()};
      break;
    case ZX_OBJ_TYPE_SOCKET:
      startup.log_socket = zx::socket{handle.release()};
      break;
  }
}

}  // namespace

StartupData ReadBootstrap(zx::unowned_channel bootstrap) {
  StartupData startup;

  uint32_t nbytes, nhandles;
  zx_status_t status = processargs_message_size(bootstrap->get(), &nbytes, &nhandles);
  if (status != ZX_OK) [[unlikely]] {
    CRASH_WITH_UNIQUE_BACKTRACE();
  }
  PROCESSARGS_BUFFER(buffer, nbytes);
  zx_handle_t handles[nhandles];
  // These will be filled to point into the buffer.
  zx_proc_args_t* procargs;
  uint32_t* handle_info;
  status = processargs_read(bootstrap->get(), buffer, nbytes, handles, nhandles, &procargs,
                            &handle_info);
  if (status != ZX_OK) [[unlikely]] {
    CRASH_WITH_UNIQUE_BACKTRACE();
  }

  for (uint32_t i = 0; i < nhandles; ++i) {
    // If not otherwise consumed below, the handle will be closed.
    zx::handle handle{std::exchange(handles[i], {})};
    switch (handle_info[i]) {
      case PA_VMAR_ROOT:
        startup.vmar.reset(handle.release());
        break;

      case PA_VMAR_LOADED:
        startup.self_vmar.reset(handle.release());
        break;

      case PA_VMO_EXECUTABLE:
        startup.executable_vmo.reset(handle.release());
        break;

      case PA_HND(PA_FD, STDERR_FILENO):
        TakeLogHandle(startup, std::move(handle));
        break;
    }
  }

  // The only part of the strings of interest is the environment, and only to
  // search it for LD_DEBUG.
  std::string_view env{
      reinterpret_cast<const char*>(&buffer[procargs->environ_off]),
      nbytes - procargs->environ_off,
  };
  startup.ld_debug = HasLdDebug(env);

  return startup;
}

}  // namespace ld
