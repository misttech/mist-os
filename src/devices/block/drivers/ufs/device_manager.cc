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

zx::result<uint32_t> DeviceManager::ReadAttribute(Attributes attribute) {
  ReadAttributeUpiu read_attribute_upiu(attribute);
  auto query_response =
      controller_.GetTransferRequestProcessor()
          .SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(read_attribute_upiu);
  if (query_response.is_error()) {
    zxlogf(ERROR, "Failed to read attribute(%x): %s", static_cast<uint8_t>(attribute),
           query_response.status_string());
    return query_response.take_error();
  }
  return zx::ok(query_response->GetResponse<AttributeResponseUpiu>().GetAttribute());
}

zx::result<> DeviceManager::WriteAttribute(Attributes attribute, uint32_t value) {
  WriteAttributeUpiu write_attribute_upiu(attribute, value);
  auto query_response =
      controller_.GetTransferRequestProcessor()
          .SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(write_attribute_upiu);
  if (query_response.is_error()) {
    zxlogf(ERROR, "Failed to write attribute(%x): %s", static_cast<uint8_t>(attribute),
           query_response.status_string());
    return query_response.take_error();
  }
  return zx::ok();
}

zx::result<> DeviceManager::CheckBootLunEnabled() {
  // Read bBootLunEn to confirm device interface is ok.
  zx::result<uint32_t> boot_lun_enabled = ReadAttribute(Attributes::bBootLunEn);
  if (boot_lun_enabled.is_error()) {
    return boot_lun_enabled.take_error();
  }
  zxlogf(DEBUG, "bBootLunEn 0x%0x", boot_lun_enabled.value());
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

zx::result<> DeviceManager::InitReferenceClock() {
  // 26MHz is a default value written in spec.
  // UFS Specification Version 3.1, section 6.4 "Reference Clock".
  return WriteAttribute(Attributes::bRefClkFreq, AttributeReferenceClock::k26MHz);
}

zx::result<> DeviceManager::InitUicPowerMode() {
  // TODO(https://fxbug.dev/42075643): Get the max gear level using DME_GET command.
  // TODO(https://fxbug.dev/42075643): Set the gear level for tx/rx lanes.

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

zx::result<> DeviceManager::SetPowerCondition(scsi::PowerCondition target_power_condition) {
  if (current_power_condition_ == target_power_condition) {
    return zx::ok();
  }

  auto scsi_lun = Ufs::TranslateUfsLunToScsiLun(static_cast<uint8_t>(WellKnownLuns::kUfsDevice));
  if (scsi_lun.is_error()) {
    return scsi_lun.take_error();
  }

  // Send START STOP UNIT to change power mode
  zx_status_t status = controller_.StartStopUnit(kPlaceholderTarget, scsi_lun.value(),
                                                 /*immed*/ false, target_power_condition);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to send START STOP UNIT SCSI command: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  current_power_condition_ = target_power_condition;
  return zx::ok();
}

zx::result<> DeviceManager::Suspend() {
  const UfsPowerMode target_power_mode = UfsPowerMode::kSleep;
  const scsi::PowerCondition target_power_condition = power_mode_map_[target_power_mode].first;
  const LinkState target_link_state = power_mode_map_[target_power_mode].second;

  if (current_power_mode_ == target_power_mode) {
    return zx::ok();
  }

  if (current_power_mode_ != UfsPowerMode::kActive || current_link_state_ != LinkState::kActive) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  // TODO(https://fxbug.dev/42075643): We need to wait for the in flight I/O.
  // TODO(https://fxbug.dev/42075643): Disable background operations.
  // TODO(https://fxbug.dev/42075643): Device's write back buffer must be flushed.

  if (zx::result<> result = controller_.Notify(NotifyEvent::kPrePowerModeChange, 0);
      result.is_error()) {
    return result.take_error();
  }

  if (zx::result<> result = SetPowerCondition(target_power_condition); result.is_error()) {
    return result.take_error();
  }

  DmeHibernateEnterCommand dme_hibernate_enter_command(controller_);
  if (auto result = dme_hibernate_enter_command.SendCommand(); result.is_error()) {
    // TODO(https://fxbug.dev/42075643): Link has a problem and needs to perform error recovery.
    return result.take_error();
  }
  current_link_state_ = target_link_state;

  if (zx::result<> result = controller_.Notify(NotifyEvent::kPostPowerModeChange, 0);
      result.is_error()) {
    return result.take_error();
  }

  current_power_mode_ = target_power_mode;
  return zx::ok();
}

zx::result<> DeviceManager::Resume() {
  const UfsPowerMode target_power_mode = UfsPowerMode::kActive;
  const scsi::PowerCondition target_power_condition = power_mode_map_[target_power_mode].first;
  const LinkState target_link_state = power_mode_map_[target_power_mode].second;

  if (current_power_mode_ == target_power_mode) {
    return zx::ok();
  }

  if (current_power_mode_ != UfsPowerMode::kSleep || current_link_state_ != LinkState::kHibernate) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  if (zx::result<> result = controller_.Notify(NotifyEvent::kPrePowerModeChange, 0);
      result.is_error()) {
    return result.take_error();
  }

  DmeHibernateExitCommand dme_hibernate_exit_command(controller_);
  if (auto result = dme_hibernate_exit_command.SendCommand(); result.is_error()) {
    // TODO(https://fxbug.dev/42075643): Link has a problem and needs to perform error recovery.
    return result.take_error();
  }
  current_link_state_ = target_link_state;

  if (zx::result<> result = SetPowerCondition(target_power_condition); result.is_error()) {
    return result.take_error();
  }

  if (zx::result<> result = controller_.Notify(NotifyEvent::kPostPowerModeChange, 0);
      result.is_error()) {
    return result.take_error();
  }

  current_power_mode_ = target_power_mode;
  return zx::ok();
}

zx::result<> DeviceManager::InitUfsPowerMode() {
  // Read current power mode (bCurrentPowerMode, bActiveIccLevel)
  zx::result<uint32_t> power_mode = ReadAttribute(Attributes::bCurrentPowerMode);
  if (power_mode.is_error()) {
    return power_mode.take_error();
  }
  current_power_mode_ = static_cast<UfsPowerMode>(power_mode.value());
  if (current_power_mode_ != UfsPowerMode::kActive) {
    zxlogf(ERROR, "Initial power mode is not active: 0x%x",
           static_cast<uint8_t>(current_power_mode_));
    return zx::error(ZX_ERR_BAD_STATE);
  }
  zxlogf(DEBUG, "bCurrentPowerMode 0x%0x", power_mode.value());

  current_power_condition_ = power_mode_map_[current_power_mode_].first;
  current_link_state_ = power_mode_map_[current_power_mode_].second;

  // TODO(https://fxbug.dev/42075643): Calculate and set the maximum ICC level. Currently, this
  // value is temporarily set to 0x0F, which is the highest active ICC level.
  if (auto result = WriteAttribute(Attributes::bActiveIccLevel, kHighestActiveIcclevel);
      result.is_error()) {
    return result.take_error();
  }

  // TODO(https://fxbug.dev/42075643): Enable auto hibernate

  return zx::ok();
}

}  // namespace ufs
