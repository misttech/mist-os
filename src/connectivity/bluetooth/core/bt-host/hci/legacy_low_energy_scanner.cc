// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci/legacy_low_energy_scanner.h"

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci/advertising_report_parser.h"

#pragma clang diagnostic ignored "-Wswitch-enum"

namespace bt::hci {
namespace pwemb = pw::bluetooth::emboss;

bool LegacyLowEnergyScanner::DeviceAddressFromAdvReport(
    const pw::bluetooth::emboss::LEAdvertisingReportDataView& report,
    DeviceAddress* out_addr,
    bool* out_resolved) {
  pw::bluetooth::emboss::LEAddressType le_addr_type =
      report.address_type().Read();

  *out_resolved =
      le_addr_type == pw::bluetooth::emboss::LEAddressType::PUBLIC_IDENTITY ||
      le_addr_type == pw::bluetooth::emboss::LEAddressType::RANDOM_IDENTITY;

  std::optional<DeviceAddress::Type> addr_type =
      DeviceAddress::LeAddrToDeviceAddr(le_addr_type);

  if (!addr_type || *addr_type == DeviceAddress::Type::kLEAnonymous) {
    bt_log(WARN, "hci", "invalid address type in advertising report");
    return false;
  }

  *out_addr = DeviceAddress(*addr_type, DeviceAddressBytes(report.address()));

  return true;
}

LegacyLowEnergyScanner::LegacyLowEnergyScanner(
    LocalAddressDelegate* local_addr_delegate,
    Transport::WeakPtr transport,
    pw::async::Dispatcher& pw_dispatcher)
    : LowEnergyScanner(
          local_addr_delegate, std::move(transport), pw_dispatcher),
      weak_self_(this) {
  auto self = weak_self_.GetWeakPtr();
  event_handler_id_ = hci()->command_channel()->AddLEMetaEventHandler(
      hci_spec::kLEAdvertisingReportSubeventCode,
      [self](const EmbossEventPacket& event) {
        if (!self.is_alive()) {
          return hci::CommandChannel::EventCallbackResult::kRemove;
        }

        self->OnAdvertisingReportEvent(event);
        return hci::CommandChannel::EventCallbackResult::kContinue;
      });
}

LegacyLowEnergyScanner::~LegacyLowEnergyScanner() {
  // This object is probably being destroyed because the stack is shutting down,
  // in which case the HCI layer may have already been destroyed.
  if (!hci().is_alive() || !hci()->command_channel()) {
    return;
  }

  hci()->command_channel()->RemoveEventHandler(event_handler_id_);
  StopScan();
}

bool LegacyLowEnergyScanner::StartScan(const ScanOptions& options,
                                       ScanStatusCallback callback) {
  BT_ASSERT(options.interval >= hci_spec::kLEScanIntervalMin);
  BT_ASSERT(options.interval <= hci_spec::kLEScanIntervalMax);
  BT_ASSERT(options.window >= hci_spec::kLEScanIntervalMin);
  BT_ASSERT(options.window <= hci_spec::kLEScanIntervalMax);
  return LowEnergyScanner::StartScan(options, std::move(callback));
}

EmbossCommandPacket LegacyLowEnergyScanner::BuildSetScanParametersPacket(
    const DeviceAddress& local_address, const ScanOptions& options) {
  auto packet = hci::EmbossCommandPacket::New<
      pw::bluetooth::emboss::LESetScanParametersCommandWriter>(
      hci_spec::kLESetScanParameters);
  auto params = packet.view_t();

  params.le_scan_type().Write(pw::bluetooth::emboss::LEScanType::PASSIVE);
  if (options.active) {
    params.le_scan_type().Write(pw::bluetooth::emboss::LEScanType::ACTIVE);
  }

  params.le_scan_interval().Write(options.interval);
  params.le_scan_window().Write(options.window);
  params.scanning_filter_policy().Write(options.filter_policy);

  if (local_address.type() == DeviceAddress::Type::kLERandom) {
    params.own_address_type().Write(
        pw::bluetooth::emboss::LEOwnAddressType::RANDOM);
  } else {
    params.own_address_type().Write(
        pw::bluetooth::emboss::LEOwnAddressType::PUBLIC);
  }

  return packet;
}

EmbossCommandPacket LegacyLowEnergyScanner::BuildEnablePacket(
    const ScanOptions& options,
    pw::bluetooth::emboss::GenericEnableParam enable) {
  auto packet = EmbossCommandPacket::New<
      pw::bluetooth::emboss::LESetScanEnableCommandWriter>(
      hci_spec::kLESetScanEnable);
  auto params = packet.view_t();
  params.le_scan_enable().Write(enable);

  params.filter_duplicates().Write(
      pw::bluetooth::emboss::GenericEnableParam::DISABLE);
  if (options.filter_duplicates) {
    params.filter_duplicates().Write(
        pw::bluetooth::emboss::GenericEnableParam::ENABLE);
  }

  return packet;
}

void LegacyLowEnergyScanner::HandleScanResponse(const DeviceAddress& address,
                                                bool resolved,
                                                int8_t rssi,
                                                const ByteBuffer& data) {
  std::unique_ptr<PendingScanResult> pending = RemovePendingResult(address);
  if (!pending) {
    bt_log(DEBUG, "hci-le", "dropping unmatched scan response");
    return;
  }

  BT_DEBUG_ASSERT(address == pending->result().address());
  pending->result().AppendData(data);
  pending->result().set_resolved(resolved);
  pending->result().set_rssi(rssi);

  delegate()->OnPeerFound(pending->result());

  // The callback handler may stop the scan, destroying objects within the
  // LowEnergyScanner. Avoid doing anything more to prevent use after free
  // bugs.
}

void LegacyLowEnergyScanner::OnAdvertisingReportEvent(
    const EmbossEventPacket& event) {
  if (!IsScanning()) {
    return;
  }

  AdvertisingReportParser parser(event);
  pw::bluetooth::emboss::LEAdvertisingReportDataView view;
  int8_t rssi;

  while (parser.GetNextReport(&view, &rssi)) {
    if (view.data_length().Read() > hci_spec::kMaxLEAdvertisingDataLength) {
      bt_log(WARN, "hci-le", "advertising data too long! Ignoring");
      continue;
    }

    DeviceAddress address;
    bool resolved;
    if (!DeviceAddressFromAdvReport(view, &address, &resolved)) {
      continue;
    }

    bool needs_scan_rsp = false;
    bool connectable = false;
    bool directed = false;

    switch (view.event_type().Read()) {
      case pwemb::LEAdvertisingEventType::CONNECTABLE_DIRECTED: {
        directed = true;
        break;
      }
      case pwemb::LEAdvertisingEventType::
          CONNECTABLE_AND_SCANNABLE_UNDIRECTED: {
        connectable = true;
        [[fallthrough]];
      }
      case pwemb::LEAdvertisingEventType::SCANNABLE_UNDIRECTED: {
        if (IsActiveScanning()) {
          needs_scan_rsp = true;
        }
        break;
      }
      case pwemb::LEAdvertisingEventType::SCAN_RESPONSE: {
        if (IsActiveScanning()) {
          BufferView data = BufferView(view.data().BackingStorage().data(),
                                       view.data_length().Read());
          HandleScanResponse(address, resolved, rssi, data);
        }
        continue;
      }
      default: {
        break;
      }
    }

    LowEnergyScanResult result(address, resolved, connectable);
    result.AppendData(BufferView(view.data().BackingStorage().data(),
                                 view.data_length().Read()));
    result.set_rssi(rssi);

    if (directed) {
      delegate()->OnDirectedAdvertisement(result);
      continue;
    }

    if (!needs_scan_rsp) {
      delegate()->OnPeerFound(result);
      continue;
    }

    AddPendingResult(std::move(result));
  }
}

}  // namespace bt::hci
