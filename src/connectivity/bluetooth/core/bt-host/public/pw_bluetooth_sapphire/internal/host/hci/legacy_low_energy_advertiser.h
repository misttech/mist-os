// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_LEGACY_LOW_ENERGY_ADVERTISER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_LEGACY_LOW_ENERGY_ADVERTISER_H_

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci/low_energy_advertiser.h"

namespace bt::hci {

class Transport;
class SequentialCommandRunner;

class LegacyLowEnergyAdvertiser final : public LowEnergyAdvertiser {
 public:
  explicit LegacyLowEnergyAdvertiser(hci::Transport::WeakPtr hci)
      : LowEnergyAdvertiser(std::move(hci)) {}
  ~LegacyLowEnergyAdvertiser() override;

  // LowEnergyAdvertiser overrides:
  size_t MaxAdvertisements() const override { return 1; }
  size_t GetSizeLimit(bool extended_pdu) const override {
    // LegacyLowEnergyAdvertiser is unable to take advantage of extended
    // advertising PDUs. Return the legacy advertising PDU size limit.
    return hci_spec::kMaxLEAdvertisingDataLength;
  }
  bool AllowsRandomAddressChange() const override {
    return !starting_ && !IsAdvertising();
  }

  // LegacyLowEnergyAdvertiser supports only a single advertising instance,
  // hence it can report additional errors in the following conditions:
  // 1. If called while a start request is pending, reports kRepeatedAttempts.
  // 2. If called while a stop request is pending, then cancels the stop request
  //    and proceeds with start.
  void StartAdvertising(const DeviceAddress& address,
                        const AdvertisingData& data,
                        const AdvertisingData& scan_rsp,
                        const AdvertisingOptions& options,
                        ConnectionCallback connect_callback,
                        ResultFunction<> result_callback) override;

  void StopAdvertising() override;

  // If called while a stop request is pending, returns false.
  // If called while a start request is pending, then cancels the start
  // request and proceeds with start.
  // Returns false if called while not advertising.
  // TODO(https://fxbug.dev/42127634): Update documentation.
  void StopAdvertising(const DeviceAddress& address,
                       bool extended_pdu) override;

  void OnIncomingConnection(
      hci_spec::ConnectionHandle handle,
      pw::bluetooth::emboss::ConnectionRole role,
      const DeviceAddress& peer_address,
      const hci_spec::LEConnectionParameters& conn_params) override;

 private:
  EmbossCommandPacket BuildEnablePacket(
      const DeviceAddress& address,
      pw::bluetooth::emboss::GenericEnableParam enable,
      bool extended_pdu) override;

  std::optional<EmbossCommandPacket> BuildSetAdvertisingParams(
      const DeviceAddress& address,
      pw::bluetooth::emboss::LEAdvertisingType type,
      pw::bluetooth::emboss::LEOwnAddressType own_address_type,
      const AdvertisingIntervalRange& interval,
      bool extended_pdu) override;

  EmbossCommandPacket BuildSetAdvertisingData(const DeviceAddress& address,
                                              const AdvertisingData& data,
                                              AdvFlags flags,
                                              bool extended_pdu) override;

  EmbossCommandPacket BuildUnsetAdvertisingData(const DeviceAddress& address,
                                                bool extended_pdu) override;

  EmbossCommandPacket BuildSetScanResponse(const DeviceAddress& address,
                                           const AdvertisingData& scan_rsp,
                                           bool extended_pdu) override;

  EmbossCommandPacket BuildUnsetScanResponse(const DeviceAddress& address,
                                             bool extended_pdu) override;

  EmbossCommandPacket BuildRemoveAdvertisingSet(const DeviceAddress& address,
                                                bool extended_pdu) override;

  // |starting_| is set to true if a start is pending.
  // |staged_params_| are the parameters that will be advertised.
  struct StagedParams {
    DeviceAddress address;
    AdvertisingData data;
    AdvertisingData scan_rsp;
    AdvertisingOptions options;
    ConnectionCallback connect_callback;
    ResultFunction<> result_callback;
  };
  std::optional<StagedParams> staged_params_;
  bool starting_ = false;
  DeviceAddress local_address_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(LegacyLowEnergyAdvertiser);
};

}  // namespace bt::hci

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_LEGACY_LOW_ENERGY_ADVERTISER_H_
