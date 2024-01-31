// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_GRAPHICS_DISPLAY_LIB_DEVICE_PROTOCOL_DISPLAY_INCLUDE_LIB_DEVICE_PROTOCOL_DISPLAY_PANEL_H_
#define SRC_GRAPHICS_DISPLAY_LIB_DEVICE_PROTOCOL_DISPLAY_INCLUDE_LIB_DEVICE_PROTOCOL_DISPLAY_PANEL_H_

#include <zircon/types.h>

// Supported panel types

// Panel: BOE TV070WSM-TG1
// DDIC: Fitipower JD9364
// Board: Astro
// Touch IC: FocalTech FT3x27 (Astro)
#define PANEL_TV070WSM_FT_ASTRO UINT8_C(0x00)

// Panel: Innolux P070ACB-DB0
// DDIC: Fitipower JD9364
// Board: Astro
// Touch IC: Goodix GT9293
#define PANEL_P070ACB_FT UINT8_C(0x01)

// Panel: BOE TV101WXM-AG0
// DDIC: Fitipower JD9364
// Board: Sherlock
// Touch IC: FocalTech FT5726
#define PANEL_TV101WXM_FT UINT8_C(0x02)

// Panel: Innolux P101DEZ-DD0
// DDIC: Fitipower JD9364
// Board: Sherlock
// Touch IC: FocalTech FT5726
#define PANEL_P101DEZ_FT UINT8_C(0x03)

// 0x04 was for PANEL_ILI9881C
// 0x05 was for PANEL_ST7701S
// 0x06 was for PANEL_TV080WXM_FT

// Panel: BOE TV101WXM-AG0
// DDIC: Fitipower JD9365
// Board: Sherlock
// Touch IC: FocalTech FT5726
#define PANEL_TV101WXM_FT_9365 UINT8_C(0x07)

// Panel: BOE TV070WSM-TG1
// DDIC: Fitipower JD9365
// Board: Nelson
// Touch IC: Goodix GT6853 (Nelson)
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

// Panel: BOE TV070WSM-TG1
// DDIC: Fitipower JD9364
// Board: Nelson
// Touch IC: Goodix GT6853 (Nelson)
#define PANEL_TV070WSM_FT_NELSON UINT8_C(0x0d)

#define PANEL_UNKNOWN UINT8_C(0xFF)

typedef struct {
  uint32_t width;
  uint32_t height;
  uint32_t panel_type;
} display_panel_t;

#endif  // SRC_GRAPHICS_DISPLAY_LIB_DEVICE_PROTOCOL_DISPLAY_INCLUDE_LIB_DEVICE_PROTOCOL_DISPLAY_PANEL_H_
