// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/usb-peripheral-utils/event-watcher.h>

namespace usb_peripheral_utils {

EventWatcher::EventWatcher(async::Loop& loop,
                           fidl::ServerEnd<fuchsia_hardware_usb_peripheral::Events> svc,
                           size_t functions)
    : loop_(loop), functions_(functions) {
  fidl::BindServer(loop.dispatcher(), std::move(svc), this);
}

void EventWatcher::FunctionRegistered(FunctionRegisteredCompleter::Sync& completer) {
  functions_registered_++;
  if (all_functions_registered()) {
    loop_.Quit();
    completer.Close(ZX_ERR_CANCELED);
  } else {
    completer.Reply();
  }
}

void EventWatcher::FunctionsCleared(FunctionsClearedCompleter::Sync& completer) {
  all_functions_cleared_ = true;
  loop_.Quit();
  completer.Close(ZX_ERR_CANCELED);
}

}  // namespace usb_peripheral_utils
