/* Copyright 2021 The Fuchsia Authors
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 */

/*
 * See linuxboot.ld for more details.  This file only sets the symbols
 * that differ between the 32-bit and pure 64-bit versions.
 */

/*
 * We use only "bzImage" protocol, which loads the 32-bit code at fixed 1MiB.
 */
HIDDEN(LINUXBOOT_LOAD_ADDRESS = 0x100000);

/*
 * This _start symbol just sets e_entry in ELF headers (as per phys.ld), which
 * don't exist at runtime.  Set it to match the 32-bit protocol's entry point.
 * There is no global symbol for the 16-bit entry point (exactly 512 bytes into
 * the .boot.header region) because the linuxboot-header.S code is not actually
 * part of the same memory image as the rest of the program.
 */
HIDDEN(_start = Phys32Entry);
