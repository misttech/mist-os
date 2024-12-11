// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// The protocol definition is taken from
// https://android.googlesource.com/platform/bootable/libbootloader/+/refs/heads/main/gbl/libefi_types/defs/protocols/gbl_efi_fastboot_protocol.h

#ifndef SRC_FIRMWARE_GIGABOOT_CPP_GBL_EFI_FASTBOOT_PROTOCOL_H_
#define SRC_FIRMWARE_GIGABOOT_CPP_GBL_EFI_FASTBOOT_PROTOCOL_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <zircon/compiler.h>

#include <efi/types.h>

__BEGIN_CDECLS

#define GBL_EFI_FASTBOOT_SERIAL_NUMBER_MAX_LEN_UTF8 32

typedef struct gbl_efi_fastboot_policy {
  // Indicates whether device can be unlocked
  bool can_unlock;
  // Device firmware supports 'critical' partition locking
  bool has_critical_lock;
  // Indicates whether device allows booting from image loaded directly from
  // RAM.
  bool can_ram_boot;
} gbl_efi_fastboot_policy;

enum GBL_EFI_FASTBOOT_PARTITION_PERMISSION_FLAGS {
  // Firmware can read the given partition and send its data to fastboot client.
  GBL_EFI_FASTBOOT_PARTITION_READ = 0x1 << 0,
  // Firmware can overwrite the given partition.
  GBL_EFI_FASTBOOT_PARTITION_WRITE = 0x1 << 1,
  // Firmware can erase the given partition.
  GBL_EFI_FASTBOOT_PARTITION_ERASE = 0x1 << 2,
};

enum GBL_EFI_FASTBOOT_LOCK_FLAGS {
  // All device partitions are locked.
  GBL_EFI_FASTBOOT_GBL_EFI_LOCKED = 0x1 << 0,
  // All 'critical' device partitions are locked.
  GBL_EFI_FASTBOOT_GBL_EFI_CRITICAL_LOCKED = 0x1 << 1,
};

typedef void (*get_var_callback)(void* context, const char* const* args, size_t num_args,
                                 const char* val);

typedef struct gbl_efi_fastboot_protocol {
  // Revision of the protocol supported.
  uint32_t version;
  // Null-terminated UTF-8 encoded string
  uint8_t serial_number[GBL_EFI_FASTBOOT_SERIAL_NUMBER_MAX_LEN_UTF8];

  // Fastboot variable methods
  efi_status (*get_var)(struct gbl_efi_fastboot_protocol* self, const char* const* args,
                        size_t num_args, uint8_t* out, size_t* out_size) EFIAPI;

  efi_status (*get_var_all)(struct gbl_efi_fastboot_protocol* self, void* ctx,
                            get_var_callback cb) EFIAPI;

  // Fastboot oem function methods
  efi_status (*run_oem_function)(struct gbl_efi_fastboot_protocol* self, const uint8_t* command,
                                 size_t command_len, uint8_t* buf, size_t* bufsize) EFIAPI;

  // Device lock methods
  efi_status (*get_policy)(struct gbl_efi_fastboot_protocol* self,
                           gbl_efi_fastboot_policy* policy) EFIAPI;
  efi_status (*set_lock)(struct gbl_efi_fastboot_protocol* self, uint64_t lock_state) EFIAPI;
  efi_status (*clear_lock)(struct gbl_efi_fastboot_protocol* self, uint64_t lock_state) EFIAPI;

  // Misc methods
  efi_status (*get_partition_permissions)(struct gbl_efi_fastboot_protocol* self,
                                          const uint8_t* part_name, size_t part_name_len,
                                          uint64_t* permissions) EFIAPI;
  efi_status (*wipe_user_data)(struct gbl_efi_fastboot_protocol* self) EFIAPI;
} gbl_efi_fastboot_protocol;

__END_CDECLS

#endif  // SRC_FIRMWARE_GIGABOOT_CPP_GBL_EFI_FASTBOOT_PROTOCOL_H_
