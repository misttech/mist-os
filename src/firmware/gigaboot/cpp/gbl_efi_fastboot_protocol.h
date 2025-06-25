// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of self source code is governed by a BSD-style license that can be
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

// Callback function pointer passed to gbl_efi_fastboot_protocol.get_var_all.
//
// context: Caller specific context.
// args: An array of NULL-terminated strings that contains the variable name
//       followed by additional arguments if any.
// val: A NULL-terminated string representing the value.
typedef void (*get_var_callback)(void* context, const char* const* args, size_t num_args,
                                 const char* val);

typedef enum EFI_FASTBOOT_MESSAGE_TYPE {
  OKAY,
  FAIL,
  INFO,
} EfiFastbootMessageType;

typedef efi_status (*fastboot_message_sender)(void* context, EfiFastbootMessageType msg_type,
                                              const char* msg, size_t msg_len);

typedef enum GBL_EFI_FASTBOOT_PARTITION_PERMISSION_FLAGS {
  // Firmware can read the given partition and send its data to fastboot client.
  GBL_EFI_FASTBOOT_PARTITION_READ = 0x1 << 0,
  // Firmware can overwrite the given partition.
  GBL_EFI_FASTBOOT_PARTITION_WRITE = 0x1 << 1,
  // Firmware can erase the given partition.
  GBL_EFI_FASTBOOT_PARTITION_ERASE = 0x1 << 2,
} GblEfiFastbootPartitionPermissionFlags;

typedef enum GBL_EFI_FASTBOOT_LOCK_FLAGS {
  // All device partitions are locked.
  GBL_EFI_FASTBOOT_GBL_EFI_LOCKED = 0x1 << 0,
  // All 'critical' device partitions are locked.
  GBL_EFI_FASTBOOT_GBL_EFI_CRITICAL_LOCKED = 0x1 << 1,
} GblEfiFastbootLockFlags;

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
  efi_status (*run_oem_function)(struct gbl_efi_fastboot_protocol* self, const char* cmd,
                                 size_t len, uint8_t* download_buffer, size_t download_data_size,
                                 fastboot_message_sender sender, void* ctx) EFIAPI;

  // Fastboot get_staged backend
  efi_status (*get_staged)(struct gbl_efi_fastboot_protocol* self, uint8_t* out, size_t* out_size,
                           size_t* out_remain) EFIAPI;

  // Device lock methods
  efi_status (*get_policy)(struct gbl_efi_fastboot_protocol* self,
                           gbl_efi_fastboot_policy* policy) EFIAPI;
  efi_status (*set_lock)(struct gbl_efi_fastboot_protocol* self, uint64_t lock_state) EFIAPI;
  efi_status (*clear_lock)(struct gbl_efi_fastboot_protocol* self, uint64_t lock_state) EFIAPI;

  // Local session methods
  efi_status (*start_local_session)(struct gbl_efi_fastboot_protocol* self, void** ctx) EFIAPI;
  efi_status (*update_local_session)(struct gbl_efi_fastboot_protocol* self, void* ctx,
                                     uint8_t* buf, size_t* buf_size) EFIAPI;
  efi_status (*close_local_session)(struct gbl_efi_fastboot_protocol* self, void* ctx) EFIAPI;

  // Misc methods
  efi_status (*get_partition_permissions)(struct gbl_efi_fastboot_protocol* self,
                                          const uint8_t* part_name, size_t part_name_len,
                                          uint64_t* permissions) EFIAPI;
  efi_status (*wipe_user_data)(struct gbl_efi_fastboot_protocol* self) EFIAPI;
  bool (*should_stop_in_fastboot)(struct gbl_efi_fastboot_protocol* self) EFIAPI;
} gbl_efi_fastboot_protocol;

extern bool g_should_stop_in_fastboot;

__END_CDECLS

#endif  // SRC_FIRMWARE_GIGABOOT_CPP_GBL_EFI_FASTBOOT_PROTOCOL_H_
