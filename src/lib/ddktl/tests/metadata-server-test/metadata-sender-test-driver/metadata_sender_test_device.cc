// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/ddktl/tests/metadata-server-test/metadata-sender-test-driver/metadata_sender_test_device.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>

#include <bind/metadata_server_test_bind_library/cpp/bind.h>

namespace ddk::test {

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> MetadataSenderTestDevice::InitOutgoing() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "Failed to create endpoints: %s", endpoints.status_string());
    return endpoints.take_error();
  }

  zx_status_t status = metadata_server_.Serve(outgoing_, dispatcher_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to server metadata server: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  zx::result result = outgoing_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to serve outgoing directory: %s", result.status_string());
    return result.take_error();
  }

  return zx::ok(std::move(endpoints->client));
}

zx_status_t MetadataSenderTestDevice::Create(void* ctx, zx_device_t* parent, const char* name,
                                             zx_handle_t handle) {
  auto sender = std::make_unique<MetadataSenderTestDevice>(parent);

  zx::result outgoing = sender->InitOutgoing();
  if (outgoing.is_error()) {
    zxlogf(ERROR, "Failed to initialize outgoing directory: %s", outgoing.status_string());
    return outgoing.status_value();
  }

  std::array offers = {MetadataServer::kFidlServiceName};
  const std::vector<zx_device_str_prop_t> kStrProps = {
      ddk::MakeStrProperty(bind_metadata_server_test::PURPOSE,
                           bind_metadata_server_test::PURPOSE_SEND_METADATA),
  };

  zx_status_t status = sender->DdkAdd(ddk::DeviceAddArgs("metadata_sender")
                                          .set_fidl_service_offers(offers)
                                          .set_str_props(kStrProps)
                                          .set_outgoing_dir(outgoing->TakeChannel()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add device: %s", zx_status_get_string(status));
    return status;
  }

  [[maybe_unused]] auto unused = sender.release();
  return ZX_OK;
}

void MetadataSenderTestDevice::DdkRelease() { delete this; }

void MetadataSenderTestDevice::SetMetadata(SetMetadataRequestView request,
                                           SetMetadataCompleter::Sync& completer) {
  zx_status_t status = metadata_server_.SetMetadata(request->metadata);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to set metadata: %s", zx_status_get_string(status));
    completer.Reply(fit::error(status));
  }
  completer.Reply(fit::ok());
}

static constexpr zx_driver_ops_t sender_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.create = &MetadataSenderTestDevice::Create;
  return ops;
}();

}  // namespace ddk::test

ZIRCON_DRIVER(metadata_sender, ddk::test::sender_driver_ops, "zircon", "0.1");
