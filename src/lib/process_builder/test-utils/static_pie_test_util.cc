// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This program is used to test the process_builder library's handling of
// statically linked PIE executables.
//
// Uses elfldltl to bootstrap the static pie. Reads the processargs bootstrap
// message to find another channel handle with type PA_USER0, and then reads a
// message from that channel and echos it back on the same channel. The test
// uses this echo to confirm that the process was loaded correctly.
#include <lib/zircon-internal/unique-backtrace.h>
#include <zircon/processargs.h>
#include <zircon/syscalls.h>

#include <array>

#include "fuchsia-static-pie.h"

// Entry point. Arguments are a handle to the bootstrap channel and the base
// address that the vDSO was loaded at.
extern "C" void _start(zx_handle_t bootstrap_chan, void* vdso_base) {
  StaticPieSetup(vdso_base);

  // Read the bootstrap message from the bootstrap channel and find the PA_USER0
  // channel handle.
  union ReadMsg {
    zx_proc_args_t hdr;
    std::byte buffer[ZX_CHANNEL_MAX_MSG_BYTES];
  } read_msg;
  zx_handle_t read_handles[ZX_CHANNEL_MAX_MSG_HANDLES];
  uint32_t actual_bytes, actual_handles;
  if (zx_channel_read(bootstrap_chan, 0, &read_msg, read_handles, std::size(read_msg.buffer),
                      std::size(read_handles), &actual_bytes, &actual_handles) != ZX_OK) {
    CRASH_WITH_UNIQUE_BACKTRACE();
  }

  uint32_t* handle_info =
      reinterpret_cast<uint32_t*>(&read_msg.buffer[read_msg.hdr.handle_info_off]);

  zx_handle_t user_chan = ZX_HANDLE_INVALID;
  zx_handle_t loaded_vmar = ZX_HANDLE_INVALID;
  for (uint32_t i = 0; i < actual_handles; ++i) {
    switch (handle_info[i]) {
      case PA_HND(PA_USER0, 0):
        user_chan = read_handles[i];
        continue;

      case PA_HND(PA_VMAR_LOADED, 0):
        loaded_vmar = read_handles[i];
        continue;
    }
  }
  if (user_chan == ZX_HANDLE_INVALID) {
    CRASH_WITH_UNIQUE_BACKTRACE();
  }
  // Expect a valid handle to image vmar was provided.
  if (loaded_vmar == ZX_HANDLE_INVALID) {
    CRASH_WITH_UNIQUE_BACKTRACE();
  }

  // Apply relro protections.
  StaticPieRelro(loaded_vmar);

  // Read a message from the PA_USER0 channel and echo it back. Note that
  // ZX_ERR_SHOULD_WAIT isn't handled here; the test should make sure to write
  // to the channel before starting us.
  if (zx_channel_read(user_chan, 0, &read_msg, read_handles, std::size(read_msg.buffer),
                      std::size(read_handles), &actual_bytes, &actual_handles) != ZX_OK) {
    CRASH_WITH_UNIQUE_BACKTRACE();
  }
  zx_channel_write(user_chan, 0, &read_msg, actual_bytes, read_handles, actual_handles);

  // Exit cleanly.
  zx_process_exit(0);
}

// Inline functions in libc++ headers call this.
[[noreturn]] void std::__libcpp_verbose_abort(const char* format, ...) noexcept {
  __builtin_trap();
}
