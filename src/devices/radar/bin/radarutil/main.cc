// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.radar/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <stdio.h>

#include "radarutil.h"

int main(int argc, char** argv) {
  zx::result client = component::Connect<fuchsia_hardware_radar::RadarBurstReaderProvider>();
  if (client.is_error()) {
    fprintf(stderr, "Failed to open %s: %s\n",
            fidl::DiscoverableProtocolName<fuchsia_hardware_radar::RadarBurstReaderProvider>,
            client.status_string());
    return 1;
  }

  zx_status_t status = radarutil::RadarUtil::Run(argc, argv, std::move(client.value()));
  return status == ZX_OK ? 0 : 1;
}
