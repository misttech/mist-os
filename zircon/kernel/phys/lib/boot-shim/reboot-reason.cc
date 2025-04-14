// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/boot-shim/reboot-reason.h"

#include <array>

namespace boot_shim {
namespace {

// See https://source.android.com/docs/core/architecture/bootloader/boot-reason
constexpr std::string_view kBootArg = "androidboot.bootreason=";

struct RebootReasonMap {
  std::string_view reason;
  zbi_hw_reboot_reason_t value;
};

constexpr auto kRebootReasons = std::to_array<RebootReasonMap>({

    // Generally indicates the hardware has its state reset and ramoops/crashlog should retain
    // persistent
    // content.
    {.reason = "warm", .value = ZBI_HW_REBOOT_REASON_WARM},

    // Generally indicates the memory and the devices retain some state, and the ramoops/crashlog
    // backing
    // store contains persistent content.
    {.reason = "hard", .value = ZBI_HW_REBOOT_REASON_WARM},

    // Generally indicates a full reset of all devices, including memory.
    {.reason = "cold", .value = ZBI_HW_REBOOT_REASON_COLD},
    {.reason = "watchdog", .value = ZBI_HW_REBOOT_REASON_WATCHDOG},
});

}  // namespace

void RebootReasonItem::Init(std::string_view cmdline, const char* shim_name, FILE* log) {
  size_t boot_arg = cmdline.rfind(kBootArg);

  // No reboot reason.
  if (boot_arg == std::string_view::npos) {
    return;
  }

  std::string_view option = cmdline.substr(boot_arg + kBootArg.size());
  option = option.substr(0, option.find_first_of(' '));

  if (option.empty()) {
    fprintf(log, "%s: ERROR %.*s was empty, no reboot reason.", shim_name,
            static_cast<int>(kBootArg.size() - 1), kBootArg.data());
    return;
  }

  // options format: reason,sub-reason*,detail0*,...,detailN* with the following fields being
  // optional.
  std::string_view reboot_reason = option.substr(0, option.find_first_of(','));

  for (auto [reason, value] : kRebootReasons) {
    if (reboot_reason == reason) {
      set_payload(value);
      return;
    }
  }
  fprintf(log, "%s: ERROR %.*s was <%.*s>, no known reboot reason.", shim_name,
          static_cast<int>(kBootArg.size() - 1), kBootArg.data(),
          static_cast<int>(reboot_reason.size() - 1), reboot_reason.data());
}

}  // namespace boot_shim
