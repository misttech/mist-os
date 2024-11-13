// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include <fuchsia/wlan/ieee80211/c/banjo.h>
#include <zircon/errors.h>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"
#include "src/connectivity/wlan/lib/common/cpp/include/wlan/common/macaddr.h"

namespace wlan::brcmfmac {
namespace {

constexpr zx::duration kSimulatedClockDuration = zx::sec(10);

}  // namespace

const common::MacAddr kDefaultMac({0x12, 0x34, 0x56, 0x65, 0x43, 0x21});

// Rates are defined in 500 kbps
constexpr uint8_t kExpectedBasic2gRates[] = {2, 4, 11, 22, 12, 18, 24, 36, 48, 72, 96, 108};
constexpr uint8_t kExpectedBasic5gRates[] = {12, 18, 24, 36, 48, 72, 96, 108};

// Verify that a query operation works on a client interface
TEST_F(SimTest, ClientIfcQuery) {
  ASSERT_EQ(Init(), ZX_OK);

  SimInterface client_ifc;
  ASSERT_EQ(StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc, kDefaultMac), ZX_OK);

  wlan_fullmac_wire::WlanFullmacImplQueryResponse ifc_query_result;
  // TODO(https://fxbug.dev/42176028): This query silently logs errors and fails because the
  // the "chanspecs", "ldpc_cap", and other iovars are not supported by the simulated firmware.
  env_->ScheduleNotification(std::bind(&SimInterface::Query, &client_ifc, &ifc_query_result),
                             zx::sec(1));
  env_->Run(kSimulatedClockDuration);

  // Mac address returned should match the one we specified when we created the interface
  ASSERT_EQ(wlan_ieee80211::kMacAddrLen, common::kMacAddrLen);
  EXPECT_EQ(
      0, memcmp(kDefaultMac.byte, ifc_query_result.sta_addr().data(), wlan_ieee80211::kMacAddrLen));

  ASSERT_TRUE(ifc_query_result.has_role());
  ASSERT_TRUE(ifc_query_result.has_sta_addr());
  ASSERT_TRUE(ifc_query_result.has_band_caps());

  EXPECT_EQ(ifc_query_result.role(), wlan_common::WlanMacRole::kClient);

  // Number of bands shouldn't exceed the maximum allowable
  ASSERT_LE(ifc_query_result.band_caps().count(), (size_t)wlan_common::kMaxBands);
  ASSERT_GT(ifc_query_result.band_caps().count(), 0);

  for (size_t band = 0; band < ifc_query_result.band_caps().count(); band++) {
    wlan_fullmac_wire::WlanFullmacBandCapability* band_cap = &ifc_query_result.band_caps()[band];

    // Band id should be in valid range
    ASSERT_TRUE(band_cap->band == wlan_common::WlanBand::kTwoGhz ||
                band_cap->band == wlan_common::WlanBand::kFiveGhz);

    if (band_cap->band == wlan_common::WlanBand::kTwoGhz) {
      ASSERT_EQ(band_cap->basic_rates.count(), std::size(kExpectedBasic2gRates));
      ASSERT_EQ(0, std::memcmp(band_cap->basic_rates.data(), kExpectedBasic2gRates,
                               band_cap->basic_rates.count()));
    } else {
      ASSERT_EQ(band_cap->basic_rates.count(), std::size(kExpectedBasic5gRates));
      ASSERT_EQ(0, std::memcmp(band_cap->basic_rates.data(), kExpectedBasic5gRates,
                               band_cap->basic_rates.count()));
    }
  }
}

// Verify that we can retrieve interface attributes even if the nchain iovar value is too large
TEST_F(SimTest, BadNchainIovar) {
  ASSERT_EQ(Init(), ZX_OK);

  SimInterface client_ifc;
  ASSERT_EQ(StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc), ZX_OK);

  // This invalid value of rxchain data has the potential to overflow the driver's internal
  // data structures
  const std::vector<uint8_t> alt_rxchain_data = {0xff, 0xff, 0xff, 0xff};
  WithSimDevice([&](brcmfmac::SimDevice* device) {
    brcmf_simdev* sim = device->GetSim();
    sim->sim_fw->err_inj_.AddErrInjIovar("rxstreams_cap", ZX_OK, BCME_OK, client_ifc.iface_id_,
                                         &alt_rxchain_data);
  });

  wlan_fullmac_wire::WlanFullmacImplQueryResponse ifc_query_result;
  env_->ScheduleNotification(std::bind(&SimInterface::Query, &client_ifc, &ifc_query_result),
                             zx::sec(1));
  env_->Run(kSimulatedClockDuration);

  // This test just verifies that we don't crash when the iovar is retrieved
}

}  // namespace wlan::brcmfmac
