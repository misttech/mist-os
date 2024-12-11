// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gbl_efi_fastboot_protocol.h"

#include <lib/stdcompat/span.h>
#include <zircon/assert.h>

#include <functional>
#include <string_view>

#include <phys/efi/main.h>

#include "utils.h"

#define GBL_EFI_FASTBOOT_PROTOCOL_GUID \
  {0xc67e48a0, 0x5eb8, 0x4127, {0xbe, 0x89, 0xdf, 0x2e, 0xd9, 0x3d, 0x8a, 0x9a}}

namespace {
struct GblEfiFastbootProtocol {
  struct gbl_efi_fastboot_protocol protocol;
};

// Contains information such as variable name and value.
constexpr struct Variable {
  const char* var_name;
  // For now we only consider constant variable.
  const char* var_impl;

  /// Gets the name as a string_view.
  std::string_view name() const { return std::string_view(var_name); }

  /// Gets the value as a string_view.
  std::string_view impl() const { return std::string_view(var_impl); }
} kVariables[] = {
    {"hw-revision", BOARD_NAME},
};

/// Gets the list of variables
cpp20::span<const Variable> variables() { return cpp20::span<const Variable>(kVariables); }

EFIAPI efi_status GetVar(struct gbl_efi_fastboot_protocol* self, const char* const* args,
                         size_t num_args, uint8_t* buf, size_t* bufsize) {
  const cpp20::span<const char* const> args_span{args, num_args};
  if (args_span.empty() || !bufsize) {
    return EFI_INVALID_PARAMETER;
  }

  cpp20::span<uint8_t> out{buf, *bufsize};
  for (size_t i = 0; i < variables().size(); i++) {
    const Variable& var = variables()[i];
    if (std::string_view(args_span[0]) != var.name()) {
      continue;
    }

    if (out.size() < var.impl().size() + 1) {
      return EFI_BUFFER_TOO_SMALL;
    }
    memcpy(out.data(), var.impl().data(), var.impl().size());
    *bufsize = var.impl().size();
    out.data()[*bufsize] = 0;
    return EFI_SUCCESS;
  }
  return EFI_NOT_FOUND;
}

EFIAPI efi_status GetVarAll(struct gbl_efi_fastboot_protocol* self, void* ctx,
                            get_var_callback cb) {
  for (size_t i = 0; i < variables().size(); i++) {
    std::array args{variables()[i].name().data()};
    cb(ctx, args.data(), args.size(), variables()[i].impl().data());
  }
  return EFI_SUCCESS;
}

EFIAPI efi_status RunOemFunction(struct gbl_efi_fastboot_protocol* self, const uint8_t* command,
                                 size_t command_len, uint8_t* buf, size_t* bufsize) {
  return EFI_UNSUPPORTED;
}

EFIAPI efi_status GetPolicy(struct gbl_efi_fastboot_protocol* self,
                            gbl_efi_fastboot_policy* policy) {
  return EFI_UNSUPPORTED;
}

EFIAPI efi_status SetLock(struct gbl_efi_fastboot_protocol* self, uint64_t lock_state) {
  return EFI_UNSUPPORTED;
}

EFIAPI efi_status ClearLock(struct gbl_efi_fastboot_protocol* self, uint64_t lock_state) {
  return EFI_UNSUPPORTED;
}

EFIAPI efi_status GetPartitionPermissions(struct gbl_efi_fastboot_protocol* self,
                                          const uint8_t* part_name, size_t part_name_len,
                                          uint64_t* permissions) {
  return EFI_UNSUPPORTED;
}

EFIAPI efi_status WipeUserData(struct gbl_efi_fastboot_protocol* self) { return EFI_UNSUPPORTED; }

GblEfiFastbootProtocol protocol = {
    .protocol = {.version = 0x01,
                 .get_var = GetVar,
                 .get_var_all = GetVarAll,
                 .run_oem_function = RunOemFunction,
                 .get_policy = GetPolicy,
                 .set_lock = SetLock,
                 .clear_lock = ClearLock,
                 .get_partition_permissions = GetPartitionPermissions,
                 .wipe_user_data = WipeUserData}};

efi_guid guid = GBL_EFI_FASTBOOT_PROTOCOL_GUID;

}  // namespace

namespace gigaboot {
efi_status InstallGblEfiFastbootProtocol() {
  efi_handle out_handle = NULL;
  return gEfiSystemTable->BootServices->InstallMultipleProtocolInterfaces(&out_handle, &guid,
                                                                          &protocol.protocol, NULL);
}
}  // namespace gigaboot
