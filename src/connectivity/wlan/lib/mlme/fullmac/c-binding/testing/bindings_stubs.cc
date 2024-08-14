// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bindings_stubs.h"

#include "src/connectivity/wlan/lib/mlme/fullmac/c-binding/bindings.h"

namespace bindings_stubs {

namespace {
StartFullmacType DefaultStartFullmacStub =
    [](rust_fullmac_device_ffi_t raw_device,
       zx_handle_t fullmac_client_end_handle) -> wlan_fullmac_mlme_handle_t * {
  static wlan_fullmac_mlme_handle_t handle;
  return &handle;
};

StopFullmacType DefaultStopFullmacStub = [](wlan_fullmac_mlme_handle_t *) {};
DeleteFullmacType DefaultDeleteFullmacStub = [](wlan_fullmac_mlme_handle_t *) {};

}  // namespace

StartFullmacType FullmacMlmeBindingsStubs::start_fullmac_mlme_stub = DefaultStartFullmacStub;
StopFullmacType FullmacMlmeBindingsStubs::stop_fullmac_mlme_stub = DefaultStopFullmacStub;
DeleteFullmacType FullmacMlmeBindingsStubs::delete_fullmac_mlme_stub = DefaultStopFullmacStub;

FullmacMlmeBindingsStubs::~FullmacMlmeBindingsStubs() {
  start_fullmac_mlme_stub = DefaultStartFullmacStub;
  stop_fullmac_mlme_stub = DefaultStopFullmacStub;
  delete_fullmac_mlme_stub = DefaultDeleteFullmacStub;
}

}  // namespace bindings_stubs

extern "C" wlan_fullmac_mlme_handle_t *start_fullmac_mlme(rust_fullmac_device_ffi_t raw_device,
                                                          zx_handle_t fullmac_client_end_handle) {
  return bindings_stubs::FullmacMlmeBindingsStubs::start_fullmac_mlme_stub(
      raw_device, fullmac_client_end_handle);
}
extern "C" void stop_fullmac_mlme(wlan_fullmac_mlme_handle_t *mlme) {
  bindings_stubs::FullmacMlmeBindingsStubs::stop_fullmac_mlme_stub(mlme);
}
extern "C" void delete_fullmac_mlme(wlan_fullmac_mlme_handle_t *mlme) {
  bindings_stubs::FullmacMlmeBindingsStubs::delete_fullmac_mlme_stub(mlme);
}
