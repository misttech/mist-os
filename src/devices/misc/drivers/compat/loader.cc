// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/misc/drivers/compat/loader.h"

namespace fldsvc = fuchsia_ldsvc;

namespace compat {

Loader::Loader(async_dispatcher_t* dispatcher, fidl::UnownedClientEnd<fuchsia_ldsvc::Loader> loader,
               zx::vmo driver_vmo)
    : dispatcher_(dispatcher), client_(loader), driver_vmo_(std::move(driver_vmo)) {}

void Loader::Done(DoneCompleter::Sync& completer) { completer.Close(ZX_OK); }

void Loader::LoadObject(LoadObjectRequestView request, LoadObjectCompleter::Sync& completer) {
  // When there is a request for the DFv1 driver library, return the
  // compatibility driver's VMO instead.
  if (request->object_name.get() == kLibDriverName) {
    if (driver_vmo_) {
      completer.Reply(ZX_OK, std::move(driver_vmo_));
    } else {
      // We have already provided and instance of driver VMO, or
      // `Loader::Bind()` has not been called.
      completer.Reply(ZX_ERR_NOT_FOUND, {});
    }
    return;
  }

  fidl::WireResult result = fidl::WireCall(client_)->LoadObject(request->object_name);
  if (!result.ok()) {
    completer.Reply(result.status(), {});
    return;
  }
  completer.Reply(result->rv, std::move(result->object));
}

void Loader::Config(ConfigRequestView request, ConfigCompleter::Sync& completer) {
  fidl::WireResult result = fidl::WireCall(client_)->Config(request->config);
  if (!result.ok()) {
    completer.Reply(result.status());
    return;
  }
  completer.Reply(result->rv);
}

void Loader::Clone(CloneRequestView request, CloneCompleter::Sync& completer) {
  fidl::BindServer(dispatcher_, std::move(request->loader), this);
  completer.Reply(ZX_OK);
}

}  // namespace compat
