// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pty_fuchsia.h"

#include <fidl/fuchsia.hardware.pty/cpp/wire.h>

namespace fpty = fuchsia_hardware_pty;

zx_status_t pty_read_events(zx_handle_t handle, uint32_t* out_events) {
  const fidl::WireResult result =
      fidl::WireCall(fidl::UnownedClientEnd<fpty::Device>{zx::unowned_channel{handle}})
          ->ReadEvents();
  if (result.status() != ZX_OK) {
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (response.status != ZX_OK) {
    return response.status;
  }
  *out_events = response.events;
  return ZX_OK;
}

bool pty_event_is_interrupt(uint32_t events) {
  return events & fuchsia_hardware_pty::wire::kEventInterrupt;
}
