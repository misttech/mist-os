// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gbl_loader.h"

#include <zircon/assert.h>

#include <phys/efi/main.h>

#include "utils.h"

namespace gigaboot {

namespace {

// Gigaboot is designed to print to both `SystemTable->ConOut` and the first found
// `SerialIoProtocol`. However GBL currently only prints to `SystemTable->ConOut`. It remains to be
// investigated if we should port Gigaboot's behavior to GBL. For now we use a workaround that
// overrides `SystemTable->ConOut->OutputString` with Gigaboot's print behavior before booting GBL.

// Stores the original function pointer from `gEfiSystemTable->ConOut->OutputString`
efi_status (*original_conout_output_string)(struct efi_simple_text_output_protocol* self,
                                            char16_t* string) EFIAPI = nullptr;

// Overrides for `gEfiSystemTable->ConOut->OutputString` for GBL.
EFIAPI efi_status ConAndSerialOutputString(struct efi_simple_text_output_protocol* self,
                                           char16_t* string) {
  ZX_ASSERT(original_conout_output_string);
  self->OutputString = original_conout_output_string;
  for (size_t i = 0; string[i]; i++) {
    putchar(string[i]);
  }
  self->OutputString = ConAndSerialOutputString;
  return EFI_SUCCESS;
}
}  // namespace

const cpp20::span<const uint8_t> GetGblEfiApp();

zx::result<> LaunchGbl() {
  const cpp20::span<const uint8_t> app = GetGblEfiApp();

  // Allocates boot buffer and copies over the embedded GBL EFI app blob. This is because
  // `LoadImage()` requires a non-const pointer for the source buffer parameter.
  efi_physical_addr addr;
  efi_status status = gEfiSystemTable->BootServices->AllocatePages(
      AllocateAnyPages, EfiLoaderData, DivideRoundUp(app.size(), kUefiPageSize), &addr);
  if (status != EFI_SUCCESS) {
    printf("Failed to allocate GBL EFI load buffer: %s\n", EfiStatusToString(status));
    return zx::error(static_cast<int>(status));
  }
  memcpy(reinterpret_cast<void*>(addr), app.data(), app.size());

  // Load EFI image.
  efi_handle app_handle = NULL;
  status = gEfiSystemTable->BootServices->LoadImage(
      false, gEfiImageHandle, nullptr, reinterpret_cast<void*>(addr), app.size(), &app_handle);
  if (status != EFI_SUCCESS) {
    printf("Failed to load image: %s\n", EfiStatusToString(status));
    return zx::error(static_cast<int>(status));
  }

  printf("Starting GBL...\n");

  // Overrides `gEfiSystemTable->ConOut->OutputString`
  original_conout_output_string = gEfiSystemTable->ConOut->OutputString;
  gEfiSystemTable->ConOut->OutputString = ConAndSerialOutputString;

  // Starts EFI image.
  size_t exit_data_size = 0;
  status = gEfiSystemTable->BootServices->StartImage(app_handle, &exit_data_size, nullptr);
  gEfiSystemTable->ConOut->OutputString = original_conout_output_string;
  if (status != EFI_SUCCESS) {
    printf("Failed to start image: %s, %zu\n", EfiStatusToString(status), status);
    return zx::error(static_cast<int>(status));
  }

  // Not expected to return if boot succeeds.
  ZX_ASSERT(false);

  return zx::error(1);
}
}  // namespace gigaboot
