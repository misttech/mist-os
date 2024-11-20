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

// Maxmimum string length for variable name and value.
constexpr size_t kMaxVarStringLength = 256;

// Contains information such as variable name and value.
constexpr struct Variable {
  const char var_name[kMaxVarStringLength];
  // For now we only consider constant variable.
  const char var_impl[kMaxVarStringLength];

  /// Gets the name as a string_view.
  std::string_view name() const { return std::string_view(var_name); }

  /// Gets the value as a string_view.
  std::string_view impl() const { return std::string_view(var_impl); }
} kVariables[] = {
    {{"hw-revision\0"}, {BOARD_NAME "\0"}},
};

/// Gets the list of variables
cpp20::span<const Variable> variables() { return cpp20::span<const Variable>(kVariables); }

// Converts `gbl_efi_fastboot_arg` to `std::string_view`.
std::string_view FbArgToStr(const gbl_efi_fastboot_arg* arg) {
  return std::string_view(reinterpret_cast<const char*>(arg->str_utf8), arg->len);
}

EFIAPI efi_status GetVar(struct gbl_efi_fastboot_protocol* self, const gbl_efi_fastboot_arg* args,
                         size_t num_args, uint8_t* buf, size_t* bufsize,
                         gbl_efi_fastboot_token hint) {
  cpp20::span<const gbl_efi_fastboot_arg> args_span{args, num_args};
  if (args_span.empty() || !bufsize) {
    return EFI_INVALID_PARAMETER;
  }

  cpp20::span<uint8_t> out{buf, *bufsize};
  // For this implementation, `hint` is interpreted as index for `variables()`.
  size_t start = reinterpret_cast<size_t>(hint);
  for (size_t i = 0; i < variables().size(); i++) {
    size_t idx = (start + i) % variables().size();
    const Variable& var = variables()[idx];
    if (FbArgToStr(&args_span[0]) != var.name()) {
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

EFIAPI efi_status StartVarIterator(struct gbl_efi_fastboot_protocol* self,
                                   gbl_efi_fastboot_token* token) {
  *reinterpret_cast<size_t*>(token) = 0;
  return EFI_SUCCESS;
}

EFIAPI efi_status GetNextVarArgs(struct gbl_efi_fastboot_protocol* self, gbl_efi_fastboot_arg* args,
                                 size_t* num_args, gbl_efi_fastboot_token* token) {
  size_t* idx = reinterpret_cast<size_t*>(token);
  if (*idx > variables().size() || *num_args == 0) {
    return EFI_INVALID_PARAMETER;
  } else if (*idx == variables().size()) {
    *num_args = 0;
    return EFI_SUCCESS;
  }

  cpp20::span<gbl_efi_fastboot_arg> args_span{args, *num_args};
  args_span[0].str_utf8 = reinterpret_cast<const uint8_t*>(variables()[*idx].name().data());
  args_span[0].len = variables()[*idx].name().size();
  *num_args = 1;
  (*idx)++;
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
                 .start_var_iterator = StartVarIterator,
                 .get_next_var_args = GetNextVarArgs,
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
