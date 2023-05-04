// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_FIRMWARE_GIGABOOT_SRC_OSBOOT_H_
#define SRC_FIRMWARE_GIGABOOT_SRC_OSBOOT_H_

#include <stdbool.h>
#include <stdint.h>
#include <zircon/compiler.h>

#include <efi/protocol/graphics-output.h>
#include <efi/system-table.h>

__BEGIN_CDECLS

#define PAGE_SIZE (4096)
#define PAGE_MASK (PAGE_SIZE - 1)

#define BYTES_TO_PAGES(n) (((n) + PAGE_MASK) / PAGE_SIZE)
#define ROUNDUP(size, align) (((size) + ((align)-1)) & ~((align)-1))

#define CMDLINE_MAX PAGE_SIZE

// Space for extra ZBI items.
#define EXTRA_ZBI_ITEM_SPACE (8 * PAGE_SIZE)
uint64_t find_acpi_root(efi_handle img, efi_system_table* sys);
uint64_t find_smbios(efi_handle img, efi_system_table* sys);

uint32_t get_zx_pixel_format(efi_graphics_output_protocol* gop);

int boot_deprecated(efi_handle img, efi_system_table* sys, void* image, size_t sz, void* ramdisk,
                    size_t rsz, void* cmdline, size_t csz);

int zbi_boot(efi_handle img, efi_system_table* sys, void* image, size_t sz);

bool image_is_valid(void* image, size_t sz);

// sz may be just one block or sector
// if the header looks like a kernel image, return expected size
// otherwise returns 0
size_t image_getsize(void* imageheader, size_t sz);

// Where to start the kernel from
extern size_t kernel_zone_size;
extern efi_physical_addr kernel_zone_base;

// Selection for how to boot the device.
typedef enum {
  kBootActionDefault,
  kBootActionFastboot,
  kBootActionNetboot,
  kBootActionSlotA,
  kBootActionSlotB,
  kBootActionSlotR,
} BootAction;

// Determines what boot action to take.
//
// Priority order goes:
//   1. use the bootbyte if set (e.g. via `dm reboot-bootloader`)
//   2. let the user select from a boot menu
//   3. use "bootloader.default" commandline arg
//
// Args:
//   have_network: true if we have a working network interface.
//   have_fb: true if we have a framebuffer.
//
// Returns the chosen boot action.
BootAction get_boot_action(efi_runtime_services* runtime, bool have_network, bool have_fb);

// 'printf' signature for output testing.
typedef int (*print_function)(const char* fmt, ...);

// Print buffer as hex string.
void uefi_var_print_hex(const uint8_t* buf, size_t size, print_function printer);

// Print buf as UTF8 string.
void uefi_var_print_str(const uint8_t* buf, size_t size, print_function printer);

// Get UCS-2 string length.
//
// str      : UCS-2 string, not necessarily null-terminated.
// buf_size : str buffer size in bytes.
size_t ucs2_len(const char16_t* str, size_t buf_size);

// Convert UCS-2 string to UTF8 and print it to printer.
//
// variable_name          : UCS-2 null-terminated string
// variable_name_buf_size : variable_name buffer size, can be bigger than actual string. If there is
//                          no nullptr termination this will be considered input string length.
// printer                : printf() style function for outputting result c-string
//
// Returns 0 on success, and -1 on error;
int print_ucs2(const char16_t* variable_name, size_t variable_name_buf_size,
               print_function printer);

// Formatted print of buffer
void uefi_print_var(const uint8_t* buf, size_t size, print_function printer);

// Print UEFI variable information
void dump_var_info(print_function printer);

// Dump verbosity
typedef enum {
  kEfiVarDumpVerbosityFull,
  kEfiVarDumpVerbosityShort,
  kEfiVarDumpVerbosityNameOnly,
} EfiVarDumpVerbosity;

// Print all UEFI variables
void dump_uefi_vars_custom_printer(print_function printer, EfiVarDumpVerbosity verbosity);

__END_CDECLS

#endif  // SRC_FIRMWARE_GIGABOOT_SRC_OSBOOT_H_
