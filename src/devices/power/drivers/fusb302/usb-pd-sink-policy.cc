// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/fusb302/usb-pd-sink-policy.h"

#include <lib/ddk/debug.h>
#include <zircon/assert.h>

#include <cinttypes>
#include <optional>
#include <utility>

#include "src/devices/power/drivers/fusb302/usb-pd-message-objects.h"

namespace usb_pd {

bool SinkPolicyInfo::IsValid() const {
  if (min_voltage_mv <= 0) {
    return false;
  }
  if (max_voltage_mv < min_voltage_mv) {
    return false;
  }

  if (max_power_mw <= 0) {
    return false;
  }
  return true;
}

SinkPolicy::SinkPolicy(const SinkPolicyInfo& policy_info) : policy_info_(policy_info) {
  ZX_DEBUG_ASSERT(policy_info.IsValid());

  PopulateSinkCapabilities();
}

SinkPolicy::~SinkPolicy() = default;

void SinkPolicy::DidReceiveSourceCapabilities(const Message& capabilities) {
  // clear() doesn't currently work for elements without a default constructor.
  source_capabilities_ = {};

  for (uint32_t capability : capabilities.data_objects()) {
    source_capabilities_.push_back(PowerData(capability));
  }
}

// A score for how well a power source offering suits our sink's needs.
struct SinkPolicy::PowerDataSuitability {
  int32_t power_mw = 0;
  int32_t voltage_mv = 0;

  bool operator<(const PowerDataSuitability& rhs) const {
    if (power_mw != rhs.power_mw) {
      return power_mw < rhs.power_mw;
    }
    // Prefer the lowest voltage that hits a power goal.
    return voltage_mv > rhs.voltage_mv;
  }
};

PowerRequestData SinkPolicy::GetPowerRequest() const {
  ZX_DEBUG_ASSERT_MSG(source_capabilities_.size() != 0,
                      "DidReceiveSourceCapabilities() not called");

  PowerDataSuitability best_suitability;
  std::optional<PowerRequestData> best_request_data;

  // The cast does not overflow (causing UB) because the maximum vector size is
  // `Header::kMaxDataObjectCount`.
  const int32_t capabilities_count = static_cast<int32_t>(source_capabilities_.size());
  for (int i = 0; i < capabilities_count; ++i) {
    const int32_t position = i + 1;
    const PowerData power_data = source_capabilities_[i];

    switch (power_data.supply_type()) {
      case PowerSupplyType::kFixedSupply: {
        auto [request_data, suitability] =
            ScoreFixedPower(FixedPowerSupplyData(power_data), position);
        if (best_suitability < suitability) {
          best_suitability = suitability;
          best_request_data = request_data;
        }
        break;
      }
      default:
        zxlogf(WARNING, "Skipping unsupported power type %" PRIu8,
               static_cast<unsigned char>(power_data.supply_type()));
        break;
    }
  }

  // usbpd3.1 guarantees that we'll find at least one suitable source.
  return SetCommonRequestFields(best_request_data.value());
}

cpp20::span<const uint32_t> SinkPolicy::GetSinkCapabilities() const { return sink_capabilities_; }

void SinkPolicy::PopulateSinkCapabilities() {
  ZX_DEBUG_ASSERT_MSG(sink_capabilities_.empty(), "PopulateSinkCapabilities() already called");

  // Fixed Supply PDOs must come first in a Capabilities message.
  static constexpr int32_t kVoltagesMv[] = {5'000, 9'000, 10'1000, 12'000, 15'000, 20'000};
  for (int32_t voltage_mv : kVoltagesMv) {
    if (voltage_mv < policy_info_.min_voltage_mv || voltage_mv > policy_info_.max_voltage_mv) {
      continue;
    }

    const int32_t max_power_uw = policy_info_.max_power_mw * 1'000;

    // Using ceiling division to get an upper bound on the power consumption.
    const int32_t current_ma = (max_power_uw + voltage_mv - 1) / voltage_mv;

    SinkFixedPowerSupplyData fixed_supply;
    fixed_supply.set_voltage_mv(voltage_mv).set_maximum_current_ma(current_ma);
    sink_capabilities_.push_back(static_cast<uint32_t>(fixed_supply));
  }

  // The first PDO must be a Fixed Supply PDO with vSafe5V.
  ZX_DEBUG_ASSERT(!sink_capabilities_.empty());
  const PowerData first_power_data(sink_capabilities_[0]);
  const SinkFixedPowerSupplyData first_fixed_supply(first_power_data);
  ZX_DEBUG_ASSERT(first_fixed_supply.voltage_mv() == 5'000);

  sink_capabilities_[0] = static_cast<uint32_t>(SetAdditionalInformation(first_fixed_supply));
}

std::pair<PowerRequestData, SinkPolicy::PowerDataSuitability> SinkPolicy::ScoreFixedPower(
    FixedPowerSupplyData power_data, int32_t data_position) const {
  auto request_data = FixedVariableSupplyPowerRequestData::CreateForPosition(data_position);
  int32_t voltage_mv = power_data.voltage_mv();
  if (voltage_mv < policy_info_.min_voltage_mv || voltage_mv > policy_info_.max_voltage_mv) {
    return {request_data, {}};
  }

  const int32_t max_power_uw = policy_info_.max_power_mw * 1'000;

  // We use ceiling division because the requested current must be an upper
  // bound for the actual consumption.
  int32_t requested_current_ma =
      std::min((max_power_uw + voltage_mv - 1) / voltage_mv, power_data.maximum_current_ma());

  request_data.set_limit_current_ma(requested_current_ma)
      .set_operating_current_ma(requested_current_ma);

  // The multiplication does not overflow (causing UB) because the result is at
  // most 523,264,500 uV. Due to the USB PD format, `voltage_mv` is at most
  // 51,150 mV, and `requested_current_ma` is at most 10,230 mA.
  const int32_t power_uw = (requested_current_ma * voltage_mv);

  // The requested power may be slightly higher than the maximum consumption,
  // due to the ceiling division above. We don't want this rounding error to
  // favor a higher voltage over a lower voltage, so we fix it up here.
  const int32_t power_mw = std::min(power_uw / 1'000, policy_info_.max_power_mw);

  PowerDataSuitability suitability = {.power_mw = power_mw, .voltage_mv = voltage_mv};
  return {request_data, suitability};
}

SinkFixedPowerSupplyData SinkPolicy::SetAdditionalInformation(
    SinkFixedPowerSupplyData fixed_power) const {
  fixed_power.set_supports_dual_role_power(false)
      .set_requires_more_than_5v(false)
      .set_supports_usb_communications(policy_info_.supports_usb_communications)
      .set_supports_dual_role_data(false)
      .set_fast_swap_current_requirement(FastSwapCurrentRequirement::kNotSupported);
  return fixed_power;
}

PowerRequestData SinkPolicy::SetCommonRequestFields(PowerRequestData request) const {
  request.set_supports_power_give_back(false)
      .set_prefers_waiving_usb_suspend(true)
      .set_supports_usb_communications(policy_info_.supports_usb_communications)
      .set_supports_unchunked_extended_messages(false)
      .set_supports_extended_power_range(false);
  return request;
}

}  // namespace usb_pd
