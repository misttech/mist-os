// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_GAP_LOW_ENERGY_STATE_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_GAP_LOW_ENERGY_STATE_H_

#include <cstdint>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/constants.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/data_buffer_info.h"

namespace bt::gap {

// Stores Bluetooth Low Energy settings and state information.
class LowEnergyState final {
 public:
  // Returns true if |feature_bit| is set as supported in the local LE features
  // list.
  inline bool IsFeatureSupported(
      hci_spec::LESupportedFeature feature_bit) const {
    return supported_features_ & static_cast<uint64_t>(feature_bit);
  }

  uint64_t supported_features() const { return supported_features_; }

  // Returns the LE ACL data buffer capacity.
  const hci::DataBufferInfo& acl_data_buffer_info() const {
    return acl_data_buffer_info_;
  }

  // Returns the ISO data buffer capacity.
  const hci::DataBufferInfo& iso_data_buffer_info() const {
    return iso_data_buffer_info_;
  }

  uint16_t max_advertising_data_length() const {
    return max_advertising_data_length_;
  }

 private:
  friend class Adapter;
  friend class AdapterImpl;

  // Storage capacity information about the controller's internal ACL data
  // buffers.
  hci::DataBufferInfo acl_data_buffer_info_;

  // Storage capacity information about the controller's internal ISO data
  // buffers.
  hci::DataBufferInfo iso_data_buffer_info_;

  // Local supported LE Features reported by the controller.
  uint64_t supported_features_ = 0;

  // Local supported LE states reported by the controller.
  uint64_t supported_states_ = 0;

  // Maximum length of data supported by the Controller for use as advertisement
  // data or scan response data in an advertising event or as periodic
  // advertisement data
  uint16_t max_advertising_data_length_ = 0;
};

}  // namespace bt::gap

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_GAP_LOW_ENERGY_STATE_H_
