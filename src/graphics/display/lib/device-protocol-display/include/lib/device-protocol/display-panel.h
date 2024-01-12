// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_GRAPHICS_DISPLAY_LIB_DEVICE_PROTOCOL_DISPLAY_INCLUDE_LIB_DEVICE_PROTOCOL_DISPLAY_PANEL_H_
#define SRC_GRAPHICS_DISPLAY_LIB_DEVICE_PROTOCOL_DISPLAY_INCLUDE_LIB_DEVICE_PROTOCOL_DISPLAY_PANEL_H_

#include <fuchsia/hardware/dsiimpl/c/banjo.h>
#include <zircon/types.h>

// Supported panel types

// Panel: BOE TV070WSM-TG1
// DDIC: Fitipower JD9364
// Board: Astro, Nelson
// Touch IC: FocalTech FT3x27 (Astro), Goodix GT6853 (Nelson)
#define PANEL_TV070WSM_FT UINT8_C(0x00)

// Panel: Innolux P070ACB-DB0
// DDIC: Fitipower JD9364
// Board: Astro
// Touch IC: Goodix GT9293
#define PANEL_P070ACB_FT UINT8_C(0x01)

// Panel: BOE TV101WXM-LG0
// DDIC: Fitipower JD9364
// Board: Sherlock
// Touch IC: FocalTech FT5726
#define PANEL_TV101WXM_FT UINT8_C(0x02)

// Panel: Innolux G101B158
// DDIC: Fitipower JD9364
// Board: Sherlock
// Touch IC: FocalTech FT5726
#define PANEL_G101B158_FT UINT8_C(0x03)

// 0x04 was for PANEL_ILI9881C
// 0x05 was for PANEL_ST7701S
// 0x06 was for PANEL_TV080WXM_FT

// Panel: BOE TV101WXM-LG0
// DDIC: Fitipower JD9365
// Board: Sherlock
// Touch IC: FocalTech FT5726
#define PANEL_TV101WXM_FT_9365 UINT8_C(0x07)

// Panel: BOE TV070WSM-TG1
// DDIC: Fitipower JD9365
// Board: Astro, Nelson
// Touch IC: FocalTech FT3x27 (Astro), Goodix GT6853 (Nelson)
#define PANEL_TV070WSM_FT_9365 UINT8_C(0x08)

// Panel: K&D KD070D82-39TI-A010
// DDIC: Fitipower JD9364
// Board: Nelson
// Touch IC: Goodix GT6853
#define PANEL_KD070D82_FT UINT8_C(0x09)

// Panel: K&D KD070D82-39TI-A010
// DDIC: Fitipower JD9365
// Board: Nelson
// Touch IC: Goodix GT6853
#define PANEL_KD070D82_FT_9365 UINT8_C(0x0a)

// Panel: BOE TV070WSM-TG1
// DDIC: Sitronix ST7703I
// Board: Nelson
// Touch IC: Goodix GT6853
#define PANEL_TV070WSM_ST7703I UINT8_C(0x0b)

// Khadas TS-050
// Panel: Microtech MTF050FHDI-03
// DDIC: Novatek NT35596
// Board: VIM3
// Touch IC: FocalTech FT5336
#define PANEL_MTF050FHDI_03 UINT8_C(0x0c)

#define PANEL_UNKNOWN UINT8_C(0xFF)

// Astro/Sherlock Display Configuration. These configuration comes directly from
// from LCD vendor and hardware team.
const display_setting_t kDisplaySettingTV070WSM_FT = {
    .lane_num = 4,
    .bit_rate_max = 360,
    .lcd_clock = 44250000,
    .h_active = 600,
    .v_active = 1024,
    .h_period = 700,
    .v_period = 1053,
    .hsync_width = 24,
    .hsync_bp = 36,
    .hsync_pol = 0,
    .vsync_width = 2,
    .vsync_bp = 8,
    .vsync_pol = 0,
};
const display_setting_t kDisplaySettingP070ACB_FT = {
    .lane_num = 4,
    .bit_rate_max = 400,
    .lcd_clock = 49434000,
    .h_active = 600,
    .v_active = 1024,
    .h_period = 770,
    .v_period = 1070,
    .hsync_width = 10,
    .hsync_bp = 80,
    .hsync_pol = 0,
    .vsync_width = 6,
    .vsync_bp = 20,
    .vsync_pol = 0,
};
const display_setting_t kDisplaySettingG101B158_FT = {
    .lane_num = 4,
    .bit_rate_max = 566,
    .lcd_clock = 70701600,
    .h_active = 800,
    .v_active = 1280,
    .h_period = 890,
    .v_period = 1324,
    .hsync_width = 24,
    .hsync_bp = 20,
    .hsync_pol = 0,
    .vsync_width = 4,
    .vsync_bp = 20,
    .vsync_pol = 0,
};
const display_setting_t kDisplaySettingTV101WXM_FT = {
    .lane_num = 4,
    .bit_rate_max = 566,
    .lcd_clock = 70701600,
    .h_active = 800,
    .v_active = 1280,
    .h_period = 890,
    .v_period = 1324,
    .hsync_width = 20,
    .hsync_bp = 50,
    .hsync_pol = 0,
    .vsync_width = 4,
    .vsync_bp = 20,
    .vsync_pol = 0,
};
const display_setting_t kDisplaySettingKD070D82_FT = {
    .lane_num = 4,
    .bit_rate_max = 400,
    .lcd_clock = 49434000,
    .h_active = 600,
    .v_active = 1024,
    .h_period = 770,
    .v_period = 1070,
    .hsync_width = 10,
    .hsync_bp = 80,
    .hsync_pol = 0,
    .vsync_width = 6,
    .vsync_bp = 20,
    .vsync_pol = 0,
};
const display_setting_t kDisplaySettingTV070WSM_ST7703I = {
    .lane_num = 4,
    .bit_rate_max = 400,
    .lcd_clock = 44226000,
    .h_active = 600,
    .v_active = 1024,
    .h_period = 700,
    .v_period = 1053,
    .hsync_width = 24,
    .hsync_bp = 36,
    .hsync_pol = 0,
    .vsync_width = 2,
    .vsync_bp = 8,
    .vsync_pol = 0,
};

// Display timing information of MTF050FHDI-03 LCD used for Khadas TS050
// touchscreen, dumped from VIM3 bootloader output.
const display_setting_t kDisplaySettingMTF050FHDI_03 = {
    .lane_num = 4,
    .bit_rate_max = 1000,
    .lcd_clock = 120'000'000,
    .h_active = 1080,
    .v_active = 1920,
    .h_period = 1120,
    .v_period = 1933,
    .hsync_width = 2,
    .hsync_bp = 23,
    .hsync_pol = 0,
    .vsync_width = 4,
    .vsync_bp = 7,
    .vsync_pol = 0,
};

typedef struct {
  uint32_t width;
  uint32_t height;
  uint32_t panel_type;
} display_panel_t;

#endif  // SRC_GRAPHICS_DISPLAY_LIB_DEVICE_PROTOCOL_DISPLAY_INCLUDE_LIB_DEVICE_PROTOCOL_DISPLAY_PANEL_H_
