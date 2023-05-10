// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_FIRMWARE_GIGABOOT_INCLUDE_SHARED_XEFI_H_
#define SRC_FIRMWARE_GIGABOOT_INCLUDE_SHARED_XEFI_H_

#include <zircon/compiler.h>

#include <efi/boot-services.h>
#include <efi/protocol/device-path.h>
#include <efi/protocol/file.h>
#include <efi/protocol/serial-io.h>
#include <efi/protocol/simple-text-output.h>
#include <efi/system-table.h>
#include <efi/types.h>

__BEGIN_CDECLS

void xefi_init(efi_handle img, efi_system_table* sys);

// Fetches a single character from the console or serial.
//
// Returns whichever interface has an input character ready first. If both
// have characters ready, the console input will be returned.
//
// Console characters are converted from UTF-16 to ASCII. If the character
// can't be represented in ASCII, '\0' is returned instead.
//
// timeout_us: how long to wait in milliseconds. 0 will poll once and
//             return, negative values will wait forever.
//
// Returns the character, or -1 on timeout/error.
int xefi_getc(int64_t timeout_ms);

void xefi_fatal(const char* msg, efi_status status);
char16_t* xefi_handle_to_str(efi_handle handle);
const char* xefi_strerror(efi_status status);
const char16_t* xefi_wstrerror(efi_status status);
size_t strlen_16(char16_t* str);

char16_t* xefi_devpath_to_str(efi_device_path_protocol* path);

typedef struct {
  efi_handle img;
  efi_system_table* sys;
  efi_boot_services* bs;
  efi_simple_text_output_protocol* conout;
  efi_serial_io_protocol* serial;
} xefi_global;

extern xefi_global xefi_global_state;

// Global Context
#define gImg (xefi_global_state.img)
#define gSys (xefi_global_state.sys)
#define gBS (xefi_global_state.bs)
#define gConOut (xefi_global_state.conout)
#define gSerial (xefi_global_state.serial)

__END_CDECLS

#endif  // SRC_FIRMWARE_GIGABOOT_INCLUDE_SHARED_XEFI_H_
