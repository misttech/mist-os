// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_EXTENDED_LOW_ENERGY_ADVERTISER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_EXTENDED_LOW_ENERGY_ADVERTISER_H_

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci/advertising_handle_map.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci/low_energy_advertiser.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/transport.h"

namespace bt::hci {

class SequentialCommandRunner;

class ExtendedLowEnergyAdvertiser final : public LowEnergyAdvertiser {
 public:
  explicit ExtendedLowEnergyAdvertiser(hci::Transport::WeakPtr hci);
  ~ExtendedLowEnergyAdvertiser() override;

  // Convert a LEAdvertisingType's properties (e.g. connectable, scannable,
  // directed, etc) to the appropriate advertising event bits for use in
  // HCI_LE_Set_Extended_Advertising_Parameters (Core Spec, Volume 4, Part E,
  // Section 7.8.53)
  using AdvertisingEventPropertiesBits = uint16_t;
  static AdvertisingEventPropertiesBits AdvertisingTypeToLegacyPduEventBits(
      pw::bluetooth::emboss::LEAdvertisingType type);

  bool AllowsRandomAddressChange() const override { return !IsAdvertising(); }
  size_t GetSizeLimit(bool extended_pdu) const override;

  // Attempt to start advertising. See LowEnergyAdvertiser::StartAdvertising for
  // full documentation.
  //
  // According to the Bluetooth Spec, Volume 4, Part E, Section 7.8.58, "the
  // number of advertising sets that can be supported is not fixed and the
  // Controller can change it at any time. The memory used to store advertising
  // sets can also be used for other purposes."
  //
  // This method may report an error if the controller cannot currently support
  // another advertising set.
  void StartAdvertising(const DeviceAddress& address,
                        const AdvertisingData& data,
                        const AdvertisingData& scan_rsp,
                        const AdvertisingOptions& options,
                        ConnectionCallback connect_callback,
                        ResultFunction<> result_callback) override;

  void StopAdvertising() override;
  void StopAdvertising(const DeviceAddress& address,
                       bool extended_pdu) override;

  void OnIncomingConnection(
      hci_spec::ConnectionHandle handle,
      pw::bluetooth::emboss::ConnectionRole role,
      const DeviceAddress& peer_address,
      const hci_spec::LEConnectionParameters& conn_params) override;

  size_t MaxAdvertisements() const override {
    return advertising_handle_map_.capacity();
  }

  // Returns the last used advertising handle that was used for an advertising
  // set when communicating with the controller.
  std::optional<hci_spec::AdvertisingHandle> LastUsedHandleForTesting() const {
    return advertising_handle_map_.LastUsedHandleForTesting();
  }

 private:
  struct StagedConnectionParameters {
    pw::bluetooth::emboss::ConnectionRole role;
    DeviceAddress peer_address;
    hci_spec::LEConnectionParameters conn_params;
  };

  struct StagedAdvertisingParameters {
    bool include_tx_power_level = false;
    int8_t selected_tx_power_level = 0;
    bool extended_pdu = false;

    void clear() {
      include_tx_power_level = false;
      selected_tx_power_level = 0;
      extended_pdu = false;
    }
  };

  EmbossCommandPacket BuildEnablePacket(
      const DeviceAddress& address,
      pw::bluetooth::emboss::GenericEnableParam enable,
      bool extended_pdu) override;

  std::optional<EmbossCommandPacket> BuildSetAdvertisingParams(
      const DeviceAddress& address,
      pw::bluetooth::emboss::LEAdvertisingType type,
      pw::bluetooth::emboss::LEOwnAddressType own_address_type,
      AdvertisingIntervalRange interval,
      bool extended_pdu) override;

  EmbossCommandPacket BuildSetAdvertisingData(const DeviceAddress& address,
                                              const AdvertisingData& data,
                                              AdvFlags flags,
                                              bool extended_pdu) override;

  EmbossCommandPacket BuildUnsetAdvertisingData(const DeviceAddress& address,
                                                bool extended_pdu) override;

  EmbossCommandPacket BuildSetScanResponse(const DeviceAddress& address,
                                           const AdvertisingData& data,
                                           bool extended_pdu) override;

  EmbossCommandPacket BuildUnsetScanResponse(const DeviceAddress& address,
                                             bool extended_pdu) override;

  EmbossCommandPacket BuildRemoveAdvertisingSet(const DeviceAddress& address,
                                                bool extended_pdu) override;

  void OnSetAdvertisingParamsComplete(const EventPacket& event) override;

  void OnCurrentOperationComplete() override;

  // Event handler for the HCI LE Advertising Set Terminated event
  void OnAdvertisingSetTerminatedEvent(const EventPacket& event);
  CommandChannel::EventHandlerId event_handler_id_;

  AdvertisingHandleMap advertising_handle_map_;
  std::queue<fit::closure> op_queue_;
  StagedAdvertisingParameters staged_advertising_parameters_;

  // Core Spec Volume 4, Part E, Section 7.8.56: Incoming connections to LE
  // Extended Advertising occur through two events: HCI_LE_Connection_Complete
  // and HCI_LE_Advertising_Set_Terminated. The HCI_LE_Connection_Complete event
  // provides the connection handle along with some other connection related
  // parameters. Notably missing is the advertising handle, which we need to
  // obtain the advertised device address. Until we receive the
  // HCI_LE_Advertising_Set_Terminated event, we stage these parameters.
  std::unordered_map<hci_spec::ConnectionHandle, StagedConnectionParameters>
      staged_connections_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(ExtendedLowEnergyAdvertiser);
};

}  // namespace bt::hci

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_EXTENDED_LOW_ENERGY_ADVERTISER_H_
