// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/device.h>

#include <cstring>

#include "src/devices/board/drivers/vim3/vim3.h"

namespace vim3 {

bool Vim3::HasLcd() {
  // It checks the availability of DSI display by checking the boot variable set
  // by the bootloader (or overridden by build configuration).
  //
  // TODO(https://fxbug.dev/42076008): Currently either this is hardcoded at build-time or
  // it relies on the bootloader to set up the value. We should support probing
  // LCD display availability directly in Fuchsia instead.

  // The result is idempotent, thus it can be cached.
  if (has_lcd_.has_value()) {
    return *has_lcd_;
  }
  bool has_lcd = [&] {
    constexpr const char* kBootVariable = "driver.vim3.has_lcd";
    char value[32];
    zx_status_t status = device_get_variable(parent_, kBootVariable, value, sizeof(value), nullptr);
    if (status == ZX_OK) {
      return strncmp(value, "true", sizeof("true")) == 0 || strncmp(value, "1", sizeof("1")) == 0;
    }
    if (status == ZX_ERR_NOT_FOUND) {
      return false;
    }
    zxlogf(ERROR, "Cannot get boot variable (%s): %s", kBootVariable, zx_status_get_string(status));
    return false;
  }();
  has_lcd_ = has_lcd;
  return *has_lcd_;
}

}  // namespace vim3
