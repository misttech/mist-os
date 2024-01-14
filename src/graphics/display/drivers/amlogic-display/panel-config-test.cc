// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/panel-config.h"

#include <lib/device-protocol/display-panel.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <gtest/gtest.h>

namespace amlogic_display {

namespace {

TEST(PanelConfig, Tv070wsmFt) {
  const PanelConfig* config_tv070wsm_ft = GetPanelConfig(PANEL_TV070WSM_FT);
  ASSERT_NE(config_tv070wsm_ft, nullptr);
  EXPECT_STREQ(config_tv070wsm_ft->name, "TV070WSM_FT");
}

TEST(PanelConfig, P070acbFt) {
  const PanelConfig* config_p070acb_ft = GetPanelConfig(PANEL_P070ACB_FT);
  ASSERT_NE(config_p070acb_ft, nullptr);
  EXPECT_STREQ(config_p070acb_ft->name, "P070ACB_FT");
}

TEST(PanelConfig, Tv101wxmFt) {
  const PanelConfig* config_tv101wxm_ft = GetPanelConfig(PANEL_TV101WXM_FT);
  ASSERT_NE(config_tv101wxm_ft, nullptr);
  EXPECT_STREQ(config_tv101wxm_ft->name, "TV101WXM_FT");
}

TEST(PanelConfig, P101dezFt) {
  const PanelConfig* config_p101dez_ft = GetPanelConfig(PANEL_P101DEZ_FT);
  ASSERT_NE(config_p101dez_ft, nullptr);
  EXPECT_STREQ(config_p101dez_ft->name, "P101DEZ_FT");
}

TEST(PanelConfig, Tv101wxmFt9365) {
  const PanelConfig* config_tv101wxm_ft_9365 = GetPanelConfig(PANEL_TV101WXM_FT_9365);
  ASSERT_NE(config_tv101wxm_ft_9365, nullptr);
  EXPECT_STREQ(config_tv101wxm_ft_9365->name, "TV101WXM_FT_9365");
}

TEST(PanelConfig, Tv070wsmFt9365) {
  const PanelConfig* config_tv070wsm_ft_9365 = GetPanelConfig(PANEL_TV070WSM_FT_9365);
  ASSERT_NE(config_tv070wsm_ft_9365, nullptr);
  EXPECT_STREQ(config_tv070wsm_ft_9365->name, "TV070WSM_FT_9365");
}

TEST(PanelConfig, Kd070d82Ft) {
  const PanelConfig* config_kd070d82_ft = GetPanelConfig(PANEL_KD070D82_FT);
  ASSERT_NE(config_kd070d82_ft, nullptr);
  EXPECT_STREQ(config_kd070d82_ft->name, "KD070D82_FT");
}

TEST(PanelConfig, Kd070d82Ft9365) {
  const PanelConfig* config_kd070d82_ft_9365 = GetPanelConfig(PANEL_KD070D82_FT_9365);
  ASSERT_NE(config_kd070d82_ft_9365, nullptr);
  EXPECT_STREQ(config_kd070d82_ft_9365->name, "KD070D82_FT_9365");
}

TEST(PanelConfig, Tv070wsmSt7703i) {
  const PanelConfig* config_tv070wsm_st7703i = GetPanelConfig(PANEL_TV070WSM_ST7703I);
  ASSERT_NE(config_tv070wsm_st7703i, nullptr);
  EXPECT_STREQ(config_tv070wsm_st7703i->name, "TV070WSM_ST7703I");
}

TEST(PanelConfig, Mtf050fhdi03) {
  const PanelConfig* config_mtf050fhdi_03 = GetPanelConfig(PANEL_MTF050FHDI_03);
  ASSERT_NE(config_mtf050fhdi_03, nullptr);
  EXPECT_STREQ(config_mtf050fhdi_03->name, "MTF050FHDI_03");
}

TEST(PanelConfig, InvalidPanels) {
  const PanelConfig* config_0x04 = GetPanelConfig(0x04);
  EXPECT_EQ(config_0x04, nullptr);

  const PanelConfig* config_0x05 = GetPanelConfig(0x05);
  EXPECT_EQ(config_0x05, nullptr);

  const PanelConfig* config_0x06 = GetPanelConfig(0x06);
  EXPECT_EQ(config_0x06, nullptr);

  const PanelConfig* config_overly_large = GetPanelConfig(0x0d);
  EXPECT_EQ(config_overly_large, nullptr);

  const PanelConfig* config_unknown = GetPanelConfig(PANEL_UNKNOWN);
  EXPECT_EQ(config_unknown, nullptr);
}

}  // namespace

}  // namespace amlogic_display
