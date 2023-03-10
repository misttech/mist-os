// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_POWER_DRIVERS_FUSB302_USB_PD_DEFS_H_
#define SRC_DEVICES_POWER_DRIVERS_FUSB302_USB_PD_DEFS_H_

// The comments in this file reference the USB Power Delivery Specification,
// downloadable at https://usb.org/document-library/usb-power-delivery
//
// usbpd3.1 is Revision 3.1, Version 1.7, published January 2023.

#include <zircon/assert.h>

#include <cstdint>

namespace usb_pd {

// Type-safe selector for a USB power role.
//
// The values match usbpd3.1 6.2.1.1.4 "Port Power Role". So, instances can be
// bit-copied into PD message headers.
enum class PowerRole : int8_t {
  kSink = 0,
  kSource = 1,
};

// Descriptor for logging and debugging.
const char* DebugStringFromPowerRole(PowerRole power_role);

// Specification Revision
//
// The values match usbpd3.1 6.2.1.1.5 "Specification Revision". So, instances
// can be bit-copied into PD message headers.
enum class SpecRevision : uint8_t {
  kRev1 = 0b00,
  kRev2 = 0b01,
  kRev3 = 0b10,
};

// Type-safe selector for a USB data role.
//
// The values match usbpd3.1 6.2.1.1.6 "Port Data Role". So, instances can be
// bit-copied into PD message headers.
enum class DataRole : uint8_t {
  kUpstreamFacingPort = 0,    // UFP
  kDownstreamFacingPort = 1,  // DFP
};

// Descriptor for logging and debugging.
const char* DebugStringFromDataRole(DataRole data_role);

// Type-safe selector for one of the two CC (Configuration Channel) pins.
//
// The Configuration Channel is introduced in usbpd3.1 2.1 "Introduction", and
// its role is summarized in usbpd3.1 2.3 "Configuration Process".
enum class ConfigChannelPinId : int8_t {
  kCc1 = 1,
  kCc2 = 2,
};

// Descriptor for logging and debugging.
const char* DebugStringFromConfigChannelPinId(ConfigChannelPinId pin_id);

// State of a switch that can either point to a CC pin or be open.
//
// See `ConfigChannelPinId` for a description of the CC (Configuration Channel)
// pins.
enum class ConfigChannelPinSwitch : int8_t {
  kNone = 0,
  kCc1 = 1,
  kCc2 = 2,
};

// Descriptor for logging and debugging.
const char* DebugStringFromConfigChannelPinSwitch(ConfigChannelPinSwitch cc_switch);

// Measured voltage of a CC pin.
enum class ConfigChannelTermination : int8_t {
  kUnknown = 0,
  kOpen = 1,
  kRa = 2,
  kRd = 3,
  kRpStandardUsb = 4,
  kRp1500mA = 5,
  kRp3000mA = 6,
};

// Descriptor for logging and debugging.
const char* DebugStringFromConfigChannelTermination(ConfigChannelTermination termination);

// Inverts a ConfigChannelPinId.
constexpr ConfigChannelPinId ConfigChannelPinIdFromInverse(ConfigChannelPinId id) {
  return (id == ConfigChannelPinId::kCc1) ? ConfigChannelPinId::kCc2 : ConfigChannelPinId::kCc1;
}

// `cc_switch` must not be `kNone`.
constexpr ConfigChannelPinId ConfigChannelPinIdFromSwitch(ConfigChannelPinSwitch cc_switch) {
  ZX_DEBUG_ASSERT(cc_switch != ConfigChannelPinSwitch::kNone);
  return static_cast<ConfigChannelPinId>(cc_switch);
}

}  // namespace usb_pd

#endif  // SRC_DEVICES_POWER_DRIVERS_FUSB302_USB_PD_DEFS_H_
