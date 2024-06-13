// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/riscv64/feature.h>
#include <lib/stdcompat/string_view.h>
#include <zircon/assert.h>

namespace arch {

RiscvFeatures& RiscvFeatures::SetMany(std::string_view isa_string) {
  using namespace std::string_view_literals;

  auto next_token = [&isa_string]() -> std::string_view {
    size_t pos = isa_string.find('_');
    if (pos == std::string_view::npos) {  // Last token.
      return std::exchange(isa_string, {});
    }
    std::string_view token = isa_string.substr(0, pos);
    isa_string.remove_prefix(pos + 1);
    return token;
  };

  std::string_view standard_exts = next_token();
  ZX_ASSERT(cpp20::starts_with(standard_exts, "rv32") || cpp20::starts_with(standard_exts, "rv64"));
  standard_exts.remove_prefix(4);  // Eat "rv{32,64}".
  if (standard_exts.find('v') != std::string_view::npos) {
    Set(RiscvFeature::kVector);
  }

  while (!isa_string.empty()) {
    std::string_view ext = next_token();
    if (ext == "sstc"sv) {
      Set(RiscvFeature::kSstc);
    } else if (ext == "svpbmt"sv) {
      Set(RiscvFeature::kSvpbmt);
    } else if (ext == "zicbom"sv) {
      Set(RiscvFeature::kZicbom);
    } else if (ext == "zicboz"sv) {
      Set(RiscvFeature::kZicboz);
    } else if (ext == "zicntr"sv) {
      Set(RiscvFeature::kZicntr);
    }
  }
  return *this;
}

}  // namespace arch
