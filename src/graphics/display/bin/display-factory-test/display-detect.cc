// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/zx/channel.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <cstdint>
#include <string_view>

namespace sysinfo = fuchsia_sysinfo;
namespace gpio = fuchsia_hardware_gpio;

enum Boards {
  SHERLOCK,
  LUIS,
  UNKNOWN_BOARD,
  BOARD_COUNT,
};

Boards board = UNKNOWN_BOARD;

Boards GetBoard() {
  zx::result channel = component::Connect<sysinfo::SysInfo>();
  if (channel.is_error()) {
    return UNKNOWN_BOARD;
  }

  const fidl::WireResult result = fidl::WireCall(channel.value())->GetBoardName();
  if (!result.ok()) {
    return UNKNOWN_BOARD;
  }
  const fidl::WireResponse response = result.value();
  if (response.status != ZX_OK) {
    return UNKNOWN_BOARD;
  };

  if (response.name.get().find("sherlock") != std::string_view::npos) {
    return SHERLOCK;
  }
  if (response.name.get().find("luis") != std::string_view::npos) {
    return LUIS;
  }
  return UNKNOWN_BOARD;
}

zx::result<uint8_t> GetGpioValue(const char* gpio_path) {
  zx::result client_end = component::Connect<gpio::Gpio>(gpio_path);
  if (client_end.is_error()) {
    return client_end.take_error();
  }

  const fidl::WireResult result = fidl::WireCall(*client_end)->Read();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  fit::result response = result.value();
  if (response.is_error()) {
    return response.take_error();
  }
  return zx::ok(response.value()->value);
}

int main(int argc, const char* argv[]) {
  if (GetBoard() == SHERLOCK) {
    const auto* path = "/dev/sys/platform/05:04:1/aml-axg-gpio/gpio-76";
    zx::result val = GetGpioValue(path);
    if (val.is_error()) {
      printf("MIPI device detect failed: %s\n", val.status_string());
      return -1;
    }
    printf("MIPI device detect type: %s\n", val.value() ? "Innolux" : "BOE");
  } else if (GetBoard() == LUIS) {
    printf("MIPI device detect type: BOE\n");
  } else {
    printf("Unknown board\n");
  }
  return 0;
}
