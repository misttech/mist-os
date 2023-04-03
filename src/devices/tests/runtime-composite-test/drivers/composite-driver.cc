// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/tests/runtime-composite-test/drivers/composite-driver.h"

#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>

#include "src/devices/tests/runtime-composite-test/drivers/composite-driver-bind.h"

namespace frct = fuchsia_runtime_composite_test;

namespace composite_driver {

// static
zx_status_t CompositeDriver::Bind(void* ctx, zx_device_t* device) {
  auto dev = std::make_unique<CompositeDriver>(device);

  // Verify the metadata.
  char metadata[32] = "";
  size_t len = 0;
  auto status = dev->DdkGetMetadata(DEVICE_METADATA_PRIVATE, &metadata, std::size(metadata), &len);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to read metadata %d", status);
    return status;
  }

  constexpr char kMetadataStr[] = "composite-metadata";
  if (strlen(kMetadataStr) + 1 != len) {
    zxlogf(ERROR, "Incorrect metadata size: %zu", strlen(kMetadataStr));
    return ZX_ERR_INTERNAL;
  }

  if (strcmp(kMetadataStr, metadata) != 0) {
    zxlogf(ERROR, "Incorrect metadata value: %s", metadata);
    return ZX_ERR_INTERNAL;
  }

  status = dev->DdkAdd("composite");
  if (status != ZX_OK) {
    return status;
  }

  [[maybe_unused]] auto ptr = dev.release();
  return ZX_OK;
}

void CompositeDriver::DdkInit(ddk::InitTxn init_txn) {
  // Connect to our parent driver's driver transport protocol.
  auto client_end =
      DdkConnectFragmentRuntimeProtocol<frct::Service::RuntimeCompositeProtocol>("test_primary");
  if (client_end.is_error()) {
    zxlogf(ERROR, "DdkConnectRuntimeProtocol failed");
    init_txn.Reply(client_end.status_value());
    return;
  }

  auto* dispatcher = fdf_dispatcher_get_current_dispatcher();
  client_ = fdf::Client<frct::RuntimeCompositeProtocol>(std::move(*client_end), dispatcher);

  client_->Handshake().ThenExactlyOnce(
      [init_txn = std::move(init_txn)](
          fdf::Result<frct::RuntimeCompositeProtocol::Handshake>& result) mutable {
        ZX_ASSERT(result.is_ok());
        init_txn.Reply(ZX_OK);
      });
}

static zx_driver_ops_t kDriverOps = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = CompositeDriver::Bind;
  return ops;
}();

}  // namespace composite_driver

ZIRCON_DRIVER(composite_driver, composite_driver::kDriverOps, "zircon", "0.1");
