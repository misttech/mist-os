// Copyright (c) 2020 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_RECOVERY_RECOVERY_TRIGGER_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_RECOVERY_RECOVERY_TRIGGER_H_

#include <zircon/status.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/debug.h"
#include "trigger_condition.h"

namespace wlan::brcmfmac {

// This is the center controller class for the driver recovery process, managing all
// TriggerConditions, it do the initialization for them and also clear the counters for all of them
// when recovery is triggered.
class RecoveryTrigger {
 public:
  RecoveryTrigger(std::shared_ptr<std::function<zx_status_t()>> callback);
  ~RecoveryTrigger();

  // Set the counters of all TriggerConditions to 0.
  void ClearStatistics();

  // List the trigger condition objects here.
  TriggerCondition firmware_crash_;
  TriggerCondition sdio_timeout_;
  TriggerCondition ctrl_frame_response_timeout_;

  // Limits of the occurrence of trigger conditions. This value is 1 means that the recovery should
  // be trigger immediately when being updated.
  static constexpr uint32_t kFirmwareCrashThreshold = 1;
  static constexpr uint16_t kSdioTimeoutThreshold = 5;
  static constexpr uint16_t kCtrlFrameResponseTimeoutThreshold = 5;
};

}  // namespace wlan::brcmfmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_RECOVERY_RECOVERY_TRIGGER_H_
