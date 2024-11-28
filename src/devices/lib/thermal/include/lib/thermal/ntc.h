// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_LIB_THERMAL_INCLUDE_LIB_THERMAL_NTC_H_
#define SRC_DEVICES_LIB_THERMAL_INCLUDE_LIB_THERMAL_NTC_H_

#include <zircon/assert.h>
#include <zircon/types.h>

#include <algorithm>
#include <vector>

namespace thermal {

static constexpr uint32_t kMaxProfileLen = 200;
static constexpr uint32_t kMaxNameLen = 50;

struct NtcTable {
  float temperature_c;
  uint32_t resistance_ohm;
};

struct NtcChannel {
  uint32_t adc_channel;
  uint32_t pullup_ohms;
  uint32_t profile_idx;
  char name[kMaxNameLen];
};

struct NtcInfo {
  char part[kMaxNameLen];
  NtcTable profile[kMaxProfileLen];  // profile table should be sorted in decreasing resistance
};

class Ntc {
 public:
  Ntc(NtcInfo ntc_info, uint32_t pullup_ohms)
      : profile_(ntc_info.profile, ntc_info.profile + kMaxProfileLen), pullup_ohms_(pullup_ohms) {
    // Since all entries in profile table may not have been used (passed as metadata) check to make
    // sure resistance isn't out of range of the table.
    std::erase_if(profile_,
                  [](NtcTable const& table) { return table.resistance_ohm == kInvalidResistance; });
    // Sort profile table descending by resistance to ensure proper lookup
    auto sort_compare = [](NtcTable const& x, NtcTable const& y) -> bool {
      return x.resistance_ohm > y.resistance_ohm;
    };
    std::sort(profile_.begin(), profile_.end(), sort_compare);
  }
  // we use a normalized sample [0-1] to prevent having to worry about adc resolution
  //  in this library. This assumes the call site will normalize the value appropriately
  // Since the thermistor is in series with a pullup resistor, we must convert our sample
  //  value to a resistance then lookup in the profile table.
  zx_status_t GetTemperatureCelsius(float norm_sample, float* out) const {
    // norm_sample should never be 1.0 because that would mean there is no pullup resistor. Also,
    // this ensures that division below is valid.
    if ((norm_sample < 0) || (norm_sample >= 1.0)) {
      return ZX_ERR_INVALID_ARGS;
    }
    float ratio = -(norm_sample) / (norm_sample - 1);
    float resistance_f = ratio * static_cast<float>(pullup_ohms_);
    return LookupCelsius(resistance_f, out);
  }

  // Returns the normalized sample. Convert from resistance.
  zx_status_t GetNormalizedSample(float temperature_c, float* out) const {
    float resistance;
    zx_status_t status = LookupResistance(temperature_c, &resistance);
    if (status != ZX_OK) {
      return status;
    }
    *out = resistance / (resistance + static_cast<float>(pullup_ohms_));
    return ZX_OK;
  }

 private:
  zx_status_t LookupCelsius(float resistance, float* out) const {
    if (profile_.empty()) {
      return ZX_ERR_NO_RESOURCES;
    }

    if (resistance >= static_cast<float>(profile_[0].resistance_ohm)) {
      *out = profile_[0].temperature_c;
      return ZX_OK;
    }
    if (resistance <= static_cast<float>(profile_[profile_.size() - 1].resistance_ohm)) {
      *out = profile_[profile_.size() - 1].temperature_c;
      return ZX_OK;
    }

    auto lb_compare = [](NtcTable const& lhs, float val) -> bool {
      return static_cast<float>(lhs.resistance_ohm) > val;
    };
    auto low = std::lower_bound(profile_.begin(), profile_.end(), resistance, lb_compare);
    size_t idx = (low - profile_.begin());

    *out = LinearInterpolate(static_cast<float>(profile_.at(idx).resistance_ohm),
                             profile_.at(idx).temperature_c,
                             static_cast<float>(profile_.at(idx - 1).resistance_ohm),
                             profile_.at(idx - 1).temperature_c, resistance);
    return ZX_OK;
  }

  zx_status_t LookupResistance(float temperature_c, float* out) const {
    if (profile_.empty()) {
      return ZX_ERR_NO_RESOURCES;
    }

    if (temperature_c <= profile_[0].temperature_c) {
      *out = static_cast<float>(profile_[0].resistance_ohm);
      return ZX_OK;
    }
    if (temperature_c >= profile_[profile_.size() - 1].temperature_c) {
      *out = static_cast<float>(profile_[profile_.size() - 1].resistance_ohm);
      return ZX_OK;
    }

    auto lb_compare = [](NtcTable const& lhs, float val) -> bool {
      return lhs.temperature_c < val;
    };
    auto lower = std::lower_bound(profile_.begin(), profile_.end(), temperature_c, lb_compare);
    size_t idx = (lower - profile_.begin());

    *out = LinearInterpolate(
        profile_.at(idx).temperature_c, static_cast<float>(profile_.at(idx).resistance_ohm),
        profile_.at(idx - 1).temperature_c, static_cast<float>(profile_.at(idx - 1).resistance_ohm),
        temperature_c);
    return ZX_OK;
  }

  // Find y for (x, y) on the line of (x1, y1) (x2, y2)
  static constexpr float LinearInterpolate(float x1, float y1, float x2, float y2, float x) {
    float scale = (x - x1) / (x2 - x1);
    return scale * (y2 - y1) + y1;
  }

  static constexpr uint32_t kInvalidResistance = 0;
  std::vector<NtcTable> profile_;
  uint32_t pullup_ohms_ = 0;
};
}  // namespace thermal

#endif  // SRC_DEVICES_LIB_THERMAL_INCLUDE_LIB_THERMAL_NTC_H_
