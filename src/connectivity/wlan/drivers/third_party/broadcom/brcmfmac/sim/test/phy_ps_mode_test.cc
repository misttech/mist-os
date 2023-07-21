// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/wlanphyimpl/c/banjo.h>
#include <zircon/errors.h>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-device/device.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-env.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/common.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/defs.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/fwil.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"

namespace wlan::brcmfmac {

class PhyPsModeTest : public SimTest {
 public:
  PhyPsModeTest() = default;
  void Init();
  void CreateInterface();
  void DeleteInterface();
  zx_status_t SetPowerSaveMode(const wlan_common::PowerSaveType* ps_mode);
  void GetPowerSaveModeFromFirmware(uint32_t* ps_mode);
  zx_status_t SetPowerSaveModeInFirmware(const wlan_phy_ps_mode_t* ps_mode);
  zx_status_t ClearCountryCode();
  uint32_t DeviceCountByProtocolId(uint32_t proto_id);

 private:
  SimInterface client_ifc_;
};

void PhyPsModeTest::Init() { ASSERT_EQ(SimTest::Init(), ZX_OK); }

void PhyPsModeTest::CreateInterface() {
  zx_status_t status;

  status = StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc_);
  ASSERT_EQ(status, ZX_OK);
}

void PhyPsModeTest::DeleteInterface() { EXPECT_EQ(SimTest::DeleteInterface(&client_ifc_), ZX_OK); }

uint32_t PhyPsModeTest::DeviceCountByProtocolId(uint32_t proto_id) {
  return (dev_mgr_->DeviceCountByProtocolId(proto_id));
}

zx_status_t PhyPsModeTest::SetPowerSaveMode(const wlan_common::PowerSaveType* ps_mode) {
  fidl::Arena fidl_arena;
  auto builder =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplSetPowerSaveModeRequest::Builder(fidl_arena);
  builder.ps_mode(*ps_mode);
  auto result = client_.sync().buffer(test_arena_)->SetPowerSaveMode(builder.Build());
  EXPECT_TRUE(result.ok());
  if (result->is_error()) {
    return result->error_value();
  }
  return ZX_OK;
}

// Note that this function is meant for SIM only. It retrieves the internal
// state of PS Mode setting by bypassing the interfaces.
void PhyPsModeTest::GetPowerSaveModeFromFirmware(uint32_t* ps_mode) {
  EXPECT_NE(ps_mode, nullptr);
  brcmf_simdev* sim = device_->GetSim();
  struct brcmf_if* ifp = brcmf_get_ifp(sim->drvr, client_ifc_.iface_id_);
  zx_status_t status = brcmf_fil_cmd_int_get(ifp, BRCMF_C_GET_PM, ps_mode, nullptr);
  EXPECT_EQ(status, ZX_OK);
}

zx_status_t PhyPsModeTest::SetPowerSaveModeInFirmware(const wlan_phy_ps_mode_t* ps_mode) {
  EXPECT_NE(ps_mode, nullptr);
  brcmf_simdev* sim = device_->GetSim();
  return brcmf_set_power_save_mode(sim->drvr, ps_mode);
}

TEST_F(PhyPsModeTest, SetPowerSaveModeIncorrect) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLANPHY_IMPL), 1u);
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);

  // Get the country code and verify that it is set to WW.
  uint32_t fw_ps_mode;
  GetPowerSaveModeFromFirmware(&fw_ps_mode);
  ASSERT_EQ(fw_ps_mode, (uint32_t)PM_OFF);

  // Set PS mode but without passing any PS mode to set and verify
  // that it FAILS
  fidl::Arena fidl_arena;
  auto builder =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplSetPowerSaveModeRequest::Builder(fidl_arena);
  auto result = client_.sync().buffer(test_arena_)->SetPowerSaveMode(builder.Build());
  EXPECT_TRUE(result.ok());
  zx_status_t status = result->is_error() ? result->error_value() : ZX_OK;

  ASSERT_EQ(status, ZX_ERR_INVALID_ARGS);

  DeleteInterface();
}

// Test setting PS Mode to invalid and valid values.
TEST_F(PhyPsModeTest, SetPowerSaveMode) {
  const auto valid_ps_mode = wlan_common::PowerSaveType::kPsModeBalanced;
  zx_status_t status;
  uint32_t fw_ps_mode;

  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLANPHY_IMPL), 1u);
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);

  // Get the country code and verify that it is set to WW.
  GetPowerSaveModeFromFirmware(&fw_ps_mode);
  ASSERT_EQ(fw_ps_mode, (uint32_t)PM_OFF);

  // Set a valid PS mode and verify it succeeds
  status = SetPowerSaveMode(&valid_ps_mode);
  ASSERT_EQ(status, ZX_OK);
  GetPowerSaveModeFromFirmware(&fw_ps_mode);
  ASSERT_EQ(fw_ps_mode, (uint32_t)PM_FAST);
  DeleteInterface();
}
// Ensure PS Mode set in FW is either OFF or FAST.
TEST_F(PhyPsModeTest, CheckFWPsMode) {
  auto valid_ps_mode = wlan_common::PowerSaveType::kPsModeBalanced;
  zx_status_t status;
  uint32_t fw_ps_mode;

  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLANPHY_IMPL), 1u);
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);

  // Get the country code and verify that it is set to WW.
  GetPowerSaveModeFromFirmware(&fw_ps_mode);
  ASSERT_EQ(fw_ps_mode, (uint32_t)PM_OFF);

  // Set PS mode to PS_MODE_BALANCED
  status = SetPowerSaveMode(&valid_ps_mode);
  ASSERT_EQ(status, ZX_OK);
  // Verify that it gets set to FAST in FW
  GetPowerSaveModeFromFirmware(&fw_ps_mode);
  ASSERT_EQ(fw_ps_mode, (uint32_t)PM_FAST);

  // Set PS mode to PS_MODE_ULTRA_LOW_POWER
  valid_ps_mode = wlan_common::PowerSaveType::kPsModeUltraLowPower;
  status = SetPowerSaveMode(&valid_ps_mode);
  ASSERT_EQ(status, ZX_OK);
  // Verify that it gets set to FAST in FW
  GetPowerSaveModeFromFirmware(&fw_ps_mode);
  ASSERT_EQ(fw_ps_mode, (uint32_t)PM_FAST);
  // Set PS mode to PS_MODE_LOW_POWER
  valid_ps_mode = wlan_common::PowerSaveType::kPsModeLowPower;
  status = SetPowerSaveMode(&valid_ps_mode);
  ASSERT_EQ(status, ZX_OK);
  // Verify that it gets set to FAST in FW
  GetPowerSaveModeFromFirmware(&fw_ps_mode);
  ASSERT_EQ(fw_ps_mode, (uint32_t)PM_FAST);

  // Set PS mode to PS_MODE_PERFORMANCE
  valid_ps_mode = wlan_common::PowerSaveType::kPsModePerformance;
  status = SetPowerSaveMode(&valid_ps_mode);
  ASSERT_EQ(status, ZX_OK);
  // Verify that it gets set to OFF in FW
  GetPowerSaveModeFromFirmware(&fw_ps_mode);
  ASSERT_EQ(fw_ps_mode, (uint32_t)PM_OFF);
  DeleteInterface();
}

// Test Getting PS Mode
TEST_F(PhyPsModeTest, GetPowerSaveMode) {
  Init();
  CreateInterface();

  {
    const auto valid_ps_mode = wlan_common::PowerSaveType::kPsModeBalanced;
    const wlan_phy_ps_mode_t valid_ps_mode_banjo = {.ps_mode = POWER_SAVE_TYPE_PS_MODE_BALANCED};
    ASSERT_EQ(ZX_OK, SetPowerSaveModeInFirmware(&valid_ps_mode_banjo));
    auto result = client_.sync().buffer(test_arena_)->GetPowerSaveMode();
    EXPECT_TRUE(result.ok());
    ASSERT_FALSE(result->is_error());
    EXPECT_EQ(result->value()->ps_mode(), valid_ps_mode);
  }

  // Try again, just in case the first one was a default value.
  {
    const auto valid_ps_mode = wlan_common::PowerSaveType::kPsModePerformance;
    const wlan_phy_ps_mode_t valid_ps_mode_banjo = {.ps_mode = POWER_SAVE_TYPE_PS_MODE_PERFORMANCE};
    ASSERT_EQ(ZX_OK, SetPowerSaveModeInFirmware(&valid_ps_mode_banjo));
    auto result = client_.sync().buffer(test_arena_)->GetPowerSaveMode();
    EXPECT_TRUE(result.ok());
    ASSERT_FALSE(result->is_error());
    EXPECT_EQ(result->value()->ps_mode(), valid_ps_mode);
  }
  DeleteInterface();
}
}  // namespace wlan::brcmfmac
