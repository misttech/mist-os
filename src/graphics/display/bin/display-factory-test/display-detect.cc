// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/zx/channel.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <cstdint>
#include <cstdio>
#include <string>
#include <string_view>

namespace sysinfo = fuchsia_sysinfo;
namespace gpio = fuchsia_hardware_gpio;

enum class Board {
  kSherlock,
  kUnknown,
};

Board GetBoard() {
  zx::result<fidl::ClientEnd<sysinfo::SysInfo>> channel_result =
      component::Connect<sysinfo::SysInfo>();
  if (channel_result.is_error()) {
    return Board::kUnknown;
  }
  fidl::ClientEnd<sysinfo::SysInfo> channel = std::move(channel_result).value();

  const fidl::WireResult<fuchsia_sysinfo::SysInfo::GetBoardName> board_name_result =
      fidl::WireCall(channel)->GetBoardName();
  if (!board_name_result.ok()) {
    std::printf("SysInfo::GetBoardName() failed: %s\n", board_name_result.status_string());
    return Board::kUnknown;
  }
  const fidl::WireResponse<fuchsia_sysinfo::SysInfo::GetBoardName> board_name_response =
      board_name_result.value();
  if (board_name_response.status != ZX_OK) {
    std::printf("SysInfo::GetBoardName() returned error: %s\n",
                zx_status_get_string(board_name_response.status));
    return Board::kUnknown;
  };

  const std::string board_name(board_name_response.name.get());
  std::printf("Board name: %s\n", board_name.c_str());
  if (board_name.find("sherlock") != std::string_view::npos) {
    return Board::kSherlock;
  }
  return Board::kUnknown;
}

zx::result<uint8_t> GetGpioValue(const char* gpio_path) {
  zx::result client_end = component::Connect<gpio::Gpio>(gpio_path);
  if (client_end.is_error()) {
    return client_end.take_error();
  }

  const fidl::WireResult read_result = fidl::WireCall(*client_end)->Read();
  if (!read_result.ok()) {
    return zx::error(read_result.status());
  }
  fit::result<int, gpio::wire::GpioReadResponse*> read_response = std::move(read_result).value();

  if (read_response.is_error()) {
    return read_response.take_error();
  }
  const uint8_t read_value = read_response.value()->value;

  return zx::ok(read_value);
}

int main(int argc, const char* argv[]) {
  const Board board = GetBoard();

  if (board == Board::kSherlock) {
    static constexpr char kSherlockGpioPath[] = "/dev/sys/platform/05:04:1/aml-gpio/gpio-76";
    zx::result<uint8_t> gpio_value_result = GetGpioValue(kSherlockGpioPath);
    if (gpio_value_result.is_error()) {
      std::printf("MIPI device detect failed: %s\n", gpio_value_result.status_string());
      return -1;
    }
    const uint8_t gpio_value = gpio_value_result.value();
    std::printf("MIPI device detect type: %s\n", gpio_value ? "Innolux" : "BOE");
    return 0;
  }

  std::printf("Unsupported board\n");
  return 0;
}
