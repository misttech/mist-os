/*
 * Copyright (c) 2019 The Fuchsia Authors
 *
 * Permission to use, copy, modify, and/or distribute this software for any
 * purpose with or without fee is hereby granted, provided that the above
 * copyright notice and this permission notice appear in all copies.
 *
 * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
 * WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY
 * SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
 * WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION
 * OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
 * CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
 */

#include <fidl/fuchsia.wlan.common/cpp/fidl.h>

#include <gtest/gtest.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/brcmu_d11.h"

namespace {

static void verify_channel_to_chanspec(const fuchsia_wlan_ieee80211::WlanChannel& in_ch,
                                       const brcmu_chan& expected) {
  brcmu_d11inf d11_inf = {.io_type = BRCMU_D11AC_IOTYPE};
  brcmu_d11_attach(&d11_inf);

  uint16_t chanspec = channel_to_chanspec(&d11_inf, &in_ch);
  brcmu_chan actual = {.chspec = chanspec};
  d11_inf.decchspec(&actual);

  EXPECT_EQ(actual.chnum, expected.chnum);
  EXPECT_EQ(actual.band, expected.band);
  EXPECT_EQ(actual.bw, expected.bw);
  EXPECT_EQ(actual.sb, expected.sb);
}

TEST(ChannelConversion, ChannelToChanspec) {
  brcmu_chan out_ch;

  {
    // Try a simple 20 MHz channel in the 2.4 GHz band
    fuchsia_wlan_ieee80211::WlanChannel in_ch(11, fuchsia_wlan_ieee80211::ChannelBandwidth::kCbw20,
                                              0);
    out_ch = {
        .chnum = 11, .band = BRCMU_CHAN_BAND_2G, .bw = BRCMU_CHAN_BW_20, .sb = BRCMU_CHAN_SB_NONE};
    verify_channel_to_chanspec(in_ch, out_ch);
  }

  {
    // Try a 40+ MHz channel in the 5 GHz band
    fuchsia_wlan_ieee80211::WlanChannel in_ch(44, fuchsia_wlan_ieee80211::ChannelBandwidth::kCbw40,
                                              0);
    out_ch = {
        .chnum = 44, .band = BRCMU_CHAN_BAND_5G, .bw = BRCMU_CHAN_BW_40, .sb = BRCMU_CHAN_SB_U};
    verify_channel_to_chanspec(in_ch, out_ch);
  }

  {
    // Try a 40- MHz channel in the 5 GHz band with invalid secondary80 (which should be ignored)
    fuchsia_wlan_ieee80211::WlanChannel in_ch(
        112, fuchsia_wlan_ieee80211::ChannelBandwidth::kCbw40Below, 44);
    out_ch = {
        .chnum = 112, .band = BRCMU_CHAN_BAND_5G, .bw = BRCMU_CHAN_BW_40, .sb = BRCMU_CHAN_SB_L};
    verify_channel_to_chanspec(in_ch, out_ch);
  }
}

static void verify_chanspec_to_channel(const brcmu_chan& in_ch,
                                       const fuchsia_wlan_ieee80211::WlanChannel& expected) {
  brcmu_d11inf d11_inf = {.io_type = BRCMU_D11AC_IOTYPE};
  brcmu_d11_attach(&d11_inf);

  brcmu_chan in_ch_temp = in_ch;
  d11_inf.encchspec(&in_ch_temp);
  fuchsia_wlan_ieee80211::wire::WlanChannel actual;
  chanspec_to_channel(&d11_inf, in_ch_temp.chspec, &actual);

  EXPECT_EQ(actual.primary, expected.primary());
  EXPECT_EQ(actual.cbw, expected.cbw());
  EXPECT_EQ(actual.secondary80, expected.secondary80());
}

TEST(ChannelConversion, ChanspecToChannel) {
  brcmu_chan in_ch;

  {
    // Try a simple 20 MHz channel in the 2.4 GHz band
    in_ch = {
        .chnum = 11, .band = BRCMU_CHAN_BAND_2G, .bw = BRCMU_CHAN_BW_20, .sb = BRCMU_CHAN_SB_NONE};
    fuchsia_wlan_ieee80211::WlanChannel out_ch(11, fuchsia_wlan_ieee80211::ChannelBandwidth::kCbw20,
                                               0);
    verify_chanspec_to_channel(in_ch, out_ch);
  }

  {
    // Try a 40+ MHz channel in the 5 GHz band
    in_ch = {
        .chnum = 44, .band = BRCMU_CHAN_BAND_5G, .bw = BRCMU_CHAN_BW_40, .sb = BRCMU_CHAN_SB_U};
    fuchsia_wlan_ieee80211::WlanChannel out_ch(44, fuchsia_wlan_ieee80211::ChannelBandwidth::kCbw40,
                                               0);
    verify_chanspec_to_channel(in_ch, out_ch);
  }

  {
    // Try a 40- MHz channel in the 5 GHz band
    in_ch = {
        .chnum = 112, .band = BRCMU_CHAN_BAND_5G, .bw = BRCMU_CHAN_BW_40, .sb = BRCMU_CHAN_SB_L};
    fuchsia_wlan_ieee80211::WlanChannel out_ch(
        112, fuchsia_wlan_ieee80211::ChannelBandwidth::kCbw40Below, 0);
    verify_chanspec_to_channel(in_ch, out_ch);
  }
}

}  // namespace
