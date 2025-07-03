// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2009 Corey Tabaka
// Copyright (c) 2016 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "platform/pc/keyboard.h"

#include <debug.h>
#include <stdint.h>

#include <arch/x86.h>

// Remnants of a full i8042 keyboard driver, now just used to reboot the system as a fallback.

namespace {

/* i8042 keyboard controller registers */
constexpr uint16_t I8042_COMMAND_REG = 0x64;
constexpr uint16_t I8042_STATUS_REG = 0x64;

inline uint8_t i8042_read_status() { return inp(I8042_STATUS_REG); }
inline void i8042_write_command(uint8_t val) { outp(I8042_COMMAND_REG, val); }

/*

 * timeout in milliseconds
 */
#define I8042_CTL_TIMEOUT 500

/*
 * status register bits
 */
#define I8042_STR_PARITY 0x80
#define I8042_STR_TIMEOUT 0x40
#define I8042_STR_AUXDATA 0x20
#define I8042_STR_KEYLOCK 0x10
#define I8042_STR_CMDDAT 0x08
#define I8042_STR_MUXERR 0x04
#define I8042_STR_IBF 0x02
#define I8042_STR_OBF 0x01

/*
 * control register bits
 */
#define I8042_CTR_KBDINT 0x01
#define I8042_CTR_AUXINT 0x02
#define I8042_CTR_IGNKEYLK 0x08
#define I8042_CTR_KBDDIS 0x10
#define I8042_CTR_AUXDIS 0x20
#define I8042_CTR_XLATE 0x40

/*
 * commands
 */
#define I8042_CMD_CTL_RCTR 0x0120
#define I8042_CMD_CTL_WCTR 0x1060
#define I8042_CMD_CTL_TEST 0x01aa

#define I8042_CMD_KBD_DIS 0x00ad
#define I8042_CMD_KBD_EN 0x00ae
#define I8042_CMD_PULSE_RESET 0x00fe
#define I8042_CMD_KBD_TEST 0x01ab
#define I8042_CMD_KBD_MODE 0x01f0

int i8042_wait_write() {
  int i = 0;
  while ((i8042_read_status() & I8042_STR_IBF) && (i < I8042_CTL_TIMEOUT)) {
    spin(10);
    i++;
  }
  return -(i == I8042_CTL_TIMEOUT);
}

}  // namespace

void pc_keyboard_reboot() {
  if (i8042_wait_write() != 0) {
    return;
  }

  i8042_write_command(I8042_CMD_PULSE_RESET);
  // Wait a second for the command to process before declaring failure
  spin(1000000);
}
