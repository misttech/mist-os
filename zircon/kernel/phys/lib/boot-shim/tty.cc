// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/boot-shim/tty.h"

#include <lib/boot-options/boot-options.h>
#include <lib/uart/ns8250.h>
#include <lib/zbi-format/driver-config.h>

#include <optional>
#include <string_view>

namespace boot_shim {
namespace {

constexpr std::string_view kTtyPrefix = "tty";
constexpr std::string_view kSerialType = "S";
constexpr std::string_view kAmlType = "AML";
constexpr std::string_view kMsmType = "MSM";
constexpr std::string_view kConsoleArg = "console=";

}  // namespace

std::optional<Tty> TtyFromCmdline(std::string_view cmdline) {
  size_t pos = cmdline.rfind(kConsoleArg);
  if (pos == std::string_view::npos) {
    // Absent commandline assumes tty0.
    return Tty{.type = TtyType::kAny, .index = 0};
  }

  size_t arg_start = pos + kConsoleArg.length();
  size_t arg_end = cmdline.find(' ', arg_start);
  if (arg_end == std::string_view::npos) {
    arg_end = cmdline.size();
  }

  // format ttyTYPENNNNN
  std::string_view arg = cmdline.substr(arg_start, arg_end - arg_start);
  if (!arg.starts_with(kTtyPrefix)) {
    return std::nullopt;
  }
  arg.remove_prefix(kTtyPrefix.length());
  // Parse NNNN
  size_t index_start = arg.find_first_of("0123456789");
  if (index_start == std::string_view::npos) {
    return std::nullopt;
  }
  auto index = BootOptions::ParseInt(arg.substr(index_start));
  if (!index) {
    return std::nullopt;
  }

  // Parse TYPE
  auto type_str = arg.substr(0, index_start);
  TtyType type = TtyType::kAny;

  if (type_str == kSerialType) {
    type = TtyType::kSerial;
  } else if (type_str == kAmlType) {
    type = TtyType::kAml;
  } else if (type_str == kMsmType) {
    type = TtyType::kMsm;
  } else if (!type_str.empty()) {
    return std::nullopt;
  }

  return Tty{.type = type, .index = static_cast<size_t>(*index)};
}

}  // namespace boot_shim
