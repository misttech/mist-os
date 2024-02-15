// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_INITCODES_INL_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_INITCODES_INL_H_

#include <lib/mipi-dsi/mipi-dsi.h>

#include <cstdint>

#include "src/graphics/display/drivers/amlogic-display/panel-config.h"

namespace amlogic_display {

// clang-format off
constexpr PowerOp kLcdPowerOnSequenceForAstroSherlockNelson[] = {
    { kPowerOpGpio, 1, 0, 200 }, // This op is an upstream bug.
    { kPowerOpSignal, 0, 0, 0 },
    { kPowerOpExit, 0, 0, 0 },
};

constexpr PowerOp kLcdPowerOffSequenceForAstroSherlockNelson[] = {
    { kPowerOpSignal, 0, 0, 5 },
    { kPowerOpGpio, 0, 0, 20 },
    { kPowerOpGpio, 1, 1, 100 },  // This op is an upstream bug
    { kPowerOpExit, 0, 0, 0 },
};

constexpr PowerOp kLcdPowerOnSequenceForVim3Ts050[] = {
    // In order to power on the MTF050FHDI panel (used by Khadas TS050
    // touchscreen), the "LCD_RESET" pin must be set to high first, and then
    // set to low for at least 9us to initiate the reset procedure, and finally
    // set to high.  Within 10ms after the rising edge the display is reset to
    // its initial condition loaded from ROM.
    //
    // The sleep time used here is from Khadas-provided bringup code.
    //
    // Microtech MTF050FHDI-03 Specification Sheet, Version 1.0, dated July 7,
    // 2015, page 11.
    { kPowerOpGpio, 0, 1, /*sleep_ms=*/ 10 },
    { kPowerOpGpio, 0, 0, /*sleep_ms=*/ 10 },
    { kPowerOpGpio, 0, 1, /*sleep_ms=*/ 10 },
    { kPowerOpSignal, 0, 0, 0 },
    { kPowerOpExit, 0, 0, 0 },
};

constexpr PowerOp kLcdPowerOffSequenceForVim3Ts050[] = {
    { kPowerOpSignal, 0, 0, /*sleep_ms=*/ 5 },
    // Enter the reset state; the display will be blank (black) while in this
    // state.
    //
    // The 0ms sleep time used here is from Khadas-provided bringup code.
    //
    // Microtech MTF050FHDI-03 Specification Sheet, Version 1.0, dated July 7,
    // 2015, page 11.
    { kPowerOpGpio, 0, 0, /*sleep_ms=*/ 0 },
    { kPowerOpExit, 0, 0, 0 },
};

// For better readability, all the DSI sequence codes must follow the following
// format guidelines:
// * Indentation of 4 spaces in arrays.
// * All hexadecimal numbers use "0xAA" format.
// * Only leave one space (ASCII 0x20) between values; do not align the numbers
//   using tabs / spaces.
// * Only use "//" for comments.
// * Each separate DSI command must be on its own line.
//   - All command parameters must be on the same line. Line wrapping is not
//     allowed to make it easier for tools / scripts to parse the code.
//   - The command type must be a DsiOpcode or a MIPI-DSI data type constant.
//   - The payload size must be in decimal.

constexpr uint8_t lcd_shutdown_sequence[] = {
    0xff, 5,
    0x05, 1, 0x28,
    0xff, 60,
    0x05, 1, 0x10,
    0xff, 110,
    0xff, 5,
    0xff, 0xff,
};

constexpr uint8_t lcd_init_sequence_TV070WSM_FT_ASTRO[] = {
    kDsiOpSleep, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpGpio, 3, 0, 0, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpReadReg, 2, 4, 3,
    kDsiOpSleep, 10,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe1, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0xe2, 0x65,
    kMipiDsiDtGenShortWrite2, 2, 0xe3, 0xf8,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x90,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x90,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0xb0,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0xb0,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x3e,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x2f,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x2f,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x69,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x05,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x90,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x80,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x99,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x3c,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x0d,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x89,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x27,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x7c,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x67,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x58,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x4c,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x3c,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x3b,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x36,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x53,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x3f,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x7c,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x67,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x58,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x4c,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x3c,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x3b,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x36,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x53,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x3f,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x05, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x06, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x07, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x08, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0d, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x0f, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x10, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x11, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x13, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x14, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x15, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x16, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x23, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x24, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x25, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x27, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x28, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x29, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x20,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x32,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x08,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x88,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x07,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x5d,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xaf, 0x20,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x2b,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe6, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe7, 0x0c,
    kMipiDsiDtDcsShortWrite0, 1, 0x11,
    kDsiOpSleep, 120,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x2c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x30, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x31, 0xcc,
    kMipiDsiDtGenShortWrite2, 2, 0x32, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x33, 0xc9,
    kMipiDsiDtGenShortWrite2, 2, 0x34, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0xc0,
    kMipiDsiDtGenShortWrite2, 2, 0x36, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0xb3,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0xab,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x3b, 0x9d,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0x8f,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0x6d,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0xd8,
    kMipiDsiDtGenShortWrite2, 2, 0x46, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x47, 0x60,
    kMipiDsiDtGenShortWrite2, 2, 0x48, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x49, 0xeb,
    kMipiDsiDtGenShortWrite2, 2, 0x4a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0xe5,
    kMipiDsiDtGenShortWrite2, 2, 0x4c, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x4d, 0x6c,
    kMipiDsiDtGenShortWrite2, 2, 0x4e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x4f, 0xf2,
    kMipiDsiDtGenShortWrite2, 2, 0x50, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0xb4,
    kMipiDsiDtGenShortWrite2, 2, 0x52, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x74,
    kMipiDsiDtGenShortWrite2, 2, 0x54, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x54,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x26,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x18,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x9e,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x9b,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x94,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x8c,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x85,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x76,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x67,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x4b,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0xf7,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0xb8,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0xd6,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0xd0,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x5c,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x83, 0xe7,
    kMipiDsiDtGenShortWrite2, 2, 0x84, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x85, 0xaa,
    kMipiDsiDtGenShortWrite2, 2, 0x86, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x87, 0x74,
    kMipiDsiDtGenShortWrite2, 2, 0x88, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x89, 0x5a,
    kMipiDsiDtGenShortWrite2, 2, 0x8a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x8b, 0x3c,
    kMipiDsiDtGenShortWrite2, 2, 0x8c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x8d, 0x2c,
    kMipiDsiDtGenShortWrite2, 2, 0x8e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x8f, 0x1c,
    kMipiDsiDtGenShortWrite2, 2, 0x90, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x91, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x92, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x93, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x94, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x95, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x96, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x97, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtDcsShortWrite0, 1, 0x29,
    kDsiOpSleep, 5,
};

constexpr uint8_t lcd_init_sequence_TV070WSM_FT_9365[] = {
    // Sleeps for 10 milliseconds.
    //
    // BOE TV070WSM-TG1 spec, Section 8.0 "Power Sequence", page 21 states that
    // the interval between VCC being ready and display reset must be greater
    // than 10ms.
    kDsiOpSleep, 10,

    // Resets the panel by setting the GPIO LCD RESET pin.
    // The GPIO pin #0 is bound to the display panel's reset (LCD RESET) pin.

    // Pulls up the GPIO LCD RESET pin. Waits for 30 milliseconds.
    kDsiOpGpio, 3, 0, 1, 30,

    // Pulls down the GPIO LCD RESET pin. Waits for 10 milliseconds.
    //
    // BOE TV070WSM-TG1 spec, Section 8.0 "Power Sequence", page 21 states that
    // the duration of the RESET pin being pulled down should be greater than
    // 10 microseconds.
    kDsiOpGpio, 3, 0, 0, 10,

    // Pulls up the GPIO LCD RESET pin to finish the reset. Waits for 30
    // milliseconds.
    //
    // BOE TV070WSM-TG1 spec, Section 8.0 "Power Sequence", page 21 states that
    // interval from display reset to entering MIPI low-power mode should be
    // greater than 5 milliseconds.
    kDsiOpGpio, 3, 0, 1, 30,

    // RDDIDIF (Read Display Identification Information) - MIPI DCS command 0x04
    //
    // Reads 3 8-bit registers containing the display identification
    // information.
    //
    // JD9365 datasheet Section 10.2.3, page 130
    //
    // The 1st parameter identifies the LCD module’s manufacturer.
    // The 2nd parameter defines the panel type and tracks the LCD module /
    // driver version.
    // The 3rd parameter identifies the LCD module and driver.
    kDsiOpReadReg, 2, 4, 3,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 0.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,

    // SET_PASSWORD - 0xe1-0xe3 on all pages
    //
    // JD9365 user guide Section 2.6.5, page 23
    //
    // The password (0x93, 0x65, 0xf8) enables standard DCS commands and all
    // in-house registers.
    kMipiDsiDtGenShortWrite2, 2, 0xe1, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0xe2, 0x65,
    kMipiDsiDtGenShortWrite2, 2, 0xe3, 0xf8,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 1.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,

    // Panel Voltage Setup

    // VCOM_SET (Set VCOM Voltage) - User page 1, 0x00-0x01
    //
    // Sets the panel common voltage (VCOM) when the gates are scanned
    // normally, i.e. from the top to the bottom.
    //
    // JD9365 user guide Section 2.7.1, page 28
    //
    // Sets the VCOM[8:0] register to 0x090.
    // The VCOM voltage is set to
    //     0.3 V - 10 mV * VCOM[8:0] = 0.3 V - 10mV * 144 = -1.14 V.
    //
    // SETPANEL sets the gate scanning order to top-to-bottom, so this voltage
    // setting is effective.
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x90,

    // VCOM_R_SET (Set VCOM Voltage for reverse scan) - User page 1, 0x03-0x04
    //
    // Sets the panel common voltage (VCOM) when the gates are scanned
    // reversely, i.e. from the bottom to the top.
    //
    // JD9365 user guide Section 2.7.2, page 29
    //
    // Sets the VCOM_R[8:0] register to 0x090.
    // The VCOM voltage is set to
    //     0.3 V - 10 mV * VCOM_R[8:0] = 0.3 V - 10mV * 144 = -1.14 V.
    //
    // SETPANEL sets the gate scanning order to top-to-bottom, so this voltage
    // setting is not effective.
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x90,

    // GAMMA_SET (Set Gamma Reference Voltage) - User page 1, 0x17-0x1c
    //
    // Sets the source and reference voltages for the Gamma circuit. The
    // Gamma circuit supplies power for the Source driver.
    //
    // JD9365 user guide Section 2.7.4, page 32
    //
    // Sets the VGMP[8:0] register to 0x0b0.
    // The power source voltage for positive polarity (VGMP) is set to
    //      2.6 V + (VGMP[8:0] - 0x27) * 12.5 mV = 4.3125 V.
    // Sets the VGSP[8:0] register to 0x001.
    // The reference voltage for positive polarity (VGSP) is set to
    //      0.3 V + (VGSP[8:0] - 0x01) * 12.5 mV = 0.3 V.
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0xb0,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x01,
    // Sets the VGMN[8:0] register to 0x0b0.
    // The power source voltage for negative polarity (VGMN) is set to
    //      -2.6 V - (VGMN[8:0] - 0x27) * 12.5 mV = -4.3125 V.
    // Sets the VGSN[8:0] register to 0x001.
    // The reference voltage for negative polarity (VGSN) is set to
    //      -0.3 V - (VGSN[8:0] - 0x01) * 12.5 mV = -0.3 V.
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0xb0,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x01,

    // SETSTBA (Set Source Options) - User page 1, 0x35-0x36
    //
    // Sets the output ability of the operational amplifiers (OP or OP-AMP)
    // of the Gamma circuit and the Source channels.
    //
    // JD9365 user guide Section 2.7.8, page 37
    //
    // Sets the GAP[2:0] register to 0x2.
    // The output bias current of the Gamma circuit OP-AMP is set to
    //          (1 + GAP[2:0]) * IREF = 3 * IREF
    // where IREF stands for the fixed current.
    //
    // Sets the SAP[2:0] register to 0x8.
    // The output bias current of the Source channel OP-AMP is set to
    //          (1 + SAP[2:0]) * IREF = 9 * IREF
    // where IREF stands for the fixed current.
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x28,

    // SETPANEL (Set Panel Related Configurations) - User page 1, 0x37
    //
    // Configures the sources and gates of the panel.
    //
    // JD9365 user guide Section 2.7.9, page 38
    //
    // Sets the Z_line register to 1.
    // The Zig-zag source (selected by ENZ[1:0]) outputs to gates (i.e. lines)
    // of even numbers on the Zig-zag inversion mode.
    //
    // Sets the ENZ[1:0] register to 0b10.
    // The rightmost Zig-zag source (SZ[3]) is used for Zig-zag inversion mode.
    //
    // The two registers above are effective iff the Zig-zag inversion mode is
    // selected in SETRGBCYC. The current SETRGBCYC configuration selects the
    // Zig-zag inversion mode, so these registers are effective.
    //
    // Sets the SS_Panel register to 1.
    // The source voltage signals (S[1:2400]) are scanned horizontally from
    // the right (S[2400]) to the left (S[1]).
    //
    // Sets the GS_Panel register to 0.
    // The gate voltage signals (G[1:1280]) are scanned vertically from the
    // top (G[1]) to the bottom (G[1280]).
    //
    // Sets the REV_Panel register to 0.
    // The panel is normally black.
    //
    // Sets the BGR_Panel register to 1.
    // Source signals are mapped to subpixel components in the order of
    // (B, G, R).
    // For example, for non-zig-zag inversion modes, the first three source
    // signals are (S[1], S[2], S[3]), and they will be mapped to B, G, and R
    // components of a pixel respectively.
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x69,

    // SETRGBCYC (Set Display Waveform Cycles for RGB Mode) - User page 1, 0x38-0x3f
    //
    // Configures the display waveform cycles, i.e. the timings of the source
    // data (SD) signals emitted by the source driver.
    //
    // JD9365 user guide Section 2.7.10, pages 39-40
    //
    // Sets the RGB_JDT[2:0] register to 0b101.
    // The source driver uses the Zig-zag inversion method to drive the
    // source line of the TFT panel.
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x05,
    //
    // Sets the RGB_N_EQ1[7:0] register to 0x00.
    // The duration of the first equalization stage to pull the source driver
    // (SD) output signals to ground voltage (GND) is 0 timing clocks.
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x00,
    //
    // Sets the RGB_N_EQ2[7:0] register to 0x01.
    // The duration of the second equalization stage to pull the source driver
    // (SD) output signals to analog voltage input (VCI) is 1 timing clock, i.e.
    // 4 oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x01,
    //
    // Sets the RGB_N_EQ3[7:0] register to 0x01.
    // The duration of the third equalization stage to pull the source driver
    // (SD) output signals from grayscale voltage (e.g. +V255) back to
    // analog voltage input (VCI) is 144 timing clock, i.e. 144 * 4 = 576
    // oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x90,
    //
    // Configures the output time of the source driver operational amplifier
    // (SD OP).
    //
    // Sets the RGB_CHGEN_ON[7:0] register to 0xff.
    // The charging enable (CHGEN) signal is enabled 255 timing clocks after
    // a horizontal sync signal.
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    //
    // Sets the RGB_CHGEN_OFF[7:0] register to 0xff.
    // The charging enable (CHGEN) signal of the first SD OP is disabled 255
    // timing clocks after a horizontal sync signal. This means the first
    // SD OP is never enabled.
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xff,
    //
    // Sets the RGB_CHGEN_OFF2[7:0] register to 0xff.
    // The charging enable (CHGEN) signal of the second SD OP is disabled 255
    // timing clocks after a horizontal sync signal. This means the second
    // SD OP is never enabled.
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0xff,

    // SET TCON (Timing controller settings) - User page 1, 0x40-0x4c
    //
    // Configures the timing controller.
    //
    // JD9365 user guide Section 2.7.11, pages 41-45
    //
    // Configures the horizontal and vertical resolution to 600 x 1024.
    //
    // Sets the RSO[2:0] register to 0b010.
    // The horizontal resolution is 600 pixels. The display driver IC enables
    // source channels S[1:900] and S[1503:2400].
    //
    // Sets the LN[9:0] register to 0x200.
    // The vertical resolution is
    //     2 * LN[9:0] = 2 * 512 = 1024 lines.
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x80,
    //
    // Sets the SLT[7:0] register to 0x99.
    // The width of the scan line time is
    //     4 * SLT[7:0] = 4 * 153 = 612 oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x99,
    //
    // Sets the VFP[7:0] register to 0x06.
    // The vertical front porch is 6.
    // TODO(https://fxbug.dev/321897820): This doesn't match the display panel
    // timing parameters.
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x06,
    //
    // Sets the VBP[7:0] register to 0x09.
    // The vertical back porch plus the vertical sync width is 9.
    // TODO(https://fxbug.dev/321897820): This doesn't match the display panel
    // timing parameters.
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x09,
    //
    // Sets the HBP[7:0] register to 0x3c.
    // The horizontal back porch plus the horizontal sync width is 60.
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x3c,
    //
    // Sets the bits 15-8 of the TCON_OPT1 register to 0x04.
    // The detailed bit definitions are not available.
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0x04,

    // DCDC_SEL (Power Mode and Charge Pump Settings) - User page 1, 0x55-0x5c
    //
    // Configures the DC / DC converter.
    //
    // JD9365 user guide Section 2.7.13, pages 47-49
    //
    // Sets the DCDCM[3:0] register to 0b0010.
    // Selects the power mode for the positive analog supply voltage (AVDD),
    // negative analog supply voltage (AVEE) and clamped negative supply
    // voltage (VCL).
    //
    // AVDD and AVEE are provided by the NT (Novatek) power IC and VCL is
    // provided by the internal charge pump (CP).
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x02,
    //
    // Sets the AUTO_RT register to 0.
    // The auto pumping ratio function is disabled. The charge pump circuit
    // will not detect voltage of VCI to select suitable ratio for the charge
    // pump circuit.
    //
    // Sets the AVDD_RT[1:0] register to 0x1.
    // If the BOOSTM[1:0] input pins are 0b00, the charge pump ratio of the
    // positive analog supply voltage output (AVDD) is 2.0 x VCIP, where VCIP
    // is the DC/DC setup supply.
    //
    // The JD9365 user guide (page 47) states that AVDD uses the internal
    // charge pump when BOOSTM is 0b00; however, the JD9365D datasheet
    // (page 15) states that when BOOSTM is 0b00, AVEE is under the external
    // power mode, which conflicts with the user guide.
    //
    // The TV070WSM spec doesn't mention the pin configuration of the DDIC, so
    // it's unknown whether this configuration register is effective.
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x01,
    //
    // Sets the VGH_RT[2:0] register to 0x4.
    // The charge pump ratio of the positive gate driver voltage (VGH) is
    // 3 * AVDD - VCL.
    //
    // Sets the VGL_RT[2:0] register to 0x2.
    // The charge pump ratio the negative gate driver voltage (VGL) is
    // AVEE + VCL - AVDD.
    //
    // Sets the VCL_RT[1:0] register to 0x1.
    // The charge pump ratio of the clamped negative analog supply voltage (VCL)
    // to -VCIP.
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x89,
    //
    // Sets the AVDD[4:0] register to 0x0a.
    // Clamps the positive analog supply voltage output (AVDD) to
    // 6.5 V - 0x0a * 100mV = 5.5 V.
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x0a,
    //
    // Sets the AVEE[4:0] register to 0x0a.
    // Clamps the negative analog supply voltage output (AVEE) to
    // -6.5 V + 0x0a * 100mV = -5.5 V.
    //
    // Sets the VCL[2:0] register to 0x00.
    // Clamps the clamped negative analog supply voltage output (VCL) to
    // -2.5 V + 0x00 * 200mV = -2.5 V.
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x0a,
    //
    // Sets the VGH[6:0] register to 0x27.
    // Clamps the positive gate driver voltage (VGH) to
    // 7.0 V + 0x27 * 200mV = 14.8 V.
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x27,
    //
    // Sets the VGL[5:0] register to 0x15.
    // Clamps the negative gate driver voltage (VGL) to
    // -7.0 V - 0x15 * 200mV = -11.2 V.
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x15,

    // SET_GAMMA (Set Gamma Output Voltage) - User page 1, 0x5d-0x82
    //
    // Configures the gamma table to convert 8-bit RGB values to the amplitude
    // of grayscale voltages.
    //
    // JD9365 user guide Section 2.7.14, pages 50-51
    //
    // The following registers specify the amplitude of the reference outputs
    // of the positive polarity grayscale voltages (VOP) for 18 predefined RGB
    // input values, by adjusting the variable resistors.
    //
    // RPA18 / VPR18.
    // Sets the variable resistor for the reference grayscale voltage output on
    // positive polarities (VOP) for input of 255:
    // VOP255 = (360 - 128 + VPR18) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.989 * VGMP + 0.011 * VGSP
    // where VGMP is the power source voltage for positive polarities, and
    // VGSP is the reference voltage for positive polarities (ditto for the
    // rest of the definitions).
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x7c,
    //
    // RPA17 / VPR17.
    // Sets the variable resistor for the VOP for input of 251:
    // VOP251 = (360 - 128 + VPR17) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.925 * VGMP + 0.075 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x65,
    //
    // RPA16 / VPR16.
    // Sets the variable resistor for the VOP for input of 247:
    // VOP247 = (360 - 128 + VPR16) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.875 * VGMP + 0.125 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x53,
    //
    // RPA15 / VPR15.
    // Sets the variable resistor for the VOP for input of 243:
    // VOP243 = (360 - 128 + VPR15) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.839 * VGMP + 0.161 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x46,
    //
    // RPA14 / VPR14.
    // Sets the variable resistor for the VOP for input of 235:
    // VOP235 = (344 - 128 + VPR14) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.781 * VGMP + 0.219 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x41,
    //
    // RPA13 / VPR13.
    // Sets the variable resistor for the VOP for input of 227:
    // VOP227 = (344 - 128 + VPR13) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.742 * VGMP + 0.258 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x33,
    //
    // RPA12 / VPR12.
    // Sets the variable resistor for the VOP for input of 211:
    // VOP211 = (316 - 128 + VPR12) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.669 * VGMP + 0.331 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x35,
    //
    // RPA11 / VPR11.
    // Sets the variable resistor for the VOP for input of 191:
    // VOP191 = (316 - 128 + VPR11) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.606 * VGMP + 0.394 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x1e,
    //
    // RPA10 / VPR10.
    // Sets the variable resistor for the VOP for input of 159:
    // VOP159 = (264 - 128 + VPR10) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.528 * VGMP + 0.472 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x36,
    //
    // RPA9 / VPR9.
    // Sets the variable resistor for the VOP for input of 128:
    // VOP128 = (244 - 128 + VPR9) / 360 * (VGMP - VGSP) + VGSP
    //        = 0.467 * VGMP + 0.533 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x34,
    //
    // RPA8 / VPR8.
    // Sets the variable resistor for the VOP for input of 96:
    // VOP96 = (224 - 128 + VPR8) / 360 * (VGMP - VGSP) + VGSP
    //       = 0.411 * VGMP + 0.589 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x34,
    //
    // RPA7 / VPR7.
    // Sets the variable resistor for the VOP for input of 64:
    // VOP64 = (172 - 128 + VPR7) / 360 * (VGMP - VGSP) + VGSP
    //       = 0.347 * VGMP + 0.653 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x51,
    //
    // RPA6 / VPR6.
    // Sets the variable resistor for the VOP for input of 44:
    // VOP44 = (172 - 128 + VPR6) / 360 * (VGMP - VGSP) + VGSP
    //       = 0.294 * VGMP + 0.706 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x3e,
    //
    // RPA5 / VPR5.
    // Sets the variable resistor for the VOP for input of 28:
    // VOP28 = (144 - 128 + VPR5) / 360 * (VGMP - VGSP) + VGSP
    //       = 0.233 * VGMP + 0.767 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x44,
    //
    // RPA4 / VPR4.
    // Sets the variable resistor for the VOP for input of 20:
    // VOP20 = (144 - 128 + VPR4) / 360 * (VGMP - VGSP) + VGSP
    //       = 0.192 * VGMP + 0.809 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x35,
    //
    // RPA3 / VPR3.
    // Sets the variable resistor for the VOP for input of 12:
    // VOP12 = VPR3 / 360 * (VGMP - VGSP) + VGSP
    //       = 0.128 * VGMP + 0.872 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x2e,
    //
    // RPA2 / VPR2.
    // Sets the variable resistor for the VOP for input of 8:
    // VOP8 = VPR2 / 360 * (VGMP - VGSP) + VGSP
    //      = 0.086 * VGMP + 0.914 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x1f,
    //
    // RPA1 / VPR1.
    // Sets the variable resistor for the VOP for input of 4:
    // VOP4 = VPR1 / 360 * (VGMP - VGSP) + VGSP
    //      = 0.033 * VGMP + 0.967 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x0c,
    // RPA0 / VPR0.
    // Sets the variable resistor for the VOP for input of 0:
    // VOP0 = VPR0 / 360 * (VGMP - VGSP) + VGSP
    //      = 0 * VGMP + 1 * VGSP
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x00,

    // The following registers specify the amplitude of the reference outputs
    // of the negative polarity grayscale voltages (VON) for 18 predefined RGB
    // input values, by adjusting the variable resistors.
    //
    // For this panel, the gamma values defined for negative polarities are the
    // same as  the gamma values for positive polarities.
    //
    // RNA18 / VNR18.
    // Sets the variable resistor for the reference grayscale voltage output on
    // negative polarities (VON) for input of 255:
    // VON255 = (360 - 128 + VNR18) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.989 * VGMN + 0.011 * VGSN
    // where VGMN is the power source voltage for negative polarities, and
    // VGSN is the reference voltage for negative polarities (ditto for the
    // rest of the definitions).
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x7c,
    //
    // RNA17 / VNR17.
    // Sets the variable resistor for the VON for input of 251:
    // VON251 = (360 - 128 + VNR17) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.925 * VGMN + 0.075 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x65,
    //
    // RNA16 / VNR16.
    // Sets the variable resistor for the VON for input of 247:
    // VON247 = (360 - 128 + VNR16) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.875 * VGMN + 0.125 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x53,
    //
    // RNA15 / VNR15.
    // Sets the variable resistor for the VON for input of 243:
    // VON243 = (360 - 128 + VNR15) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.839 * VGMN + 0.161 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x46,
    //
    // RNA14 / VNR14.
    // Sets the variable resistor for the VON for input of 235:
    // VON235 = (344 - 128 + VNR14) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.781 * VGMN + 0.219 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x41,
    //
    // RNA13 / VNR13.
    // Sets the variable resistor for the VON for input of 227:
    // VON227 = (344 - 128 + VNR13) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.742 * VGMN + 0.258 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x33,
    //
    // RNA12 / VNR12.
    // Sets the variable resistor for the VON for input of 211:
    // VON211 = (316 - 128 + VNR12) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.669 * VGMN + 0.331 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x35,
    //
    // RNA11 / VNR11.
    // Sets the variable resistor for the VON for input of 191:
    // VON191 = (316 - 128 + VNR11) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.606 * VGMN + 0.394 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x1e,
    //
    // RNA10 / VNR10.
    // Sets the variable resistor for the VON for input of 159:
    // VON159 = (264 - 128 + VNR10) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.528 * VGMN + 0.472 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x36,
    //
    // RNA9 / VNR9.
    // Sets the variable resistor for the VON for input of 128:
    // VON128 = (244 - 128 + VNR9) / 360 * (VGMN - VGSN) + VGSN
    //        = 0.467 * VGMN + 0.533 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x34,
    //
    // RNA8 / VNR8.
    // Sets the variable resistor for the VON for input of 96:
    // VON96 = (224 - 128 + VNR8) / 360 * (VGMN - VGSN) + VGSN
    //       = 0.411 * VGMN + 0.589 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x34,
    // RNA7 / VNR7.
    // Sets the variable resistor for the VON for input of 64:
    // VON64 = (172 - 128 + VNR7) / 360 * (VGMN - VGSN) + VGSN
    //       = 0.347 * VGMN + 0.653 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x51,
    //
    // RNA6 / VNR6.
    // Sets the variable resistor for the VON for input of 44:
    // VON44 = (172 - 128 + VNR6) / 360 * (VGMN - VGSN) + VGSN
    //       = 0.294 * VGMN + 0.706 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x3e,
    //
    // RNA5 / VNR5.
    // Sets the variable resistor for the VON for input of 28:
    // VON28 = (144 - 128 + VNR5) / 360 * (VGMN - VGSN) + VGSN
    //       = 0.233 * VGMN + 0.767 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x44,
    //
    // RNA4 / VNR4.
    // Sets the variable resistor for the VON for input of 20:
    // VON20 = (144 - 128 + VNR4) / 360 * (VGMN - VGSN) + VGSN
    //       = 0.192 * VGMN + 0.809 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x35,
    //
    // RNA3 / VNR3.
    // Sets the variable resistor for the VON for input of 12:
    // VON12 = VNR3 / 360 * (VGMN - VGSN) + VGSN
    //       = 0.128 * VGMN + 0.872 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0x2e,
    //
    // RNA2 / VNR2.
    // Sets the variable resistor for the VON for input of 8:
    // VON8 = VNR2 / 360 * (VGMN - VGSN) + VGSN
    //      = 0.086 * VGMN + 0.914 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x1f,
    //
    // RNA1 / VNR1.
    // Sets the variable resistor for the VON for input of 4:
    // VON4 = VNR1 / 360 * (VGMN - VGSN) + VGSN
    //      = 0.033 * VGMN + 0.967 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x0c,
    //
    // RNA0 / VNR0.
    // Sets the variable resistor for the VON for input of 0:
    // VON0 = VNR0 / 360 * (VGMN - VGSN) + VGSN
    //      = 0 * VGMN + 1 * VGSN
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x00,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 2.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x02,

    // SET_GIP_L (SET CGOUTx_L Signal Mapping, GS_Panel=0) - User page 2, 0x00-0x15
    //
    // Maps the timing controller output signals to gate-in-panel control output
    // pins on the left side of panel. Effective iff the gate scan output is
    // top-to-bottom.
    //
    // JD9365 user guide Section 2.8.1, page 53
    //
    // There are 22 gate-in-panel (GIP) control output (CGOUT) pins for the
    // left side of the panel, named CGOUT(1-22)_L. The following registers map
    // the output signals to CGOUT pins.
    //
    // Because SETPANEL sets the scan direction to top-to-bottom, the following
    // configurations are effective.
    //
    // CGOUT1_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV1 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x45,
    //
    // CGOUT2_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV1 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x45,
    //
    // CGOUT3_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV3 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0x47,
    //
    // CGOUT4_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV3 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x47,
    //
    // CGOUT5_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to STV1 (vertical start pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x41,
    //
    // CGOUT6_L: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to STV1 (vertical start pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x05, 0x41,
    //
    // CGOUT7_L, ..., CGOUT13_L: Pulls to VGL on abnormal power off.
    // CGOUT is normal drive. Maps to VGL (negative gate driver output).
    kMipiDsiDtGenShortWrite2, 2, 0x06, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x07, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x08, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x1f,
    //
    // CGOUT14_L, CGOUT15_L, CGOUT16_L: Pulls to VGL on abnormal power off.
    // CGOUT is normal drive. Floating (no output mapping).
    kMipiDsiDtGenShortWrite2, 2, 0x0d, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x0f, 0x1d,
    //
    // CGOUT17_L, ..., CGOUT22_L: Pulls to VGL on abnormal power off.
    // CGOUT is normal drive. Maps to VGL (negative gate driver output).
    kMipiDsiDtGenShortWrite2, 2, 0x10, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x11, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x13, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x14, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x15, 0x1f,

    // SET_GIP_R (SET CGOUTx_R Signal Mapping, GS_Panel=0) - User page 2, 0x16-0x2b
    //
    // Maps the timing controller output signals to gate-in-panel control output
    // pins on the right side of panel. Effective iff the gate scan output is
    // top-to-bottom.
    //
    // JD9365 user guide Section 2.8.2, page 54
    //
    // There are 22 Gate-in-panel (GIP) control output (CGOUT) pins for the
    // right side of the panel, named CGOUT(1-22)_R. The following registers map
    // the output signals to CGOUT pins.
    //
    // Because SETPANEL sets the scan direction to top-to-bottom, the following
    // configurations are effective.
    //
    // CGOUT1_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV0 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x16, 0x44,
    //
    // CGOUT2_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV0 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x44,
    //
    // CGOUT3_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV2 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0x46,
    //
    // CGOUT4_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to CKV2 (vertical clock pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x46,
    //
    // CGOUT5_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to STV0 (vertical start pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x40,
    //
    // CGOUT6_R: Pulls to VGH on abnormal power off. CGOUT is normal drive.
    //           Maps to STV0 (vertical start pulse).
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0x40,
    //
    // CGOUT7_R, ..., CGOUT13_R: Pulls to VGH on abnormal power off.
    // CGOUT is normal drive. Maps to VGL (negative gate driver output).
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x1f,
    //
    // CGOUT14_R, CGOUT15_R, CGOUT16_R: Pulls to VGL on abnormal power off.
    // CGOUT is normal drive. Floating (no output mapping).
    kMipiDsiDtGenShortWrite2, 2, 0x23, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x24, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x25, 0x1d,
    //
    // CGOUT17_R, ..., CGOUT22_R: Pulls to VGH on abnormal power off.
    // CGOUT is normal drive. Maps to VGL (negative gate driver output).
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x27, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x28, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x29, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x1f,

    // SETGIP1 (Set GIP Signal Timing 1) - User page 2, 0x58-0x6b
    //
    // Configures the gate-in-panel (GIP) signal timing.
    //
    // JD9365 user guide Section 2.8.5, pages 57, 59-63
    //
    // Sets GIP_GAS_OPT to true.
    // On abnormal power off, CGOUT signals are pulled to voltages specified in
    // SET_GIP_{L,R}.
    //
    // Sets INIT_PORCH[3:0] to 0.
    // The initialization signal porch size is 0 frames, i.e. no panel
    // initialization signal is emitted.
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x40,
    //
    // Sets INIT_W[3:0] to 0.
    // The initialization signal frame width is 0 frames.
    // Because no panel initialization signal is emitted, this is not effective.
    //
    // Sets INIT[10:0] to 0.
    // The initialization signal line width is 0 lines.
    // Because no panel initialization signal is emitted, this is not effective.
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    //
    // Sets STV_NUM[1:0] to 0x1.
    // Enables 2 vertical start pulse signals: STV0 and STV1.
    //
    // Sets STV_S0[10:0] to 0x007.
    // Sets the initial phase of the first vertical start pulse signal (STV0).
    // The first vertical start pulse signal (STV0) is emitted 7 lines after
    // the beginning of a vertical sync pulse.
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x07,
    //
    // Sets STV_W[3:0] to 0x2.
    // The duration (width) of all the vertical start pulse signals are
    //   1 + STV_W[3:0] = 1 + 2 = 3 pixel clocks.
    //
    // Sets STV_S1[2:0] to 0.
    // The phase difference between vertical start pulse signals STV0 and STV1
    // is zero. Thus, the STV0 and STV1 signals are emitted at the same time.
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x20,
    //
    // Sets STV_S2[4:0] to 0.
    // The phase difference between vertical start pulse signals STV0 and STV2
    // is zero.
    // The STV2 signal is disabled, thus this register configuration won't take
    // effect.
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x00,
    //
    // Sets STV_S3[4:0] to 0.
    // The phase difference between vertical start pulse signals STV0 and STV3
    // is zero.
    // The STV3 signal is disabled, thus this register configuration won't take
    // effect.
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x00,
    //
    // Sets ETV_S2[4:0] to 0.
    // The phase difference between vertical end pulse signals EVT0 and EVT2
    // is zero.
    // The ETV (vertical end pulse) signals are not used by the panel.
    // This register configuration won't take effect.
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x00,
    //
    // Sets ETV_S3[4:0] to 0.
    // The phase difference between vertical end pulse signals EVT0 and EVT3
    // is zero.
    // The ETV (vertical end pulse) signals are not used. This register
    // configuration won't take effect.
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x00,
    //
    // Sets SETV_ON[7:0] to 0x7a.
    // Fine-tunes the starting point of the STV / ETV signals.
    //
    // The duration between the end of a horizontal sync pulse and the
    // beginning of the vertical start (end) pulse right after it is 0x7a = 122
    // oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x7a,
    //
    // Sets SETV_OFF[7:0] to 0x7a.
    // Fine-tunes the end point of the STV / ETV signals.
    //
    // The duration between the end of the vertical start (end) pulse and the
    // end of the horizontal sync pulse right before it is 0x7a = 122
    // oscillator periods.
    //
    // For example, in the diagram below, the HSYNC signal is falling edge
    // triggered, and the STV signal is rising edge triggered.
    // The duration A stands for SETV_ON, and the duration B stands for SETV_OFF.
    //
    // HSYNC ---__-----------__------------__------------__
    // STV   ____________---------------------------___
    //            |<-A->|                    |<-B->|
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x7a,
    //
    // Registers 0x65 and 0x66 configure the vertical end pulse (ETV) signals.
    // Since the ETV signals are not provided to the panel, the configuration
    // registers are not effective and are thus not documented.
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x00,
    //
    // Sets CKV0_NUM[3:0] to 3.
    // Enables 4 vertical clock (CKV) signals: CKV0, CKV1, CKV2 and CKV3.
    //
    // Sets CKV0_W[2:0] to 2.
    // The duration (width) of a vertical clock (CKV) signal is
    // 1 + CKV0_W[2:0] = 1 + 2 = 3 lines.
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x32,
    //
    // Sets CKV0_S0[7:0] to 8.
    // Sets the initial phase of the first vertical clock signal (CKV0).
    // The first vertical clock signal (CKV0) is emitted 8 pixels after
    // the beginning of a vertical sync pulse.
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x08,
    //
    // Sets CKV0_ON[7:0] to 0x7a.
    // Fine-tunes the starting point of the CKV signals.
    //
    // The duration between the end of a horizontal sync pulse and the
    // beginning of the vertical clock (CKV) pulse right after it is 0x7a = 122
    // oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x7a,
    //
    // Sets CKV0_OFF[7:0] to 0x7a.
    // Fine-tunes the end point of the CKV signals.
    //
    // The duration between the end of the vertical clock (CKV) pulse and the
    // end of the horizontal sync pulse right before it is 0x7a = 122
    // oscillator periods.
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x7a,
    //
    // Sets CKV0_DUM[7:0] to 0.
    // If CKV0_CON is zero, a total amount of CKV0_DUM[7:0] placeholder
    // vertical clock (CKV) pulses are emitted after the active lines every
    // frame. In this case, if CKV0_CON is zero, no placeholder vertical clock
    // pulses will be emitted after active lines.
    //
    // On the current configuration, CKV0_CON is 1, thus this register
    // is not effective.
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x00,

    // SETGIP2 (Set GIP Signal Timing 2) - User page 2, 0x6c-0x7e
    //
    // Configures the gate-in-panel (GIP) signal timing.
    //
    // JD9365 user guide Section 2.8.6, pages 58-63
    //
    // Sets EOLR, GEQ_LINE and GEQ_W[3:0] to 0.
    // The above registers are not documented.
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x00,
    //
    // Sets GEQ_GGND1[5:0] to 0x04.
    // The duration of the equalization period to turn on a gate GIP signal is
    // 4 oscillator clocks.
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x04,
    //
    // Sets GEQ_GGND2[5:0] to 0x04.
    // The duration of the equalization period to turn off a gate GIP signal is
    // 4 oscillator clocks.
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x04,
    //
    // Sets GIPDR[1:0] = 0b10, VGHO_SEL = 0, VGLO_SEL = 0, VGLO_SEL2 = 1,
    // CKV_GROUP = 0.
    // The registers above are not documented.
    //
    // Sets CKV0_CON = 1.
    // Overrides the CKV0_DUM configuration. CKV signals CKV0-CKV3 always output
    // on blanking areas.
    //
    // Sets CKV1_CON = 0.
    // Does not override the CKV1_DUM configuration. CKV1 signals (CKV8-CKV11)
    // are not enabled, thus this register is not effective.
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x89,
    //
    // Sets CKV1_NUM[3:0] = 0.
    // No CKV1 signal (CKV8-CKV11) is enabled.
    //
    // Sets CKV1_W[3:0] = 0.
    // The width (duration) of CKV1 signals is 0 pixels. No CKV1 signal is
    // enabled, so this register is not effective.
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x00,
    //
    // Registers 0x70-0x74 further configure the vertical clock signals
    // CKV8-CKV11.
    // Since these CKV signals are not enabled, the configuration registers
    // are not effective and are thus not documented.
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x00,
    //
    // Sets FLM_EN to 0.
    // Disables the generation of the first line marker signal (FLM).
    //
    // Other fields on registers 0x75-0x78 further configure the first line
    // marker signal (FLM). Since the FLM signal is not enabled, the
    // configuration registers are not effective and are thus not documented.
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x07,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x00,
    //
    // Sets VEN_EN to 0.
    // Disables the generation of the VEN (Vertical enabled?) signal.
    //
    // Other fields on registers 0x77, 0x79-0x7e further configure the VEN
    // signal. Since the VEN signal is not enabled, the configuration
    // registers are not effective and are thus not documented.
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x5d,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x7b,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 3.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,

    // The addresses 0xa9 and 0xac are not documented in the JD9365 datasheet
    // nor the JD9365 user guide.
    kMipiDsiDtGenShortWrite2, 2, 0xa9, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xac, 0x4d,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 4.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,

    // The addresses 0x00, 0x02 and 0x09 are not documented in the JD9365
    // datasheet nor the JD9365 user guide.
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0xb3,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x60,

    // SETSTBA2 (Set IF Source Switch Control Timing) - User page 4, 0x0c-0x0f
    //
    // JD9365 user guide Section 2.10.1, page 71
    //
    // Sets SDS[14:13] to 0b10.
    // The PEQ power source is PCAP.
    //
    // Sets SDS[11] to 0.
    // The NEQ power source is NCAP.
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x48,

    // SET_PAGE - 0xe0 on all pages
    //
    // JD9365 user guide Section 2.6.4, page 22
    //
    // Sets command page to user page 0.
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,

    // WRDISBV (Write Display Brightness) - MIPI DCS command 0x51
    //
    // Adjusts the brightness value of the display.
    //
    // JD9365 datasheet Section 10.2.35, page 164
    //
    // Sets the display brightness value to 255.
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0xff,

    // WRCTRLD (Write CTRL Display) - MIPI DCS command 0x53
    //
    // Configures the brightness / backlight control blocks.
    //
    // JD9365 datasheet Section 10.2.37, page 166
    //
    // Sets BCTRL to 1.
    // Enables the brightness control block.
    //
    // Sets DD to 1.
    // Enables display dimming.
    //
    // Sets BL to 1.
    // Enables the backlight control circuit.
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x2c,

    // Sleeps for 5 milliseconds.
    kDsiOpSleep, 5,

    // SLPOUT (Exit Sleep In Mode) - MIPI DCS command 0x11
    //
    // Turns off the sleep mode, enables the DC/DC converter, and starts
    // the internal oscillator and the panel scanning procedure.
    //
    // JD9365 datasheet Section 10.2.16, page 143
    kMipiDsiDtDcsShortWrite0, 1, 0x11,

    // Sleeps for 125 milliseconds.
    //
    // BOE TV070WSM-TG1 spec states that the interval between the MIPI-DSI
    // initialization code and the high-speed mode should be greater than 120
    // milliseconds.
    kDsiOpSleep, 125,

    // DISPON (Display On) - MIPI DCS command 0x29
    //
    // Enables output from the frame memory to the display panel.
    //
    // JD9365 datasheet Section 10.2.24, page 152
    kMipiDsiDtDcsShortWrite0, 1, 0x29,

    // TODO(https://fxbug.dev/321897820): We do not explicitly set the power
    // of the LEDs. The power sequence might be invalid for the LCDs.
    // BOE TV070WSM-TG1 spec states that the interval between the display
    // entering the high-speed mode and LED being turned on should be greater
    // than 35 milliseconds.

    // Sleeps for 20 milliseconds.
    kDsiOpSleep, 20,

    // End marker.
    kDsiOpSleep, 0xff,
};

constexpr uint8_t lcd_init_sequence_P070ACB_FT[] = {
    kDsiOpSleep, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpGpio, 3, 0, 0, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpReadReg, 2, 4, 3,
    kDsiOpSleep, 10,

    kDsiOpSleep, 100,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe1, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0xe2, 0x65,
    kMipiDsiDtGenShortWrite2, 2, 0xe3, 0xf8,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x74,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x70,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x2d,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x2d,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x7e,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0xf3,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x90,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x80,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x99,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x19,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x5a,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x69,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x19,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x56,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x27,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x2d,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x18,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x56,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x4f,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x42,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x25,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x56,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x27,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x2d,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x18,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x56,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x4f,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x42,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x25,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x53,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x05, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x06, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x07, 0x4f,
    kMipiDsiDtGenShortWrite2, 2, 0x08, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0a, 0x4b,
    kMipiDsiDtGenShortWrite2, 2, 0x0b, 0x49,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0d, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x0f, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x10, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x11, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x13, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x14, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x15, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x16, 0x52,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x50,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1d, 0x4e,
    kMipiDsiDtGenShortWrite2, 2, 0x1e, 0x4c,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x23, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x24, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x25, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x27, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x28, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x29, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x2c, 0x12,
    kMipiDsiDtGenShortWrite2, 2, 0x2d, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x2f, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x30, 0x37,
    kMipiDsiDtGenShortWrite2, 2, 0x31, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x32, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x33, 0x08,
    kMipiDsiDtGenShortWrite2, 2, 0x34, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x36, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x3b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x13,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x46, 0x37,
    kMipiDsiDtGenShortWrite2, 2, 0x47, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x48, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x49, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x4a, 0x0b,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x4c, 0x0d,
    kMipiDsiDtGenShortWrite2, 2, 0x4d, 0x0f,
    kMipiDsiDtGenShortWrite2, 2, 0x4e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x4f, 0x05,
    kMipiDsiDtGenShortWrite2, 2, 0x50, 0x07,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x52, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x54, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x74,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x16,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0xb4,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x16,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x88,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0xbc,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x2c,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x2b,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe6, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe7, 0x0c,
    kMipiDsiDtDcsShortWrite0, 1, 0x11,
    kDsiOpSleep, 120,
    kMipiDsiDtDcsShortWrite0, 1, 0x29,
    kMipiDsiDtDcsShortWrite0, 1, 0x35,
    kDsiOpSleep, 20,
};

constexpr uint8_t lcd_init_sequence_P101DEZ_FT[] = {
    kDsiOpSleep, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpGpio, 3, 0, 0, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpReadReg, 2, 4, 3,
    kDsiOpSleep, 10,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe1, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0xe2, 0x65,
    kMipiDsiDtGenShortWrite2, 2, 0xe3, 0xf8,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x03,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x5d,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x64,

    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0xc7,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0xc7,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x01,

    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x70,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x2d,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x2d,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x7e,

    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x28,

    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x19,

    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x05,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x7c,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0x7f,

    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0xa0,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x2c,

    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x0f,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x68,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x1a,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x15,

    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x7f,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x61,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x50,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x43,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x3e,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x1c,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x32,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x50,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x3e,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x37,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x32,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x12,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x7f,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x61,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x50,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x43,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x3e,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x1c,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x32,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x50,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x3e,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x37,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0x32,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x12,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x02,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x52,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x50,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x05, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x06, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x07, 0x4e,
    kMipiDsiDtGenShortWrite2, 2, 0x08, 0x4c,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x5f,
    kMipiDsiDtGenShortWrite2, 2, 0x0a, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x0b, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x0d, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x0f, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x10, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x11, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x13, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x14, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x15, 0x55,

    kMipiDsiDtGenShortWrite2, 2, 0x16, 0x53,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x1d, 0x4f,
    kMipiDsiDtGenShortWrite2, 2, 0x1e, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x5f,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x4b,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x49,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x23, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x24, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x25, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x27, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x28, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x29, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x2a, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x55,

    kMipiDsiDtGenShortWrite2, 2, 0x2c, 0x13,
    kMipiDsiDtGenShortWrite2, 2, 0x2d, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x2f, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x30, 0x37,
    kMipiDsiDtGenShortWrite2, 2, 0x31, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x32, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x33, 0x0d,
    kMipiDsiDtGenShortWrite2, 2, 0x34, 0x0f,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x36, 0x05,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x07,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x0b,
    kMipiDsiDtGenShortWrite2, 2, 0x3b, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x15,

    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x12,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x46, 0x37,
    kMipiDsiDtGenShortWrite2, 2, 0x47, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x48, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x49, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x4a, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x4c, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x4d, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x4e, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x4f, 0x08,
    kMipiDsiDtGenShortWrite2, 2, 0x50, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x52, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x54, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x15,

    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x12,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x6c,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x6c,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x75,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0xb4,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x6c,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x6c,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x88,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0xbb,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x02,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xaf, 0x20,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x2b,
    kMipiDsiDtGenShortWrite2, 2, 0x2d, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0xff,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x05,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x72,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe6, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe7, 0x0c,

    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x2c,

    kMipiDsiDtDcsShortWrite0, 1, 0x11,
    kDsiOpSleep, 120,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x01,
    kDsiOpSleep, 10,
    kMipiDsiDtGenShortWrite2, 2, 0x2c, 0x01,

    kMipiDsiDtGenShortWrite2, 2, 0x30, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x31, 0xde,
    kMipiDsiDtGenShortWrite2, 2, 0x32, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x33, 0xda,
    kMipiDsiDtGenShortWrite2, 2, 0x34, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0xd1,
    kMipiDsiDtGenShortWrite2, 2, 0x36, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0xc9,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0xc1,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x3b, 0xb3,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xa4,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0x83,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x62,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x23,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0xe4,
    kMipiDsiDtGenShortWrite2, 2, 0x46, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x47, 0x67,
    kMipiDsiDtGenShortWrite2, 2, 0x48, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x49, 0xec,
    kMipiDsiDtGenShortWrite2, 2, 0x4a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0xe8,
    kMipiDsiDtGenShortWrite2, 2, 0x4c, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x4d, 0x6d,
    kMipiDsiDtGenShortWrite2, 2, 0x4e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x4f, 0xf2,
    kMipiDsiDtGenShortWrite2, 2, 0x50, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0xb2,
    kMipiDsiDtGenShortWrite2, 2, 0x52, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x76,
    kMipiDsiDtGenShortWrite2, 2, 0x54, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x58,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x2a,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x1b,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x13,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x0b,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x00,

    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0xe7,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0xe4,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0xdd,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0xd5,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0xce,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0xbf,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0xb2,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x71,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0xf4,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x75,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0xf7,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0xf3,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x75,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x83, 0xf7,
    kMipiDsiDtGenShortWrite2, 2, 0x84, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x85, 0xb6,
    kMipiDsiDtGenShortWrite2, 2, 0x86, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x87, 0x7c,
    kMipiDsiDtGenShortWrite2, 2, 0x88, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x89, 0x5e,
    kMipiDsiDtGenShortWrite2, 2, 0x8a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x8b, 0x3f,
    kMipiDsiDtGenShortWrite2, 2, 0x8c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x8d, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x8e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x8f, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x90, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x91, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x92, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x93, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x94, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x95, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x96, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x97, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,

    kMipiDsiDtDcsShortWrite0, 1, 0x29,
    kDsiOpSleep, 5,
};

constexpr uint8_t lcd_init_sequence_TV101WXM_FT[] = {
    kDsiOpSleep, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpGpio, 3, 0, 0, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpReadReg, 2, 4, 3,
    kDsiOpSleep, 10,

    kDsiOpSleep, 120,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe1, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0xe2, 0x65,
    kMipiDsiDtGenShortWrite2, 2, 0xe3, 0xf8,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x6f,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0xaf,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0xaf,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x3e,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x28,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x28,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x7e,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x26,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x78,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0x7f,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0xa0,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x81,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x08,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x0b,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x28,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x0f,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x69,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x28,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x7f,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x6a,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x5a,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x4e,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x3a,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x3c,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x23,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x3e,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x21,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x7f,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x6a,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x5a,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x4e,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x3a,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x3c,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x23,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x3e,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x21,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x1e,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x1e,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x43,
    kMipiDsiDtGenShortWrite2, 2, 0x05, 0x43,
    kMipiDsiDtGenShortWrite2, 2, 0x06, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x07, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x08, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0a, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x0b, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0d, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x0f, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x10, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x11, 0x4b,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x4b,
    kMipiDsiDtGenShortWrite2, 2, 0x13, 0x49,
    kMipiDsiDtGenShortWrite2, 2, 0x14, 0x49,
    kMipiDsiDtGenShortWrite2, 2, 0x15, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x16, 0x1e,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x1e,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x42,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0x42,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1e, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x23, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x24, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x25, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x27, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x28, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x29, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x2a, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x30,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x0f,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x30,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x6a,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x73,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x6a,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x08,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x88,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0xdd,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x0f,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x82,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x2b,
    kMipiDsiDtGenShortWrite2, 2, 0x2d, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe6, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe7, 0x0c,
    kMipiDsiDtDcsShortWrite0, 1, 0x11,
    kDsiOpSleep, 100,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x2c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x30, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x31, 0xfc,
    kMipiDsiDtGenShortWrite2, 2, 0x32, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x33, 0xf8,
    kMipiDsiDtGenShortWrite2, 2, 0x34, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0xf0,
    kMipiDsiDtGenShortWrite2, 2, 0x36, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0xe8,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0xe0,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x3b, 0xd0,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xc0,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0xa0,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x80,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x46, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x47, 0x80,
    kMipiDsiDtGenShortWrite2, 2, 0x48, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x49, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x4a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0xfc,
    kMipiDsiDtGenShortWrite2, 2, 0x4c, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x4d, 0x7c,
    kMipiDsiDtGenShortWrite2, 2, 0x4e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x4f, 0xfc,
    kMipiDsiDtGenShortWrite2, 2, 0x50, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0xbc,
    kMipiDsiDtGenShortWrite2, 2, 0x52, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x7c,
    kMipiDsiDtGenShortWrite2, 2, 0x54, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x5c,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x3c,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x2c,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x1c,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0xc9,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0xc6,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0xbe,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0xb7,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0xb1,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0xa3,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x96,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x79,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x5d,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x26,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0xe9,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x6e,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0xf3,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x73,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x83, 0xf5,
    kMipiDsiDtGenShortWrite2, 2, 0x84, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x85, 0xb4,
    kMipiDsiDtGenShortWrite2, 2, 0x86, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x87, 0x79,
    kMipiDsiDtGenShortWrite2, 2, 0x88, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x89, 0x5d,
    kMipiDsiDtGenShortWrite2, 2, 0x8a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x8b, 0x3c,
    kMipiDsiDtGenShortWrite2, 2, 0x8c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x8d, 0x2b,
    kMipiDsiDtGenShortWrite2, 2, 0x8e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x8f, 0x1c,
    kMipiDsiDtGenShortWrite2, 2, 0x90, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x91, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x92, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x93, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x94, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x95, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x96, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x97, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtDcsShortWrite0, 1, 0x29,
    kDsiOpSleep, 0xff,
};

constexpr uint8_t lcd_init_sequence_TV101WXM_FT_9365[] = {
    kDsiOpSleep, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpGpio, 3, 0, 0, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpReadReg, 2, 4, 3,
    kDsiOpSleep, 10,

    kDsiOpSleep, 120,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe1, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0xe2, 0x65,
    kMipiDsiDtGenShortWrite2, 2, 0xe3, 0xf8,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0xaf,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0xaf,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x26,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x78,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0x7f,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0xa0,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x81,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x23,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x28,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x0f,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x69,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x28,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x7f,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x67,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x37,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x21,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x36,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x53,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x26,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x13,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x7f,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x67,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x37,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x21,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x36,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x53,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x26,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x13,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x1e,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x1e,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x43,
    kMipiDsiDtGenShortWrite2, 2, 0x05, 0x43,
    kMipiDsiDtGenShortWrite2, 2, 0x06, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x07, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x08, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0a, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x0b, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0d, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x0f, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x10, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x11, 0x4b,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x4b,
    kMipiDsiDtGenShortWrite2, 2, 0x13, 0x49,
    kMipiDsiDtGenShortWrite2, 2, 0x14, 0x49,
    kMipiDsiDtGenShortWrite2, 2, 0x15, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x16, 0x1e,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x1e,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x42,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0x42,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1e, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x23, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x24, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x25, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x27, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x28, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x29, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x2a, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x30,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x1b,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x30,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x6a,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x73,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x6a,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x08,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x88,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0xdd,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x82,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0xb3,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x61,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x36, 0x2a,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe6, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe7, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x2c,
    kDsiOpSleep, 5,
    kMipiDsiDtDcsShortWrite0, 1, 0x11,
    kDsiOpSleep, 120,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x2c, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x30, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x31, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x32, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x33, 0xed,
    kMipiDsiDtGenShortWrite2, 2, 0x34, 0xdd,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0xde,
    kMipiDsiDtGenShortWrite2, 2, 0x36, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0xee,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x3b, 0xfe,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xfe,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0xfe,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0xfe,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0xfe,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x46, 0xee,
    kMipiDsiDtGenShortWrite2, 2, 0x47, 0xee,
    kMipiDsiDtGenShortWrite2, 2, 0x48, 0xde,
    kMipiDsiDtGenShortWrite2, 2, 0x49, 0xde,
    kMipiDsiDtGenShortWrite2, 2, 0x4a, 0xed,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0xed,
    kMipiDsiDtGenShortWrite2, 2, 0x4c, 0xdd,
    kMipiDsiDtGenShortWrite2, 2, 0x4d, 0xdd,
    kMipiDsiDtGenShortWrite2, 2, 0x4e, 0xdd,
    kMipiDsiDtGenShortWrite2, 2, 0x4f, 0xdd,
    kMipiDsiDtGenShortWrite2, 2, 0x50, 0xde,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x52, 0xf0,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x54, 0x3e,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0xf1,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0xfe,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x21,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x22,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x32,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x22,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x43,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x43,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x43,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x33,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x22,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x43,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x32,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x22,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x21,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x0f,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0xed,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0xcc,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0xbb,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x05,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtDcsShortWrite0, 1, 0x29,
    kDsiOpSleep, 20,
    kMipiDsiDtDcsShortWrite0, 1, 0x35,
    kDsiOpSleep, 0xff,
};

constexpr uint8_t lcd_init_sequence_KD070D82_FT[] = {
    kDsiOpSleep, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpGpio, 3, 0, 0, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpReadReg, 2, 4, 3,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe1, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0xe2, 0x65,
    kMipiDsiDtGenShortWrite2, 2, 0xe3, 0xf8,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x9e,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0xaa,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x74,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x70,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x2d,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x2d,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x7e,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0xf3,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x90,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x80,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x99,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x19,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x5a,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x69,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x19,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x5c,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x3d,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x2f,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x58,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x5c,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x3d,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x2f,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x58,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x53,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x05, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x06, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x07, 0x4f,
    kMipiDsiDtGenShortWrite2, 2, 0x08, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0a, 0x4b,
    kMipiDsiDtGenShortWrite2, 2, 0x0b, 0x49,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0d, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x0f, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x10, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x11, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x13, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x14, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x15, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x16, 0x52,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x50,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1d, 0x4e,
    kMipiDsiDtGenShortWrite2, 2, 0x1e, 0x4c,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x23, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x24, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x25, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x27, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x28, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x29, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x2c, 0x12,
    kMipiDsiDtGenShortWrite2, 2, 0x2d, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x2f, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x30, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x31, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x32, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x33, 0x08,
    kMipiDsiDtGenShortWrite2, 2, 0x34, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x36, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x3b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x13,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x46, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x47, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x48, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x49, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x4a, 0x0b,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x4c, 0x0d,
    kMipiDsiDtGenShortWrite2, 2, 0x4d, 0x0f,
    kMipiDsiDtGenShortWrite2, 2, 0x4e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x4f, 0x05,
    kMipiDsiDtGenShortWrite2, 2, 0x50, 0x07,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x52, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x54, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x74,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x16,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0xb4,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x16,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x88,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0xbc,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x2c,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xaf, 0x20,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x2b,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe6, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe7, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x2c,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x00,
    kMipiDsiDtDcsShortWrite0, 1, 0x11,
    kDsiOpSleep, 125,
    kMipiDsiDtDcsShortWrite0, 1, 0x29,
    kDsiOpSleep, 0xff,
};

constexpr uint8_t lcd_init_sequence_KD070D82_FT_9365[] = {
    kDsiOpSleep, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpGpio, 3, 0, 0, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpReadReg, 2, 4, 3,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe1, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0xe2, 0x65,
    kMipiDsiDtGenShortWrite2, 2, 0xe3, 0xf8,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x9e,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0xaa,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x74,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0xef,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x90,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x80,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x99,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x19,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x5a,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x69,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x19,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x5c,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x3d,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x2f,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x58,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x5c,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x3d,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x2f,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x34,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x39,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x58,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x53,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x51,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x05, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x06, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x07, 0x4f,
    kMipiDsiDtGenShortWrite2, 2, 0x08, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0a, 0x4b,
    kMipiDsiDtGenShortWrite2, 2, 0x0b, 0x49,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0d, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x0f, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x10, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x11, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x13, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x14, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x15, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x16, 0x52,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x50,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x77,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0x57,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1d, 0x4e,
    kMipiDsiDtGenShortWrite2, 2, 0x1e, 0x4c,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x4a,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x23, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x24, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x25, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x27, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x28, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x29, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x55,
    kMipiDsiDtGenShortWrite2, 2, 0x2c, 0x12,
    kMipiDsiDtGenShortWrite2, 2, 0x2d, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x2f, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x30, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x31, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x32, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x33, 0x08,
    kMipiDsiDtGenShortWrite2, 2, 0x34, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x36, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x3b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x13,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x46, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x47, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x48, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x49, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x4a, 0x0b,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x4c, 0x0d,
    kMipiDsiDtGenShortWrite2, 2, 0x4d, 0x0f,
    kMipiDsiDtGenShortWrite2, 2, 0x4e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x4f, 0x05,
    kMipiDsiDtGenShortWrite2, 2, 0x50, 0x07,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x52, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x54, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x14,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x74,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x16,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0xb4,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x16,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x88,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0xbc,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x2c,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xa9, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xac, 0x4d,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0xb3,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x60,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x2c,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x00,
    kMipiDsiDtDcsShortWrite0, 1, 0x11,
    // delay 125ms
    kDsiOpSleep, 125,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x61,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtDcsShortWrite0, 1, 0x29,
    // delay 20ms
    kDsiOpSleep, 20,
    kMipiDsiDtGenShortWrite2, 2, 0x35, 0x00,
    // ending
    kDsiOpSleep, 0xff,
};

// LCD initialization sequence of MTF050FHDI-03 LCD used for Khadas TS050
// touchscreen, dumped from VIM3 bootloader output.
//
// The DCS (display command set) commands contains user command set commands
// and manufacturer command set commands.
//
// User command set is documented in NT35596 Data Sheet (Draft Spec. Version
// 0.05), Novatek [1]. Manufacturer command sets are not documented in any
// publicly available documents.
//
// [1] https://dl.khadas.com/products/add-ons/ts050/nt35596_datasheet_v0.0511.pdf
constexpr uint8_t lcd_init_sequence_MTF050FHDI_03[] = {
    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0x05,
    kMipiDsiDtDcsShortWrite1, 2, 0xfb, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xc5, 0x01,
    // delay 120ms
    kDsiOpDelay, 1, 120,

    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0xee,
    kMipiDsiDtDcsShortWrite1, 2, 0xfb, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x1f, 0x45,
    kMipiDsiDtDcsShortWrite1, 2, 0x24, 0x4f,
    kMipiDsiDtDcsShortWrite1, 2, 0x38, 0xc8,
    kMipiDsiDtDcsShortWrite1, 2, 0x39, 0x27,
    kMipiDsiDtDcsShortWrite1, 2, 0x1e, 0x77,
    kMipiDsiDtDcsShortWrite1, 2, 0x1d, 0x0f,
    kMipiDsiDtDcsShortWrite1, 2, 0x7e, 0x71,
    kMipiDsiDtDcsShortWrite1, 2, 0x7c, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xfb, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x35, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xfb, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x00, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x01, 0x55,
    kMipiDsiDtDcsShortWrite1, 2, 0x02, 0x40,
    kMipiDsiDtDcsShortWrite1, 2, 0x05, 0x40,
    kMipiDsiDtDcsShortWrite1, 2, 0x06, 0x4a,
    kMipiDsiDtDcsShortWrite1, 2, 0x07, 0x24,
    kMipiDsiDtDcsShortWrite1, 2, 0x08, 0x0c,
    kMipiDsiDtDcsShortWrite1, 2, 0x0b, 0x7d,
    kMipiDsiDtDcsShortWrite1, 2, 0x0c, 0x7d,
    kMipiDsiDtDcsShortWrite1, 2, 0x0e, 0xb0,
    kMipiDsiDtDcsShortWrite1, 2, 0x0f, 0xae,
    kMipiDsiDtDcsShortWrite1, 2, 0x11, 0x10,
    kMipiDsiDtDcsShortWrite1, 2, 0x12, 0x10,
    kMipiDsiDtDcsShortWrite1, 2, 0x13, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x14, 0x4a,
    kMipiDsiDtDcsShortWrite1, 2, 0x15, 0x12,
    kMipiDsiDtDcsShortWrite1, 2, 0x16, 0x12,
    kMipiDsiDtDcsShortWrite1, 2, 0x18, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x19, 0x77,
    kMipiDsiDtDcsShortWrite1, 2, 0x1a, 0x55,
    kMipiDsiDtDcsShortWrite1, 2, 0x1b, 0x13,
    kMipiDsiDtDcsShortWrite1, 2, 0x1c, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x1d, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x1e, 0x13,
    kMipiDsiDtDcsShortWrite1, 2, 0x1f, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x23, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x24, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x25, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x26, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x27, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x28, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x35, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x66, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x58, 0x82,
    kMipiDsiDtDcsShortWrite1, 2, 0x59, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x5a, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x5b, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x5c, 0x82,
    kMipiDsiDtDcsShortWrite1, 2, 0x5d, 0x82,
    kMipiDsiDtDcsShortWrite1, 2, 0x5e, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x5f, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x72, 0x31,
    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0x05,
    kMipiDsiDtDcsShortWrite1, 2, 0xfb, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x00, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x01, 0x0b,
    kMipiDsiDtDcsShortWrite1, 2, 0x02, 0x0c,
    kMipiDsiDtDcsShortWrite1, 2, 0x03, 0x09,
    kMipiDsiDtDcsShortWrite1, 2, 0x04, 0x0a,
    kMipiDsiDtDcsShortWrite1, 2, 0x05, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x06, 0x0f,
    kMipiDsiDtDcsShortWrite1, 2, 0x07, 0x10,
    kMipiDsiDtDcsShortWrite1, 2, 0x08, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x09, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x0a, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x0b, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x0c, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x0d, 0x13,
    kMipiDsiDtDcsShortWrite1, 2, 0x0e, 0x15,
    kMipiDsiDtDcsShortWrite1, 2, 0x0f, 0x17,
    kMipiDsiDtDcsShortWrite1, 2, 0x10, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x11, 0x0b,
    kMipiDsiDtDcsShortWrite1, 2, 0x12, 0x0c,
    kMipiDsiDtDcsShortWrite1, 2, 0x13, 0x09,
    kMipiDsiDtDcsShortWrite1, 2, 0x14, 0x0a,
    kMipiDsiDtDcsShortWrite1, 2, 0x15, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x16, 0x0f,
    kMipiDsiDtDcsShortWrite1, 2, 0x17, 0x10,
    kMipiDsiDtDcsShortWrite1, 2, 0x18, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x19, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x1a, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x1b, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x1c, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x1d, 0x13,
    kMipiDsiDtDcsShortWrite1, 2, 0x1e, 0x15,
    kMipiDsiDtDcsShortWrite1, 2, 0x1f, 0x17,
    kMipiDsiDtDcsShortWrite1, 2, 0x20, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x21, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x22, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x23, 0x40,
    kMipiDsiDtDcsShortWrite1, 2, 0x24, 0x40,
    kMipiDsiDtDcsShortWrite1, 2, 0x25, 0xed,
    kMipiDsiDtDcsShortWrite1, 2, 0x29, 0x58,
    kMipiDsiDtDcsShortWrite1, 2, 0x2a, 0x12,
    kMipiDsiDtDcsShortWrite1, 2, 0x2b, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x4b, 0x06,
    kMipiDsiDtDcsShortWrite1, 2, 0x4c, 0x11,
    kMipiDsiDtDcsShortWrite1, 2, 0x4d, 0x20,
    kMipiDsiDtDcsShortWrite1, 2, 0x4e, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x4f, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x50, 0x20,
    kMipiDsiDtDcsShortWrite1, 2, 0x51, 0x61,
    kMipiDsiDtDcsShortWrite1, 2, 0x52, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x53, 0x63,
    kMipiDsiDtDcsShortWrite1, 2, 0x54, 0x77,
    kMipiDsiDtDcsShortWrite1, 2, 0x55, 0xed,
    kMipiDsiDtDcsShortWrite1, 2, 0x5b, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x5c, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x5d, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x5e, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x5f, 0x15,
    kMipiDsiDtDcsShortWrite1, 2, 0x60, 0x75,
    kMipiDsiDtDcsShortWrite1, 2, 0x61, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x62, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x63, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x64, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x65, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x66, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x67, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x68, 0x04,
    kMipiDsiDtDcsShortWrite1, 2, 0x69, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x6a, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x6c, 0x40,
    kMipiDsiDtDcsShortWrite1, 2, 0x75, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x76, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x7a, 0x80,
    kMipiDsiDtDcsShortWrite1, 2, 0x7b, 0xa3,
    kMipiDsiDtDcsShortWrite1, 2, 0x7c, 0xd8,
    kMipiDsiDtDcsShortWrite1, 2, 0x7d, 0x60,
    kMipiDsiDtDcsShortWrite1, 2, 0x7f, 0x15,
    kMipiDsiDtDcsShortWrite1, 2, 0x80, 0x81,
    kMipiDsiDtDcsShortWrite1, 2, 0x83, 0x05,
    kMipiDsiDtDcsShortWrite1, 2, 0x93, 0x08,
    kMipiDsiDtDcsShortWrite1, 2, 0x94, 0x10,
    kMipiDsiDtDcsShortWrite1, 2, 0x8a, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x9b, 0x0f,
    kMipiDsiDtDcsShortWrite1, 2, 0xea, 0xff,
    kMipiDsiDtDcsShortWrite1, 2, 0xec, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xfb, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x75, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x76, 0xdf,
    kMipiDsiDtDcsShortWrite1, 2, 0x77, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x78, 0xe4,
    kMipiDsiDtDcsShortWrite1, 2, 0x79, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x7a, 0xed,
    kMipiDsiDtDcsShortWrite1, 2, 0x7b, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x7c, 0xf6,
    kMipiDsiDtDcsShortWrite1, 2, 0x7d, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x7e, 0xff,
    kMipiDsiDtDcsShortWrite1, 2, 0x7f, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x80, 0x07,
    kMipiDsiDtDcsShortWrite1, 2, 0x81, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x82, 0x10,
    kMipiDsiDtDcsShortWrite1, 2, 0x83, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x84, 0x18,
    kMipiDsiDtDcsShortWrite1, 2, 0x85, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x86, 0x20,
    kMipiDsiDtDcsShortWrite1, 2, 0x87, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x88, 0x3d,
    kMipiDsiDtDcsShortWrite1, 2, 0x89, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x8a, 0x56,
    kMipiDsiDtDcsShortWrite1, 2, 0x8b, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x8c, 0x84,
    kMipiDsiDtDcsShortWrite1, 2, 0x8d, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x8e, 0xab,
    kMipiDsiDtDcsShortWrite1, 2, 0x8f, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x90, 0xec,
    kMipiDsiDtDcsShortWrite1, 2, 0x91, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x92, 0x22,
    kMipiDsiDtDcsShortWrite1, 2, 0x93, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x94, 0x23,
    kMipiDsiDtDcsShortWrite1, 2, 0x95, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x96, 0x55,
    kMipiDsiDtDcsShortWrite1, 2, 0x97, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x98, 0x8b,
    kMipiDsiDtDcsShortWrite1, 2, 0x99, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x9a, 0xaf,
    kMipiDsiDtDcsShortWrite1, 2, 0x9b, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x9c, 0xdf,
    kMipiDsiDtDcsShortWrite1, 2, 0x9d, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x9e, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x9f, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xa0, 0x2c,
    kMipiDsiDtDcsShortWrite1, 2, 0xa2, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xa3, 0x39,
    kMipiDsiDtDcsShortWrite1, 2, 0xa4, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xa5, 0x47,
    kMipiDsiDtDcsShortWrite1, 2, 0xa6, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xa7, 0x56,
    kMipiDsiDtDcsShortWrite1, 2, 0xa9, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xaa, 0x66,
    kMipiDsiDtDcsShortWrite1, 2, 0xab, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xac, 0x76,
    kMipiDsiDtDcsShortWrite1, 2, 0xad, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xae, 0x85,
    kMipiDsiDtDcsShortWrite1, 2, 0xaf, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xb0, 0x90,
    kMipiDsiDtDcsShortWrite1, 2, 0xb1, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xb2, 0xcb,
    kMipiDsiDtDcsShortWrite1, 2, 0xb3, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xb4, 0xdf,
    kMipiDsiDtDcsShortWrite1, 2, 0xb5, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xb6, 0xe4,
    kMipiDsiDtDcsShortWrite1, 2, 0xb7, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xb8, 0xed,
    kMipiDsiDtDcsShortWrite1, 2, 0xb9, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xba, 0xf6,
    kMipiDsiDtDcsShortWrite1, 2, 0xbb, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xbc, 0xff,
    kMipiDsiDtDcsShortWrite1, 2, 0xbd, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xbe, 0x07,
    kMipiDsiDtDcsShortWrite1, 2, 0xbf, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xc0, 0x10,
    kMipiDsiDtDcsShortWrite1, 2, 0xc1, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xc2, 0x18,
    kMipiDsiDtDcsShortWrite1, 2, 0xc3, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xc4, 0x20,
    kMipiDsiDtDcsShortWrite1, 2, 0xc5, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xc6, 0x3d,
    kMipiDsiDtDcsShortWrite1, 2, 0xc7, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xc8, 0x56,
    kMipiDsiDtDcsShortWrite1, 2, 0xc9, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xca, 0x84,
    kMipiDsiDtDcsShortWrite1, 2, 0xcb, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xcc, 0xab,
    kMipiDsiDtDcsShortWrite1, 2, 0xcd, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xce, 0xec,
    kMipiDsiDtDcsShortWrite1, 2, 0xcf, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xd0, 0x22,
    kMipiDsiDtDcsShortWrite1, 2, 0xd1, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xd2, 0x23,
    kMipiDsiDtDcsShortWrite1, 2, 0xd3, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xd4, 0x55,
    kMipiDsiDtDcsShortWrite1, 2, 0xd5, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xd6, 0x8b,
    kMipiDsiDtDcsShortWrite1, 2, 0xd7, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xd8, 0xaf,
    kMipiDsiDtDcsShortWrite1, 2, 0xd9, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xda, 0xdf,
    kMipiDsiDtDcsShortWrite1, 2, 0xdb, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xdc, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xdd, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xde, 0x2c,
    kMipiDsiDtDcsShortWrite1, 2, 0xdf, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xe0, 0x39,
    kMipiDsiDtDcsShortWrite1, 2, 0xe1, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xe2, 0x47,
    kMipiDsiDtDcsShortWrite1, 2, 0xe3, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xe4, 0x56,
    kMipiDsiDtDcsShortWrite1, 2, 0xe5, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xe6, 0x66,
    kMipiDsiDtDcsShortWrite1, 2, 0xe7, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xe8, 0x76,
    kMipiDsiDtDcsShortWrite1, 2, 0xe9, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xea, 0x85,
    kMipiDsiDtDcsShortWrite1, 2, 0xeb, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xec, 0x90,
    kMipiDsiDtDcsShortWrite1, 2, 0xed, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xee, 0xcb,
    kMipiDsiDtDcsShortWrite1, 2, 0xef, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xf0, 0xbb,
    kMipiDsiDtDcsShortWrite1, 2, 0xf1, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xf2, 0xc0,
    kMipiDsiDtDcsShortWrite1, 2, 0xf3, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xf4, 0xcc,
    kMipiDsiDtDcsShortWrite1, 2, 0xf5, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xf6, 0xd6,
    kMipiDsiDtDcsShortWrite1, 2, 0xf7, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xf8, 0xe1,
    kMipiDsiDtDcsShortWrite1, 2, 0xf9, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xfa, 0xea,
    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xfb, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x00, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x01, 0xf4,
    kMipiDsiDtDcsShortWrite1, 2, 0x02, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x03, 0xef,
    kMipiDsiDtDcsShortWrite1, 2, 0x04, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x05, 0x07,
    kMipiDsiDtDcsShortWrite1, 2, 0x06, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x07, 0x28,
    kMipiDsiDtDcsShortWrite1, 2, 0x08, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x09, 0x44,
    kMipiDsiDtDcsShortWrite1, 2, 0x0a, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x0b, 0x76,
    kMipiDsiDtDcsShortWrite1, 2, 0x0c, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x0d, 0xa0,
    kMipiDsiDtDcsShortWrite1, 2, 0x0e, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x0f, 0xe7,
    kMipiDsiDtDcsShortWrite1, 2, 0x10, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x11, 0x1f,
    kMipiDsiDtDcsShortWrite1, 2, 0x12, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x13, 0x22,
    kMipiDsiDtDcsShortWrite1, 2, 0x14, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x15, 0x54,
    kMipiDsiDtDcsShortWrite1, 2, 0x16, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x17, 0x8b,
    kMipiDsiDtDcsShortWrite1, 2, 0x18, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x19, 0xaf,
    kMipiDsiDtDcsShortWrite1, 2, 0x1a, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x1b, 0xe0,
    kMipiDsiDtDcsShortWrite1, 2, 0x1c, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x1d, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x1e, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x1f, 0x2d,
    kMipiDsiDtDcsShortWrite1, 2, 0x20, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x21, 0x39,
    kMipiDsiDtDcsShortWrite1, 2, 0x22, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x23, 0x47,
    kMipiDsiDtDcsShortWrite1, 2, 0x24, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x25, 0x57,
    kMipiDsiDtDcsShortWrite1, 2, 0x26, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x27, 0x65,
    kMipiDsiDtDcsShortWrite1, 2, 0x28, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x29, 0x77,
    kMipiDsiDtDcsShortWrite1, 2, 0x2a, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x2b, 0x85,
    kMipiDsiDtDcsShortWrite1, 2, 0x2d, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x2f, 0x8f,
    kMipiDsiDtDcsShortWrite1, 2, 0x30, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x31, 0xcb,
    kMipiDsiDtDcsShortWrite1, 2, 0x32, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x33, 0xbb,
    kMipiDsiDtDcsShortWrite1, 2, 0x34, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x35, 0xc0,
    kMipiDsiDtDcsShortWrite1, 2, 0x36, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x37, 0xcc,
    kMipiDsiDtDcsShortWrite1, 2, 0x38, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x39, 0xd6,
    kMipiDsiDtDcsShortWrite1, 2, 0x3a, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x3b, 0xe1,
    kMipiDsiDtDcsShortWrite1, 2, 0x3d, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x3f, 0xea,
    kMipiDsiDtDcsShortWrite1, 2, 0x40, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x41, 0xf4,
    kMipiDsiDtDcsShortWrite1, 2, 0x42, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x43, 0xfe,
    kMipiDsiDtDcsShortWrite1, 2, 0x44, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x45, 0x07,
    kMipiDsiDtDcsShortWrite1, 2, 0x46, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x47, 0x28,
    kMipiDsiDtDcsShortWrite1, 2, 0x48, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x49, 0x44,
    kMipiDsiDtDcsShortWrite1, 2, 0x4a, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x4b, 0x76,
    kMipiDsiDtDcsShortWrite1, 2, 0x4c, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x4d, 0xa0,
    kMipiDsiDtDcsShortWrite1, 2, 0x4e, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x4f, 0xe7,
    kMipiDsiDtDcsShortWrite1, 2, 0x50, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x51, 0x1f,
    kMipiDsiDtDcsShortWrite1, 2, 0x52, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x53, 0x22,
    kMipiDsiDtDcsShortWrite1, 2, 0x54, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x55, 0x54,
    kMipiDsiDtDcsShortWrite1, 2, 0x56, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x58, 0x8b,
    kMipiDsiDtDcsShortWrite1, 2, 0x59, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x5a, 0xaf,
    kMipiDsiDtDcsShortWrite1, 2, 0x5b, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x5c, 0xe0,
    kMipiDsiDtDcsShortWrite1, 2, 0x5d, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x5e, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x5f, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x60, 0x2d,
    kMipiDsiDtDcsShortWrite1, 2, 0x61, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x62, 0x39,
    kMipiDsiDtDcsShortWrite1, 2, 0x63, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x64, 0x47,
    kMipiDsiDtDcsShortWrite1, 2, 0x65, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x66, 0x57,
    kMipiDsiDtDcsShortWrite1, 2, 0x67, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x68, 0x65,
    kMipiDsiDtDcsShortWrite1, 2, 0x69, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x6a, 0x77,
    kMipiDsiDtDcsShortWrite1, 2, 0x6b, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x6c, 0x85,
    kMipiDsiDtDcsShortWrite1, 2, 0x6d, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x6e, 0x8f,
    kMipiDsiDtDcsShortWrite1, 2, 0x6f, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x70, 0xcb,
    kMipiDsiDtDcsShortWrite1, 2, 0x71, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x72, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x73, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x74, 0x21,
    kMipiDsiDtDcsShortWrite1, 2, 0x75, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x76, 0x4c,
    kMipiDsiDtDcsShortWrite1, 2, 0x77, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x78, 0x6b,
    kMipiDsiDtDcsShortWrite1, 2, 0x79, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x7a, 0x85,
    kMipiDsiDtDcsShortWrite1, 2, 0x7b, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x7c, 0x9a,
    kMipiDsiDtDcsShortWrite1, 2, 0x7d, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x7e, 0xad,
    kMipiDsiDtDcsShortWrite1, 2, 0x7f, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x80, 0xbe,
    kMipiDsiDtDcsShortWrite1, 2, 0x81, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x82, 0xcd,
    kMipiDsiDtDcsShortWrite1, 2, 0x83, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x84, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x85, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x86, 0x29,
    kMipiDsiDtDcsShortWrite1, 2, 0x87, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x88, 0x68,
    kMipiDsiDtDcsShortWrite1, 2, 0x89, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x8a, 0x98,
    kMipiDsiDtDcsShortWrite1, 2, 0x8b, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0x8c, 0xe5,
    kMipiDsiDtDcsShortWrite1, 2, 0x8d, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x8e, 0x1e,
    kMipiDsiDtDcsShortWrite1, 2, 0x8f, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x90, 0x30,
    kMipiDsiDtDcsShortWrite1, 2, 0x91, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x92, 0x52,
    kMipiDsiDtDcsShortWrite1, 2, 0x93, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x94, 0x88,
    kMipiDsiDtDcsShortWrite1, 2, 0x95, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x96, 0xaa,
    kMipiDsiDtDcsShortWrite1, 2, 0x97, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x98, 0xd7,
    kMipiDsiDtDcsShortWrite1, 2, 0x99, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0x9a, 0xf7,
    kMipiDsiDtDcsShortWrite1, 2, 0x9b, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x9c, 0x21,
    kMipiDsiDtDcsShortWrite1, 2, 0x9d, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0x9e, 0x2e,
    kMipiDsiDtDcsShortWrite1, 2, 0x9f, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xa0, 0x3d,
    kMipiDsiDtDcsShortWrite1, 2, 0xa2, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xa3, 0x4c,
    kMipiDsiDtDcsShortWrite1, 2, 0xa4, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xa5, 0x5e,
    kMipiDsiDtDcsShortWrite1, 2, 0xa6, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xa7, 0x71,
    kMipiDsiDtDcsShortWrite1, 2, 0xa9, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xaa, 0x86,
    kMipiDsiDtDcsShortWrite1, 2, 0xab, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xac, 0x94,
    kMipiDsiDtDcsShortWrite1, 2, 0xad, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xae, 0xfa,
    kMipiDsiDtDcsShortWrite1, 2, 0xaf, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xb0, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xb1, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xb2, 0x21,
    kMipiDsiDtDcsShortWrite1, 2, 0xb3, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xb4, 0x4c,
    kMipiDsiDtDcsShortWrite1, 2, 0xb5, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xb6, 0x6b,
    kMipiDsiDtDcsShortWrite1, 2, 0xb7, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xb8, 0x85,
    kMipiDsiDtDcsShortWrite1, 2, 0xb9, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xba, 0x9a,
    kMipiDsiDtDcsShortWrite1, 2, 0xbb, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xbc, 0xad,
    kMipiDsiDtDcsShortWrite1, 2, 0xbd, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xbe, 0xbe,
    kMipiDsiDtDcsShortWrite1, 2, 0xbf, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xc0, 0xcd,
    kMipiDsiDtDcsShortWrite1, 2, 0xc1, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xc2, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xc3, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xc4, 0x29,
    kMipiDsiDtDcsShortWrite1, 2, 0xc5, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xc6, 0x68,
    kMipiDsiDtDcsShortWrite1, 2, 0xc7, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xc8, 0x98,
    kMipiDsiDtDcsShortWrite1, 2, 0xc9, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xca, 0xe5,
    kMipiDsiDtDcsShortWrite1, 2, 0xcb, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xcc, 0x1e,
    kMipiDsiDtDcsShortWrite1, 2, 0xcd, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xce, 0x20,
    kMipiDsiDtDcsShortWrite1, 2, 0xcf, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xd0, 0x52,
    kMipiDsiDtDcsShortWrite1, 2, 0xd1, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xd2, 0x88,
    kMipiDsiDtDcsShortWrite1, 2, 0xd3, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xd4, 0xaa,
    kMipiDsiDtDcsShortWrite1, 2, 0xd5, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xd6, 0xd7,
    kMipiDsiDtDcsShortWrite1, 2, 0xd7, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xd8, 0xf7,
    kMipiDsiDtDcsShortWrite1, 2, 0xd9, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xda, 0x21,
    kMipiDsiDtDcsShortWrite1, 2, 0xdb, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xdc, 0x2e,
    kMipiDsiDtDcsShortWrite1, 2, 0xdd, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xde, 0x3d,
    kMipiDsiDtDcsShortWrite1, 2, 0xdf, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xe0, 0x4c,
    kMipiDsiDtDcsShortWrite1, 2, 0xe1, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xe2, 0x5e,
    kMipiDsiDtDcsShortWrite1, 2, 0xe3, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xe4, 0x71,
    kMipiDsiDtDcsShortWrite1, 2, 0xe5, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xe6, 0x86,
    kMipiDsiDtDcsShortWrite1, 2, 0xe7, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xe8, 0x94,
    kMipiDsiDtDcsShortWrite1, 2, 0xe9, 0x03,
    kMipiDsiDtDcsShortWrite1, 2, 0xea, 0xfa,
    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xfb, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0x02,
    kMipiDsiDtDcsShortWrite1, 2, 0xfb, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0x04,
    kMipiDsiDtDcsShortWrite1, 2, 0xfb, 0x01,
    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0xd3, 0x0b,
    kMipiDsiDtDcsShortWrite1, 2, 0xd4, 0x04,

    kMipiDsiDtDcsShortWrite0, 1, 0x11,
    // delay 120ms
    kDsiOpDelay, 1, 120,

    kMipiDsiDtDcsShortWrite1, 2, 0xff, 0x00,
    kMipiDsiDtDcsShortWrite1, 2, 0x35, 0x00,
    kMipiDsiDtDcsShortWrite0, 1, 0x29,
    // delay 10ms
    kDsiOpDelay, 1, 10,
    // ending
    kDsiOpSleep, 0,
};

constexpr uint8_t lcd_init_sequence_TV070WSM_FT_NELSON[] = {
    kDsiOpSleep, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpGpio, 3, 0, 0, 10,
    kDsiOpGpio, 3, 0, 1, 30,
    kDsiOpReadReg, 2, 4, 3,

    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe1, 0x93,
    kMipiDsiDtGenShortWrite2, 2, 0xe2, 0x65,
    kMipiDsiDtGenShortWrite2, 2, 0xe3, 0xf8,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x90,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x90,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0xb0,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0xb0,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x3e,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x2f,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x2f,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x0e,
    kMipiDsiDtGenShortWrite2, 2, 0x37, 0x69,
    kMipiDsiDtGenShortWrite2, 2, 0x38, 0x05,
    kMipiDsiDtGenShortWrite2, 2, 0x39, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x3a, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x3c, 0x90,
    kMipiDsiDtGenShortWrite2, 2, 0x3d, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3e, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x3f, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x40, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0x80,
    kMipiDsiDtGenShortWrite2, 2, 0x42, 0x99,
    kMipiDsiDtGenShortWrite2, 2, 0x43, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x44, 0x09,
    kMipiDsiDtGenShortWrite2, 2, 0x45, 0x3c,
    kMipiDsiDtGenShortWrite2, 2, 0x4b, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x0d,
    kMipiDsiDtGenShortWrite2, 2, 0x56, 0x01,
    kMipiDsiDtGenShortWrite2, 2, 0x57, 0x89,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x0a,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x27,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x15,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x7c,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x67,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x58,
    kMipiDsiDtGenShortWrite2, 2, 0x60, 0x4c,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x3c,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x3b,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x36,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x53,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x3f,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x7c,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x67,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x58,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x4c,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x3c,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x24,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x3b,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x38,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x36,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x53,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x3f,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x35,
    kMipiDsiDtGenShortWrite2, 2, 0x7f, 0x2e,
    kMipiDsiDtGenShortWrite2, 2, 0x80, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x81, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x82, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0x00, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x01, 0x45,
    kMipiDsiDtGenShortWrite2, 2, 0x02, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x03, 0x47,
    kMipiDsiDtGenShortWrite2, 2, 0x04, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x05, 0x41,
    kMipiDsiDtGenShortWrite2, 2, 0x06, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x07, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x08, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x0d, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x0f, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x10, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x11, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x12, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x13, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x14, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x15, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x16, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x17, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x18, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x19, 0x46,
    kMipiDsiDtGenShortWrite2, 2, 0x1a, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x1b, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x1c, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1d, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1e, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x1f, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x20, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x21, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x22, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x23, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x24, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x25, 0x1d,
    kMipiDsiDtGenShortWrite2, 2, 0x26, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x27, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x28, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x29, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2a, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x58, 0x40,
    kMipiDsiDtGenShortWrite2, 2, 0x59, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5b, 0x10,
    kMipiDsiDtGenShortWrite2, 2, 0x5c, 0x07,
    kMipiDsiDtGenShortWrite2, 2, 0x5d, 0x20,
    kMipiDsiDtGenShortWrite2, 2, 0x5e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x5f, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x61, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x62, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x63, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x64, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x65, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x66, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x67, 0x32,
    kMipiDsiDtGenShortWrite2, 2, 0x68, 0x08,
    kMipiDsiDtGenShortWrite2, 2, 0x69, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6a, 0x7a,
    kMipiDsiDtGenShortWrite2, 2, 0x6b, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6d, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6e, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x6f, 0x89,
    kMipiDsiDtGenShortWrite2, 2, 0x70, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x71, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x72, 0x06,
    kMipiDsiDtGenShortWrite2, 2, 0x73, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0x74, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x75, 0x07,
    kMipiDsiDtGenShortWrite2, 2, 0x76, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x77, 0x5d,
    kMipiDsiDtGenShortWrite2, 2, 0x78, 0x17,
    kMipiDsiDtGenShortWrite2, 2, 0x79, 0x1f,
    kMipiDsiDtGenShortWrite2, 2, 0x7a, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7b, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7c, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0x7d, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0x7e, 0x7b,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x03,
    kMipiDsiDtGenShortWrite2, 2, 0xaf, 0x20,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x04,
    kMipiDsiDtGenShortWrite2, 2, 0x09, 0x11,
    kMipiDsiDtGenShortWrite2, 2, 0x0e, 0x48,
    kMipiDsiDtGenShortWrite2, 2, 0x2b, 0x2b,
    kMipiDsiDtGenShortWrite2, 2, 0x2e, 0x44,
    kMipiDsiDtGenShortWrite2, 2, 0x41, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0xe0, 0x00,
    kMipiDsiDtGenShortWrite2, 2, 0xe6, 0x02,
    kMipiDsiDtGenShortWrite2, 2, 0xe7, 0x0c,
    kMipiDsiDtGenShortWrite2, 2, 0x51, 0xff,
    kMipiDsiDtGenShortWrite2, 2, 0x53, 0x2c,
    kMipiDsiDtGenShortWrite2, 2, 0x55, 0x00,
    kMipiDsiDtDcsShortWrite0, 1, 0x11,
    kDsiOpSleep, 125,
    kMipiDsiDtDcsShortWrite0, 1, 0x29,
};

// clang-format on

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_INITCODES_INL_H_
