// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/ufs/device_manager.h"

#include <lib/trace/event.h>
#include <lib/zx/clock.h>

#include "src/devices/block/drivers/ufs/ufs.h"
#include "src/devices/block/drivers/ufs/upiu/upiu_transactions.h"
#include "transfer_request_processor.h"

namespace ufs {
zx::result<std::unique_ptr<DeviceManager>> DeviceManager::Create(Ufs &controller) {
  fbl::AllocChecker ac;
  auto device_manager = fbl::make_unique_checked<DeviceManager>(&ac, controller);
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate device manager.");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(device_manager));
}

zx::result<> DeviceManager::SendLinkStartUp() {
  DmeLinkStartUpUicCommand link_startup_command(controller_);
  if (zx::result<std::optional<uint32_t>> result = link_startup_command.SendCommand();
      result.is_error()) {
    zxlogf(ERROR, "Failed to startup UFS link: %s", result.status_string());
    return result.take_error();
  }
  return zx::ok();
}

zx::result<> DeviceManager::DeviceInit() {
  zx::time device_init_start_time = zx::clock::get_monotonic();
  SetFlagUpiu set_flag_upiu(Flags::fDeviceInit);
  auto query_response = controller_.GetTransferRequestProcessor()
                            .SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(set_flag_upiu);
  if (query_response.is_error()) {
    zxlogf(ERROR, "Failed to set fDeviceInit flag: %s", query_response.status_string());
    return query_response.take_error();
  }

  zx::time device_init_time_out = device_init_start_time + zx::msec(kDeviceInitTimeoutMs);
  while (true) {
    ReadFlagUpiu read_flag_upiu(Flags::fDeviceInit);
    auto response = controller_.GetTransferRequestProcessor()
                        .SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(read_flag_upiu);
    if (response.is_error()) {
      zxlogf(ERROR, "Failed to read fDeviceInit flag: %s", response.status_string());
      return response.take_error();
    }
    uint8_t flag = response->GetResponse<FlagResponseUpiu>().GetFlag();

    if (!flag)
      break;

    if (zx::clock::get_monotonic() > device_init_time_out) {
      zxlogf(ERROR, "Wait for fDeviceInit timed out (%u ms)", kDeviceInitTimeoutMs);
      return zx::error(ZX_ERR_TIMED_OUT);
    }
    usleep(10000);
  }
  return zx::ok();
}

zx::result<> DeviceManager::GetControllerDescriptor() {
  ReadDescriptorUpiu read_device_desc_upiu(DescriptorType::kDevice);
  auto response = controller_.GetTransferRequestProcessor()
                      .SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(read_device_desc_upiu);
  if (response.is_error()) {
    zxlogf(ERROR, "Failed to read device descriptor: %s", response.status_string());
    return response.take_error();
  }
  device_descriptor_ =
      response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<DeviceDescriptor>();

  // The field definitions for VersionReg and wSpecVersion are the same.
  // wSpecVersion use big-endian byte ordering.
  auto version = VersionReg::Get().FromValue(betoh16(device_descriptor_.wSpecVersion));
  zxlogf(INFO, "UFS device version %u.%u%u", version.major_version_number(),
         version.minor_version_number(), version.version_suffix());

  zxlogf(INFO, "%u enabled LUNs found", device_descriptor_.bNumberLU);

  ReadDescriptorUpiu read_geometry_desc_upiu(DescriptorType::kGeometry);
  response = controller_.GetTransferRequestProcessor()
                 .SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(read_geometry_desc_upiu);
  if (response.is_error()) {
    zxlogf(ERROR, "Failed to read geometry descriptor: %s", response.status_string());
    return response.take_error();
  }
  geometry_descriptor_ =
      response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<GeometryDescriptor>();

  // The kDeviceDensityUnit is defined in the spec as 512.
  // qTotalRawDeviceCapacity use big-endian byte ordering.
  constexpr uint32_t kDeviceDensityUnit = 512;
  zxlogf(INFO, "UFS device total size is %lu bytes",
         betoh64(geometry_descriptor_.qTotalRawDeviceCapacity) * kDeviceDensityUnit);

  return zx::ok();
}

zx::result<> DeviceManager::CheckBootLunEnabled() {
  // Read bBootLunEn to confirm device interface is ok.
  ReadAttributeUpiu read_attribute_upiu(Attributes::bBootLunEn);
  auto query_response =
      controller_.GetTransferRequestProcessor()
          .SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(read_attribute_upiu);
  if (query_response.is_error()) {
    zxlogf(ERROR, "Failed to read bBootLunEn attribute: %s", query_response.status_string());
    return query_response.take_error();
  }
  auto attribute = query_response->GetResponse<AttributeResponseUpiu>().GetAttribute();
  zxlogf(DEBUG, "bBootLunEn 0x%0x", attribute);
  return zx::ok();
}

zx::result<UnitDescriptor> DeviceManager::ReadUnitDescriptor(uint8_t lun) {
  ReadDescriptorUpiu read_unit_desc_upiu(DescriptorType::kUnit, lun);
  auto response = controller_.GetTransferRequestProcessor()
                      .SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(read_unit_desc_upiu);
  if (response.is_error()) {
    return response.take_error();
  }
  return zx::ok(response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<UnitDescriptor>());
}

zx::result<> DeviceManager::SetReferenceClock() {
  // 26MHz is a default value written in spec.
  // UFS Specification Version 3.1, section 6.4 "Reference Clock".
  WriteAttributeUpiu write_attribute_upiu(Attributes::bRefClkFreq, AttributeReferenceClock::k26MHz);
  auto query_response =
      controller_.GetTransferRequestProcessor()
          .SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(write_attribute_upiu);
  if (query_response.is_error()) {
    zxlogf(ERROR, "Failed to write bRefClkFreq attribute: %s", query_response.status_string());
    return query_response.take_error();
  }
  return zx::ok();
}

zx::result<> DeviceManager::SetUicPowerMode() {
  // TODO(https://fxbug.dev/124835): Get the max gear level using DME_GET command.
  // TODO(https://fxbug.dev/124835): Set the gear level for tx/rx lanes.

  // Get connected lanes.
  DmeGetUicCommand dme_get_connected_tx_lanes_command(controller_, PA_ConnectedTxDataLanes, 0);
  zx::result<std::optional<uint32_t>> value = dme_get_connected_tx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  uint32_t connected_tx_lanes = value.value().value();

  DmeGetUicCommand dme_get_connected_rx_lanes_command(controller_, PA_ConnectedRxDataLanes, 0);
  value = dme_get_connected_rx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  uint32_t connected_rx_lanes = value.value().value();

  // Update lanes with available TX/RX lanes.
  DmeGetUicCommand dme_get_avail_tx_lanes_command(controller_, PA_AvailTxDataLanes, 0);
  value = dme_get_avail_tx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  uint32_t tx_lanes = value.value().value();

  DmeGetUicCommand dme_get_avail_rx_lanes_command(controller_, PA_AvailRxDataLanes, 0);
  value = dme_get_avail_rx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  uint32_t rx_lanes = value.value().value();

  zxlogf(DEBUG, "connected_tx_lanes=%d, connected_rx_lanes=%d", connected_tx_lanes,
         connected_rx_lanes);
  zxlogf(DEBUG, "tx_lanes_=%d, rx_lanes_=%d", tx_lanes, rx_lanes);

  return zx::ok();
}

}  // namespace ufs
