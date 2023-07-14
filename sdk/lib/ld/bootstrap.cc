// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/static-pie-with-vdso.h>
#include <zircon/syscalls.h>

// TODO(fxbug.dev/130483): _start should normally be `[[noreturn]] void` though for the timebeing
// it returns an int for easy testing.
extern "C" int _start(zx_handle_t handle, void* vdso) {
  auto diag = elfldltl::TrapDiagnostics();
  elfldltl::LinkStaticPieWithVdso(elfldltl::Self<>(), diag, vdso);

  int array[2];
  zx_channel_read(handle, 0, &array, nullptr, sizeof(array), 0, nullptr, nullptr);
  return array[0] + array[1];
}
