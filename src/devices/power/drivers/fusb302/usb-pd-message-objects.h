// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_POWER_DRIVERS_FUSB302_USB_PD_MESSAGE_OBJECTS_H_
#define SRC_DEVICES_POWER_DRIVERS_FUSB302_USB_PD_MESSAGE_OBJECTS_H_

// The comments in this file reference the USB Power Delivery Specification,
// downloadable at https://usb.org/document-library/usb-power-delivery
//
// usbpd3.1 is Revision 3.1, Version 1.7, published January 2023.

#include <zircon/assert.h>

#include <cstdint>

#include <hwreg/bitfields.h>

namespace usb_pd {

// Power supply type
//
// usbpd3.1 6.4.1 "Capabilities Message", Table 6-7 "Power Data Object"
enum class PowerSupplyType : uint8_t {
  kFixedSupply = 0b00,
  kBattery = 0b01,
  kVariableSupply = 0b10,
  kAugmentedPowerDataObject = 0b11,  // APDO
};

// Precursor for all PDO (Power Data Object) types.
//
// Conceptually, the PDO in the USB PD spec is a sum type (like std::variant).
// Values have 32 bits, and the top 2 bits are used as the type discriminant.
// `PowerData` contains the logic for encoding and decoding the discriminant.
//
// This high-level representation is defined in the introduction of usbpd3.1
// usbpd3.1 6.4.1 "Capabilities Message". Subtypes are defined in the
// subsections of usbpd3.1 6.4.1.2 "Source_Capabilities Message" and of 6.4.1.3
// "Sink Capabilities Message"
struct PowerData {
 public:
  uint32_t bits;

  // Discriminant for PDO (Powey Data Object) types.
  DEF_ENUM_SUBFIELD(bits, PowerSupplyType, 31, 30, supply_type);

  // Support explicit casting from uint32_t.
  explicit PowerData(uint32_t bits) : bits(bits) {}

  // Value type, copying is allowed.
  PowerData(const PowerData&) = default;
  PowerData& operator=(const PowerData&) = default;

  // Trivially destructible.
  ~PowerData() = default;

  // Support explicit casting to uint32_t.
  explicit operator uint32_t() const { return bits; }

  // In C++20, equality comparison can be defaulted.
  bool operator==(const PowerData& other) const { return bits == other.bits; }
  bool operator!=(const PowerData& other) const { return bits != other.bits; }
};

// A fixed power source's ability to briefly exceed the operating current.
//
// usbpd3.1 measures the Source's ability to handle spikes of three maximum
// durations (1ms / 2ms / 10ms) out of a 20ms duty cycle. In all cases, the
// current consumption over an entire duty cycle must still match the source's
// operating current. So, a spike must be compensated by lower current
// consumption during the rest of the duty cycle.
//
// usbpd3.1 6.4.1.2.2.8 "Peak Current"
enum class PeakCurrentSupport {
  kNoOverload = 0b00,

  // Overload limit: 150% for 1ms / 125% for 2ms / 110% for 10ms
  kOverloadLevel1 = 0b01,

  // Overload limit: 200% for 1ms / 150% for 2ms / 125% for 10ms
  kOverloadLevel2 = 0b10,

  // Overload limit: 200% for 1ms / 175% for 2ms / 150% for 10ms
  kOverloadLevel3 = 0b11,
};

// Well-regulated power supply that maintains a fixed voltage.
//
// See `PowerData` for a general description of the PDO (Power Data Object)
// representation.
//
// usbpd3.1 6.4.1.2.2 "Fixed Supply Power Data Object"
class FixedPowerSupplyData {
 private:
  // Needed for the following to compile.
  uint32_t bits_;

 public:
  // Indicates support for the PR_Swap message.
  //
  // Only used on the first PDO in a Source_Capabilities message.
  DEF_SUBBIT(bits_, 29, supports_dual_role_power);

  // Indicates whether the Sink must follow USB suspend rules.
  //
  // usbpd3.1 names this field USB Suspend Supported, but it conveys
  // requirements for the Sink. The requirements apply after a PD Contract is
  // negotiated. 6.4.1.2.2 "USB Suspend Supported", states that a peripheral may
  // draw up to 25 mW (pSnkSusp) and a hub may draw up to 125 mW (pHubSusp)
  // during suspend, and defers to usb2, usb3.2 and usb4.
  //
  // Only used on the first PDO in a Source_Capabilities message.
  DEF_SUBBIT(bits_, 28, requires_usb_suspend);

  // True iff the Source draws power from an "unlimited" power source.
  //
  // Only used on the first PDO in a Source_Capabilities message.
  DEF_SUBBIT(bits_, 27, has_unconstrained_power);

  // Indicates if the Source can communicate over a USB data channel.
  //
  // The data channels are D+/- introduced in usb2.0 and SS Tx/Rx introduced in
  // usb3.0.
  //
  // Only used on the first PDO in a Source_Capabilities message.
  DEF_SUBBIT(bits_, 26, supports_usb_communications);

  // Indicates support for the DR_Swap message.
  //
  // Only used on the first PDO in a Source_Capabilities message.
  DEF_SUBBIT(bits_, 25, supports_dual_role_data);

  // True iff the Port can receive messages with more than 26 data bytes.
  //
  // If true, the underlying BMC PHY must be designed to receive and transmit
  // extended messages up to 260-bytes.
  //
  // Only used on the first PDO in a Source_Capabilities message.
  DEF_SUBBIT(bits_, 24, supports_unchunked_extended_messages);

  // True iff the Source supports EPR (Extended Power Range) messages.
  //
  // EPR power sources can supply more than 100 W.
  //
  // Only used on the first PDO in a Source_Capabilities message.
  DEF_SUBBIT(bits_, 23, supports_extended_power_range);

  // The power source's ability to briefly exceed the operating current.
  DEF_ENUM_SUBFIELD(bits_, PeakCurrentSupport, 21, 20, peak_current);

  // The power source's fixed voltage, in multiplies of 50 mV.
  DEF_SUBFIELD(bits_, 19, 10, voltage_50mv);

  // The maximum current offered by the source, in multiples of 10 mA.
  DEF_SUBFIELD(bits_, 9, 0, maximum_current_10ma);

  // The power source's fixed voltage, in mV (millivolts).
  int32_t voltage_mv() const {
    // The multiplication will not overflow (causing UB) because
    // `voltage_50mv` is a 10-bit field. The maximum product is 51,150.
    return static_cast<int32_t>(static_cast<int32_t>(voltage_50mv()) * 50);
  }
  FixedPowerSupplyData& set_voltage_mv(int32_t voltage_mv) {
    return set_voltage_50mv(voltage_mv / 50);
  }

  // The maximum current offered by the source, in mA (milliamperes).
  int32_t maximum_current_ma() const {
    // The multiplication will not overflow (causing UB) because
    // `maximum_current_10ma` is a 10-bit field. The maximum product is 10,240.
    return static_cast<int32_t>(static_cast<int32_t>(maximum_current_10ma()) * 10);
  }
  FixedPowerSupplyData& set_maximum_current_ma(int32_t current_ma) {
    return set_maximum_current_10ma(current_ma / 10);
  }

  // Debug-checked casting from PowerData.
  explicit FixedPowerSupplyData(PowerData power_data) : bits_(power_data.bits) {
    ZX_DEBUG_ASSERT(supply_type() == PowerSupplyType::kFixedSupply);
  }

  // Instance with all fields except for type set to zero.
  //
  // This is an invalid PDO (power data object). At a minimum, voltage and
  // maximum current must be set before use in a PD message.
  FixedPowerSupplyData()
      : FixedPowerSupplyData(PowerData(0).set_supply_type(PowerSupplyType::kFixedSupply)) {}

  // Value type, copying is allowed.
  FixedPowerSupplyData(const FixedPowerSupplyData&) = default;
  FixedPowerSupplyData& operator=(const FixedPowerSupplyData&) = default;

  // Trivially destructible.
  ~FixedPowerSupplyData() = default;

  // Support explicit casting to uint32_t.
  explicit operator uint32_t() const { return bits_; }

  // In C++20, equality comparison can be defaulted.
  bool operator==(const FixedPowerSupplyData& other) const { return bits_ == other.bits_; }
  bool operator!=(const FixedPowerSupplyData& other) const { return bits_ != other.bits_; }

 private:
  // Must be kFixedSupply.
  DEF_ENUM_SUBFIELD(bits_, PowerSupplyType, 31, 30, supply_type);
};

// Power supply that lacks good voltage regulation, so its voltage may vary.
//
// See `PowerData` for a general description of the PDO (Power Data Object)
// representation.
//
// usbpd3.1 6.4.1.2.3 "Variable Supply (non-Battery) Power Data Object"
class VariablePowerSupplyData {
 private:
  // Needed for the following to compile.
  uint32_t bits_;

 public:
  // The power source's maximum voltage, in multiplies of 50 mV.
  DEF_SUBFIELD(bits_, 29, 20, maximum_voltage_50mv);

  // The power source's minimum voltage, in multiplies of 50 mV.
  DEF_SUBFIELD(bits_, 19, 10, minimum_voltage_50mv);

  // The maximum current offered by the source, in multiples of 10 mA.
  DEF_SUBFIELD(bits_, 9, 0, maximum_current_10ma);

  // The power source's maximum voltage, in mV (millivolts).
  int32_t maximum_voltage_mv() const {
    // The multiplication will not overflow (causing UB) because
    // `maximum_voltage_50mv` is a 10-bit field. The maximum product is 51,150.
    return static_cast<int32_t>(static_cast<int32_t>(maximum_voltage_50mv()) * 50);
  }
  VariablePowerSupplyData& set_maximum_voltage_mv(int32_t voltage_mv) {
    return set_maximum_voltage_50mv(voltage_mv / 50);
  }

  // The power source's minimum voltage, in mV (millivolts).
  int32_t minimum_voltage_mv() const {
    return static_cast<int32_t>(static_cast<int32_t>(minimum_voltage_50mv()) * 50);
  }
  VariablePowerSupplyData& set_minimum_voltage_mv(int32_t voltage_mv) {
    // The multiplication will not overflow (causing UB) because
    // `minimum_voltage_50mv` is a 10-bit field. The maximum product is 51,150.
    return set_minimum_voltage_50mv(voltage_mv / 50);
  }

  // The maximum current offered by the source, in mA (milliamperes).
  int32_t maximum_current_ma() const {
    // The multiplication will not overflow (causing UB) because
    // `maximum_current_10ma` is a 10-bit field. The maximum product is 10,240.
    return static_cast<int32_t>(static_cast<int32_t>(maximum_current_10ma()) * 10);
  }
  VariablePowerSupplyData& set_maximum_current_ma(int32_t current_ma) {
    return set_maximum_current_10ma(current_ma / 10);
  }

  // Debug-checked casting from PowerData.
  explicit VariablePowerSupplyData(PowerData power_data) : bits_(power_data.bits) {
    ZX_DEBUG_ASSERT(supply_type() == PowerSupplyType::kVariableSupply);
  }

  // Instance with all fields except for type set to zero.
  //
  // This is an invalid PDO (power data object). At a minimum, voltage and
  // maximum current must be set before use in a PD message.
  VariablePowerSupplyData()
      : VariablePowerSupplyData(PowerData(0).set_supply_type(PowerSupplyType::kVariableSupply)) {}

  // Value type, copying is allowed.
  VariablePowerSupplyData(const VariablePowerSupplyData&) = default;
  VariablePowerSupplyData& operator=(const VariablePowerSupplyData&) = default;

  // Trivially destructible.
  ~VariablePowerSupplyData() = default;

  // Support explicit casting to uint32_t.
  explicit operator uint32_t() const { return bits_; }

  // In C++20, equality comparison can be defaulted.
  bool operator==(const VariablePowerSupplyData& other) const { return bits_ == other.bits_; }
  bool operator!=(const VariablePowerSupplyData& other) const { return bits_ != other.bits_; }

 private:
  // Must be kVariableSupply.
  DEF_ENUM_SUBFIELD(bits_, PowerSupplyType, 31, 30, supply_type);
};

// Power supply that lacks good voltage regulation, so its voltage may vary.
//
// See `PowerData` for a general description of the PDO (Power Data Object)
// representation.
//
// usbpd3.1 6.4.1.2.4 "Variable Supply (non-Battery) Power Data Object"
class BatteryPowerSupplyData {
 private:
  // Needed for the following to compile.
  uint32_t bits_;

 public:
  // The power source's maximum voltage, in multiplies of 50 mV.
  DEF_SUBFIELD(bits_, 29, 20, maximum_voltage_50mv);

  // The power source's minimum voltage, in multiplies of 50 mV.
  DEF_SUBFIELD(bits_, 19, 10, minimum_voltage_50mv);

  // The maximum power offered by the source, in multiples of 250 mW.
  DEF_SUBFIELD(bits_, 9, 0, maximum_power_250mw);

  // The power source's maximum voltage, in mV (millivolts).
  int32_t maximum_voltage_mv() const {
    // The multiplication will not overflow (causing UB) because
    // `maximum_voltage_50mv` is a 10-bit field. The maximum product is 51,150.
    return static_cast<int32_t>(static_cast<int32_t>(maximum_voltage_50mv()) * 50);
  }
  BatteryPowerSupplyData& set_maximum_voltage_mv(int32_t voltage_mv) {
    return set_maximum_voltage_50mv(voltage_mv / 50);
  }

  // The power source's minimum voltage, in mV (millivolts).
  int32_t minimum_voltage_mv() const {
    // The multiplication will not overflow (causing UB) because
    // `minimum_voltage_50mv` is a 10-bit field. The maximum product is 51,150.
    return static_cast<int32_t>(static_cast<int32_t>(minimum_voltage_50mv()) * 50);
  }
  BatteryPowerSupplyData& set_minimum_voltage_mv(int32_t voltage_mv) {
    return set_minimum_voltage_50mv(voltage_mv / 50);
  }

  // The maximum power offered by the source, in mW (milliwatts).
  int32_t maximum_power_mw() const {
    // The multiplication will not overflow (causing UB) because
    // `maximum_power_250mw` is a 10-bit field. The maximum product is 255,750.
    return static_cast<int32_t>(static_cast<int32_t>(maximum_power_250mw()) * 250);
  }
  BatteryPowerSupplyData& set_maximum_power_mw(int32_t power_ma) {
    return set_maximum_power_250mw(power_ma / 250);
  }

  // Debug-checked casting from PowerData.
  explicit BatteryPowerSupplyData(PowerData power_data) : bits_(power_data.bits) {
    ZX_DEBUG_ASSERT(supply_type() == PowerSupplyType::kBattery);
  }

  // Instance with all fields except for type set to zero.
  //
  // This is an invalid PDO (power data object). At a minimum, voltage and
  // maximum current must be set before use in a PD message.
  BatteryPowerSupplyData()
      : BatteryPowerSupplyData(PowerData(0).set_supply_type(PowerSupplyType::kBattery)) {}

  // Value type, copying is allowed.
  BatteryPowerSupplyData(const BatteryPowerSupplyData&) = default;
  BatteryPowerSupplyData& operator=(const BatteryPowerSupplyData&) = default;

  // Trivially destructible.
  ~BatteryPowerSupplyData() = default;

  // Support explicit casting to uint32_t.
  explicit operator uint32_t() const { return bits_; }

  // In C++20, equality comparison can be defaulted.
  bool operator==(const BatteryPowerSupplyData& other) const { return bits_ == other.bits_; }
  bool operator!=(const BatteryPowerSupplyData& other) const { return bits_ != other.bits_; }

 private:
  // Must be kBattery.
  DEF_ENUM_SUBFIELD(bits_, PowerSupplyType, 31, 30, supply_type);
};

}  // namespace usb_pd

#endif  // SRC_DEVICES_POWER_DRIVERS_FUSB302_USB_PD_MESSAGE_OBJECTS_H_
