// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "boot_zbi_items.h"

#include <lib/stdcompat/span.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbi/zbi.h>
#include <lib/zircon_boot/zbi_utils.h>
#include <stdio.h>
#include <stdlib.h>
#include <zircon/limits.h>

#include <algorithm>
#include <numeric>

#include <efi/boot-services.h>
#include <efi/protocol/graphics-output.h>
#include <efi/system-table.h>
#include <efi/types.h>
#include <fbl/vector.h>

#include "acpi.h"
#include "phys/efi/main.h"
#include "utils.h"

namespace gigaboot {

const efi_guid kAcpiTableGuid = ACPI_TABLE_GUID;
const efi_guid kAcpi20TableGuid = ACPI_20_TABLE_GUID;
const uint8_t kAcpiRsdpSignature[8] = {'R', 'S', 'D', ' ', 'P', 'T', 'R', ' '};

const efi_guid kSmbiosTableGUID = SMBIOS_TABLE_GUID;
const efi_guid kSmbios3TableGUID = SMBIOS3_TABLE_GUID;
const uint8_t kSmbiosAnchor[4] = {'_', 'S', 'M', '_'};
const uint8_t kSmbios3Anchor[5] = {'_', 'S', 'M', '3', '_'};

extern "C" efi_status generate_efi_memory_attributes_table_item(
    void* ramdisk, const size_t ramdisk_size, efi_system_table* sys, const void* mmap,
    size_t memory_map_size, size_t dsize);

namespace {

uint8_t scratch_buffer[32 * 1024];

bool AppendSmbiosPtr(zbi_header_t* image, size_t capacity) {
  uint64_t smbios = 0;
  cpp20::span<const efi_configuration_table> entries(gEfiSystemTable->ConfigurationTable,
                                                     gEfiSystemTable->NumberOfTableEntries);

  for (const efi_configuration_table& entry : entries) {
    if ((entry.VendorGuid == kSmbiosTableGUID &&
         !memcmp(entry.VendorTable, kSmbiosAnchor, sizeof(kSmbiosAnchor))) ||
        (entry.VendorGuid == kSmbios3TableGUID &&
         !memcmp(entry.VendorTable, kSmbios3Anchor, sizeof(kSmbios3Anchor)))) {
      smbios = reinterpret_cast<uint64_t>(entry.VendorTable);
      break;
    }
  }

  if (smbios == 0) {
    return false;
  }

  return zbi_create_entry_with_payload(image, capacity, ZBI_TYPE_SMBIOS, 0, 0, &smbios,
                                       sizeof(smbios)) == ZBI_RESULT_OK;
}

bool AppendAcpiRsdp(zbi_header_t* image, size_t capacity) {
  acpi_rsdp_t const* rsdp = nullptr;
  cpp20::span<const efi_configuration_table> entries(gEfiSystemTable->ConfigurationTable,
                                                     gEfiSystemTable->NumberOfTableEntries);

  for (const efi_configuration_table& entry : entries) {
    // Check if this entry is an ACPI RSD PTR.
    if ((entry.VendorGuid == kAcpiTableGuid || entry.VendorGuid == kAcpi20TableGuid) &&
        // Verify the signature of the ACPI RSD PTR.
        !memcmp(entry.VendorTable, kAcpiRsdpSignature, sizeof(kAcpiRsdpSignature))) {
      rsdp = reinterpret_cast<const acpi_rsdp_t*>(entry.VendorTable);
      break;
    }
  }

  // Verify an ACPI table was found.
  if (rsdp == nullptr) {
    printf("RSDP was not found\n");
    return false;
  }

  // Verify the checksum of this table. Both V1 and V2 RSDPs should pass the
  // V1 checksum, which only covers the first 20 bytes of the table.
  // The checksum adds all the bytes and passes if the result is 0.
  cpp20::span<const uint8_t> acpi_bytes(reinterpret_cast<const uint8_t*>(rsdp), kAcpiRsdpV1Size);
  if (std::accumulate(acpi_bytes.begin(), acpi_bytes.end(), uint8_t{0})) {
    printf("RSDP V1 checksum failed\n");
    return false;
  }

  // V2 RSDPs should additionally pass a checksum of the entire table.
  if (rsdp->revision > 0) {
    acpi_bytes = {acpi_bytes.begin(), rsdp->length};
    if (std::accumulate(acpi_bytes.begin(), acpi_bytes.end(), uint8_t{0})) {
      printf("RSDP V2 checksum failed\n");
      return false;
    }
  }

  if (zbi_result_t result = zbi_create_entry_with_payload(image, capacity, ZBI_TYPE_ACPI_RSDP, 0, 0,
                                                          &rsdp, sizeof(rsdp));
      result != ZBI_RESULT_OK) {
    printf("Failed to create ACPI rsdp entry, %d\n", result);
    return false;
  }

  return true;
}

zbi_pixel_format_t PixelFormatFromBitmask(const efi_pixel_bitmask& bitmask) {
  struct entry {
    efi_pixel_bitmask mask;
    zbi_pixel_format_t pixel_format;
  };
  // Ignore reserved field
  constexpr entry entries[] = {
      {.mask = {.RedMask = 0xFF0000, .GreenMask = 0xFF00, .BlueMask = 0xFF},
       .pixel_format = ZBI_PIXEL_FORMAT_RGB_X888},
      {.mask = {.RedMask = 0xE0, .GreenMask = 0x1C, .BlueMask = 0x3},
       .pixel_format = ZBI_PIXEL_FORMAT_RGB_332},
      {.mask = {.RedMask = 0xF800, .GreenMask = 0x7E0, .BlueMask = 0x1F},
       .pixel_format = ZBI_PIXEL_FORMAT_RGB_565},
      {.mask = {.RedMask = 0xC0, .GreenMask = 0x30, .BlueMask = 0xC},
       .pixel_format = ZBI_PIXEL_FORMAT_RGB_2220},
  };

  auto equal_p = [&bitmask](const entry& e) -> bool {
    // Ignore reserved
    return bitmask.RedMask == e.mask.RedMask && bitmask.GreenMask == e.mask.GreenMask &&
           bitmask.BlueMask == e.mask.BlueMask;
  };

  auto res = std::find_if(std::begin(entries), std::end(entries), equal_p);
  if (res == std::end(entries)) {
    printf("unsupported pixel format bitmask: r %08x / g %08x / b %08x\n", bitmask.RedMask,
           bitmask.GreenMask, bitmask.BlueMask);
    return ZBI_PIXEL_FORMAT_NONE;
  }

  return res->pixel_format;
}

uint32_t GetZbiPixelFormat(efi_graphics_output_mode_information* info) {
  efi_graphics_pixel_format efi_fmt = info->PixelFormat;
  switch (efi_fmt) {
    case PixelBlueGreenRedReserved8BitPerColor:
      return ZBI_PIXEL_FORMAT_RGB_X888;
    case PixelBitMask:
      return PixelFormatFromBitmask(info->PixelInformation);
    default:
      printf("unsupported pixel format %d!\n", efi_fmt);
      return ZBI_PIXEL_FORMAT_NONE;
  }
}

// If the firmware supports graphics, append
// framebuffer information to the list of zbi items.
//
// Returns true if there is no graphics support or if framebuffer information
// was successfully added, and false if appending framebuffer information
// returned an error.
bool AddFramebufferIfSupported(zbi_header_t* image, size_t capacity) {
  auto graphics_protocol = gigaboot::EfiLocateProtocol<efi_graphics_output_protocol>();
  if (graphics_protocol.is_error() || graphics_protocol.value() == nullptr) {
    // Graphics are not strictly necessary.
    printf("No valid graphics output detected\n");
    return true;
  }

  zbi_swfb_t framebuffer = {
      .base = graphics_protocol.value()->Mode->FrameBufferBase,
      .width = graphics_protocol.value()->Mode->Info->HorizontalResolution,
      .height = graphics_protocol.value()->Mode->Info->VerticalResolution,
      .stride = graphics_protocol.value()->Mode->Info->PixelsPerScanLine,
      .format = GetZbiPixelFormat(graphics_protocol.value()->Mode->Info),
  };
  if (zbi_result_t result = zbi_create_entry_with_payload(image, capacity, ZBI_TYPE_FRAMEBUFFER, 0,
                                                          0, &framebuffer, sizeof(framebuffer));
      result != ZBI_RESULT_OK) {
    printf("Failed to add framebuffer zbi item: %d\n", result);
    return false;
  }

  return true;
}

bool AddSystemTable(zbi_header_t* image, size_t capacity) {
  return zbi_create_entry_with_payload(image, capacity, ZBI_TYPE_EFI_SYSTEM_TABLE, 0, 0,
                                       &gEfiSystemTable, sizeof(gEfiSystemTable)) == ZBI_RESULT_OK;
}

// Bootloader file item for ssh key provisioning
// TODO(b/239088231): Consider using dynamic allocation.
constexpr size_t kZbiFileLength = 4096;
bool zbi_file_is_initialized = false;
uint8_t zbi_files[kZbiFileLength] __attribute__((aligned(ZBI_ALIGNMENT)));

}  // namespace

zbi_result_t AddBootloaderFiles(const char* name, const void* data, size_t len) {
  if (!zbi_file_is_initialized) {
    zbi_result_t result = zbi_init(zbi_files, kZbiFileLength);
    if (result != ZBI_RESULT_OK) {
      printf("Failed to initialize zbi_files: %d\n", result);
      return result;
    }
    zbi_file_is_initialized = true;
  }

  return AppendZbiFile(reinterpret_cast<zbi_header_t*>(zbi_files), kZbiFileLength, name, data, len);
}

void ClearBootloaderFiles() { zbi_file_is_initialized = false; }

cpp20::span<uint8_t> GetZbiFiles() { return zbi_files; }

// Add memory related zbi items.
//
// Returns memory map key on success, which will be used for ExitBootService.
zx::result<size_t> AddMemoryItems(void* zbi, size_t capacity) {
  uint32_t dversion = 0;
  size_t mkey = 0;
  size_t dsize = 0;
  size_t msize = sizeof(scratch_buffer);
  // Note: Once memory map is grabbed, do not do anything that can change it, i.e. anything that
  // involves memory allocation/de-allocation, including printf if it is printing to graphics.
  efi_status status = gEfiSystemTable->BootServices->GetMemoryMap(
      &msize, reinterpret_cast<efi_memory_descriptor*>(scratch_buffer), &mkey, &dsize, &dversion);
  if (status != EFI_SUCCESS) {
    printf("boot: cannot GetMemoryMap(). %s\n", EfiStatusToString(status));
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Look for an EFI memory attributes table we can pass to the kernel.
  efi_status mem_attr_res = generate_efi_memory_attributes_table_item(
      zbi, capacity, gEfiSystemTable, scratch_buffer, msize, dsize);
  if (mem_attr_res != EFI_SUCCESS) {
    printf("failed to generate EFI memory attributes table: %s", EfiStatusToString(mem_attr_res));
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Convert the memory map in place to a range of zbi_mem_range_t, the
  // preferred ZBI memory format. In-place conversion can safely be done
  // one-by-one, given that zbi_mem_range_t is smaller than a descriptor.
  static_assert(sizeof(zbi_mem_range_t) <= sizeof(efi_memory_descriptor),
                "Cannot assume that sizeof(zbi_mem_range_t) <= dsize");
  size_t num_ranges = msize / dsize;
  zbi_mem_range_t* ranges = reinterpret_cast<zbi_mem_range_t*>(scratch_buffer);
  for (size_t i = 0; i < num_ranges; ++i) {
    const efi_memory_descriptor* desc =
        reinterpret_cast<efi_memory_descriptor*>(scratch_buffer + i * dsize);
    const zbi_mem_range_t range = {
        .paddr = desc->PhysicalStart,
        .length = desc->NumberOfPages * kUefiPageSize,
        .type = EfiToZbiMemRangeType(desc->Type),
    };
    memcpy(&ranges[i], &range, sizeof(range));
  }

  // TODO(b/236039205): Add memory ranges for uart peripheral. Refer to
  // https://cs.opensource.google/fuchsia/fuchsia/+/main:src/firmware/gigaboot/src/zircon.c;line=449;drc=de7e29bf0d7189a61b691ef0cdd0fd5ae1dfd605
  // `src/firmware/gigaboot/src/zircon.c` at line 477.

  // TODO(b/236039205): Add memory ranges for GIC. Rfer to
  // `https://cs.opensource.google/fuchsia/fuchsia/+/main:src/firmware/gigaboot/src/zircon.c;line=480;drc=de7e29bf0d7189a61b691ef0cdd0fd5ae1dfd605`

  zbi_result_t result = zbi_create_entry_with_payload(zbi, capacity, ZBI_TYPE_MEM_CONFIG, 0, 0,
                                                      ranges, num_ranges * sizeof(zbi_mem_range_t));
  if (result != ZBI_RESULT_OK) {
    printf("Failed to create memory range entry, %d\n", result);
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok(mkey);
}

bool AddGigabootZbiItems(zbi_header_t* image, size_t capacity, const AbrSlotIndex* slot) {
  if (slot && AppendCurrentSlotZbiItem(image, capacity, *slot) != ZBI_RESULT_OK) {
    return false;
  }

  if (!AppendAcpiRsdp(image, capacity)) {
    return false;
  }

  if (!AddFramebufferIfSupported(image, capacity)) {
    return false;
  }

  if (!AddSystemTable(image, capacity)) {
    return false;
  }

  if (!AppendSmbiosPtr(image, capacity)) {
    return false;
  }

  if (zbi_file_is_initialized && zbi_extend(image, capacity, zbi_files) != ZBI_RESULT_OK) {
    return false;
  }

  return true;
}

}  // namespace gigaboot
