// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZBI_FORMAT_ZBI_H_
#define LIB_ZBI_FORMAT_ZBI_H_

#ifndef __ASSEMBLER__
#include <stdint.h>
#endif

// Zircon Boot Image format (ZBI).
//
// A Zircon Boot Image consists of a container header followed by boot
// items.  Each boot item has a header (zbi_header_t) and then a payload of
// zbi_header_t.length bytes, which can be any size.  The zbi_header_t.type
// field indicates how to interpret the payload.  Many types specify an
// additional type-specific header that begins a variable-sized payload.
// zbi_header_t.length does not include the zbi_header_t itself, but does
// include any type-specific headers as part of the payload.  All fields in
// all header formats are little-endian.
//
// Padding bytes appear after each item as needed to align the payload size
// up to a ZBI_ALIGNMENT (8-byte) boundary.  This padding is not reflected
// in the zbi_header_t.length value.
//
// A bootable ZBI can be booted by a Zircon-compatible boot loader.
// It contains one ZBI_TYPE_KERNEL_{ARCH} boot item that must come first,
// followed by any number of additional boot items whose number, types,
// and details much comport with that kernel's expectations.
//
// A partial ZBI cannot be booted, and is only used during the build process.
// It contains one or more boot items and can be combined with a kernel and
// other ZBIs to make a bootable ZBI.

// All items begin at an 8-byte aligned offset into the image.
#ifdef __ASSEMBLER__
#define ZBI_ALIGNMENT (8)
#else
#define ZBI_ALIGNMENT (8u)
#endif

// Round n up to the next 8 byte boundary
#ifndef __ASSEMBLER__
#ifdef __cplusplus
constexpr
#endif
    static inline uint32_t
    ZBI_ALIGN(uint32_t n) {
  return ((n + ZBI_ALIGNMENT - 1) & -ZBI_ALIGNMENT);
}
#endif

// LSW of sha256("bootdata")
#define ZBI_CONTAINER_MAGIC (0x868cf7e6)

// LSW of sha256("bootitem")
#define ZBI_ITEM_MAGIC (0xb5781729)

// This flag is always required.
#define ZBI_FLAGS_VERSION (0x00010000)

// ZBI items with the CRC32 flag must have a valid crc32.
// Otherwise their crc32 field must contain ZBI_ITEM_NO_CRC32
#define ZBI_FLAGS_CRC32 (0x00020000)

// Value for zbi_header_t.crc32 when ZBI_FLAGS_CRC32 is not set.
#define ZBI_ITEM_NO_CRC32 (0x4a87e8d6)

#ifndef __ASSEMBLER__
// Each header must be 8-byte aligned.  The length field specifies the
// actual payload length and does not include the size of padding.
typedef struct {
  // ZBI_TYPE_* constant, see below.
  uint32_t type;

  // Size of the payload immediately following this header.  This
  // does not include the header itself nor any alignment padding
  // after the payload.
  uint32_t length;

  // Type-specific extra data.  Each type specifies the use of this
  // field; see below.  When not explicitly specified, it should be zero.
  uint32_t extra;

  // Flags for this item.  This must always include ZBI_FLAGS_VERSION.
  // It should contain ZBI_FLAGS_CRC32 for any item where it's feasible
  // to compute the CRC32 at build time.  Other flags are specific to
  // each type; see below.
  uint32_t flags;

  // For future expansion.  Set to 0.
  uint32_t reserved0;
  uint32_t reserved1;

  // Must be ZBI_ITEM_MAGIC.
  uint32_t magic;

  // Must be the CRC32 of payload if ZBI_FLAGS_CRC32 is set,
  // otherwise must be ZBI_ITEM_NO_CRC32.
  uint32_t crc32;
} zbi_header_t;
#endif

// Be sure to add new types to ZBI_ALL_TYPES.
// clang-format off
#define ZBI_ALL_TYPES(macro) \
    macro(ZBI_TYPE_CONTAINER, "CONTAINER", ".bin") \
    macro(ZBI_TYPE_KERNEL_X64, "KERNEL_X64", ".bin") \
    macro(ZBI_TYPE_KERNEL_ARM64, "KERNEL_ARM64", ".bin") \
    macro(ZBI_TYPE_KERNEL_RISCV64, "KERNEL_RISCV64", ".bin") \
    macro(ZBI_TYPE_DISCARD, "DISCARD", ".bin") \
    macro(ZBI_TYPE_STORAGE_RAMDISK, "RAMDISK", ".bin") \
    macro(ZBI_TYPE_STORAGE_BOOTFS, "BOOTFS", ".bin") \
    macro(ZBI_TYPE_STORAGE_BOOTFS_FACTORY, "BOOTFS_FACTORY", ".bin") \
    macro(ZBI_TYPE_STORAGE_KERNEL, "KERNEL", ".bin") \
    macro(ZBI_TYPE_CMDLINE, "CMDLINE", ".txt") \
    macro(ZBI_TYPE_CRASHLOG, "CRASHLOG", ".bin") \
    macro(ZBI_TYPE_NVRAM, "NVRAM", ".bin") \
    macro(ZBI_TYPE_PLATFORM_ID, "PLATFORM_ID", ".bin") \
    macro(ZBI_TYPE_CPU_CONFIG, "CPU_CONFIG", ".bin") /* Deprecated */ \
    macro(ZBI_TYPE_CPU_TOPOLOGY, "CPU_TOPOLOGY", ".bin") \
    macro(ZBI_TYPE_MEM_CONFIG, "MEM_CONFIG", ".bin") \
    macro(ZBI_TYPE_KERNEL_DRIVER, "KERNEL_DRIVER", ".bin") \
    macro(ZBI_TYPE_ACPI_RSDP, "ACPI_RSDP", ".bin") \
    macro(ZBI_TYPE_SMBIOS, "SMBIOS", ".bin") \
    macro(ZBI_TYPE_EFI_SYSTEM_TABLE, "EFI_SYSTEM_TABLE", ".bin") \
    macro(ZBI_TYPE_FRAMEBUFFER, "FRAMEBUFFER", ".bin") \
    macro(ZBI_TYPE_DRV_MAC_ADDRESS, "DRV_MAC_ADDRESS", ".bin") \
    macro(ZBI_TYPE_DRV_PARTITION_MAP, "DRV_PARTITION_MAP", ".bin") \
    macro(ZBI_TYPE_DRV_BOARD_PRIVATE, "DRV_BOARD_PRIVATE", ".bin") \
    macro(ZBI_TYPE_DRV_BOARD_INFO, "DRV_BOARD_INFO", ".bin") \
    macro(ZBI_TYPE_IMAGE_ARGS, "IMAGE_ARGS", ".txt") \
    macro(ZBI_TYPE_BOOT_VERSION, "BOOT_VERSION", ".bin") \
    macro(ZBI_TYPE_HW_REBOOT_REASON, "HW_REBOOT_REASON", ".bin") \
    macro(ZBI_TYPE_SERIAL_NUMBER, "SERIAL_NUMBER", ".txt") \
    macro(ZBI_TYPE_BOOTLOADER_FILE, "BOOTLOADER_FILE", ".bin") \
    macro(ZBI_TYPE_DEVICETREE, "DEVICETREE", ".dtb") \
    macro(ZBI_TYPE_SECURE_ENTROPY, "ENTROPY", ".bin") \
    macro(ZBI_TYPE_EFI_MEMORY_ATTRIBUTES_TABLE, "EFI_MEMORY_ATTRIBUTES_TABLE", ".bin")
// clang-format on

// Each ZBI starts with a container header.
//     length:          Total size of the image after this header.
//                      This includes all item headers, payloads, and padding.
//                      It does not include the container header itself.
//                      Must be a multiple of ZBI_ALIGNMENT.
//     extra:           Must be ZBI_CONTAINER_MAGIC.
//     flags:           Must be ZBI_FLAGS_VERSION and no other flags.
#define ZBI_TYPE_CONTAINER (0x544f4f42)  // BOOT

// Define a container header in assembly code.  The symbol name is defined
// as a local label; use .global symbol to make it global.  The length
// argument can use assembly label arithmetic like any immediate operand.
#ifdef __ASSEMBLER__
// clang-format off
#define ZBI_CONTAINER_HEADER(symbol, length)    \
    .balign ZBI_ALIGNMENT;                      \
    symbol:                                     \
        .int ZBI_TYPE_CONTAINER;                \
        .int (length);                          \
        .int ZBI_CONTAINER_MAGIC;               \
        .int ZBI_FLAGS_VERSION;                  \
        .int 0;                                 \
        .int 0;                                 \
        .int ZBI_ITEM_MAGIC;                    \
        .int ZBI_ITEM_NO_CRC32;                 \
    .size symbol, . - symbol;                   \
    .type symbol, %object
// clang-format on
#else
#define ZBI_CONTAINER_HEADER(length)                                                            \
  {                                                                                             \
    ZBI_TYPE_CONTAINER, (length), ZBI_CONTAINER_MAGIC, ZBI_FLAGS_VERSION, 0, 0, ZBI_ITEM_MAGIC, \
        ZBI_ITEM_NO_CRC32,                                                                      \
  }
#endif

#define ZBI_TYPE_KERNEL_PREFIX (0x004e524b)   // KRN\0
#define ZBI_TYPE_KERNEL_MASK (0x00FFFFFF)     // Mask to compare to the prefix.
#define ZBI_TYPE_KERNEL_X64 (0x4c4e524b)      // KRNL
#define ZBI_TYPE_KERNEL_ARM64 (0x384e524b)    // KRN8
#define ZBI_TYPE_KERNEL_RISCV64 (0x564e524b)  // KRNV

// A discarded item that should just be ignored.  This is used for an
// item that was already processed and should be ignored by whatever
// stage is now looking at the ZBI.  An earlier stage already "consumed"
// this information, but avoided copying data around to remove it from
// the ZBI item stream.
#define ZBI_TYPE_DISCARD (0x50494b53)  // SKIP

// ZBI_TYPE_STORAGE_* types represent an image that might otherwise
// appear on some block storage device, i.e. a RAM disk of some sort.
// All zbi_header_t fields have the same meanings for all these types.
// The interpretation of the payload (after possible decompression) is
// indicated by the specific zbi_header_t.type value.
//
// **Note:** The ZBI_TYPE_STORAGE_* types are not a long-term stable ABI.
//  - Items of these types are always packed for a specific version of the
//    kernel and userland boot services, often in the same build that compiles
//    the kernel.
//  - These item types are **not** expected to be synthesized or
//    examined by boot loaders.
//  - New versions of the `zbi` tool will usually retain the ability to
//    read old formats and non-default switches to write old formats, for
//    diagnostic use.
//
// The zbi_header_t.extra field always gives the exact size of the
// original, uncompressed payload.  That equals zbi_header_t.length when
// the payload is not compressed.  If ZBI_FLAGS_STORAGE_COMPRESSED is set in
// zbi_header_t.flags, then the payload is compressed.
//
// **Note:** Magic-number and header bytes at the start of the compressed
// payload indicate the compression algorithm and parameters.  The set of
// compression formats is not a long-term stable ABI.
//  - Zircon [userboot](../../../../docs/userboot.md) and core services
//    do the decompression.  A given kernel build's `userboot` will usually
//    only support one particular compression format.
//  - The `zbi` tool will usually retain the ability to compress and
//    decompress for old formats, and can be used to convert between formats.
#define ZBI_FLAGS_STORAGE_COMPRESSED (0x00000001)

// A virtual disk image.  This is meant to be treated as if it were a
// storage device.  The payload (after decompression) is the contents of
// the storage device, in whatever format that might be.
#define ZBI_TYPE_STORAGE_RAMDISK (0x4b534452)  // RDSK

// The /boot filesystem in BOOTFS format, specified in internal/bootfs.h.
// Zircon [userboot](../../../docs/userboot.md) handles the contents of this
// filesystem.
#define ZBI_TYPE_STORAGE_BOOTFS (0x42534642)  // BFSB

// Storage used by the kernel (such as a compressed image containing the actual
// kernel).  The meaning and format of the data is specific to the kernel,
// though it always uses the standard storage compression protocol described
// above.  Each particular KERNEL_{ARCH} item image and its STORAGE_KERNEL item
// image are intimately tied and one cannot work without the exact correct
// corresponding other.
#define ZBI_TYPE_STORAGE_KERNEL (0x5254534b)  // KSTR

// Device-specific factory data, stored in BOOTFS format, specified below.
#define ZBI_TYPE_STORAGE_BOOTFS_FACTORY (0x46534642)  // BFSF

// The remaining types are used to communicate information from the boot
// loader to the kernel.  Usually these are synthesized in memory by the
// boot loader, but they can also be included in a ZBI along with the
// kernel and BOOTFS.  Some boot loaders may set the zbi_header_t flags
// and crc32 fields to zero, though setting them to ZBI_FLAGS_VERSION and
// ZBI_ITEM_NO_CRC32 is specified.  The kernel doesn't check.

// A kernel command line fragment, a UTF-8 string that need not be
// NUL-terminated.  The kernel's own option parsing accepts only printable
// ASCII and treats all other characters as equivalent to whitespace. Multiple
// ZBI_TYPE_CMDLINE items can appear.  They are treated as if concatenated with
// ' ' between each item, in the order they appear: first items in the bootable
// ZBI containing the kernel; then items in the ZBI synthesized by the boot
// loader.  The kernel interprets the [whole command line](../../../../docs/kernel_cmdline.md).
#define ZBI_TYPE_CMDLINE (0x4c444d43)  // CMDL

// The crash log from the previous boot, a UTF-8 string.
#define ZBI_TYPE_CRASHLOG (0x4d4f4f42)  // BOOM

// Physical memory region that will persist across warm boots. See zbi_nvram_t
// for payload description.
#define ZBI_TYPE_NVRAM (0x4c4c564e)  // NVLL

// Platform ID Information.
#define ZBI_TYPE_PLATFORM_ID (0x44494C50)  // PLID

// Board-specific information.
#define ZBI_TYPE_DRV_BOARD_INFO (0x4953426D)  // mBSI

// Legacy CPU configuration. See zbi_cpu_config_t for a description of the payload.
#define ZBI_TYPE_CPU_CONFIG (0x43555043)  // CPUC

// CPU configuration. See zbi_topology_node_t for a description of the payload.
#define ZBI_TYPE_CPU_TOPOLOGY (0x544F504F)  // TOPO

// Device memory configuration. See zbi_mem_range_t for a description of the
// payload.
#define ZBI_TYPE_MEM_CONFIG (0x434D454D)  // MEMC

// Kernel driver configuration.  The zbi_header_t.extra field gives a
// ZBI_KERNEL_DRIVER_* type that determines the payload format.
// See <lib/zbi-format/driver-config.h> for details.
#define ZBI_TYPE_KERNEL_DRIVER (0x5652444B)  // KDRV

// ACPI Root Table Pointer, a uint64_t physical address.
#define ZBI_TYPE_ACPI_RSDP (0x50445352)  // RSDP

// SMBIOS entry point, a uint64_t physical address.
#define ZBI_TYPE_SMBIOS (0x49424d53)  // SMBI

// EFI system table, a uint64_t physical address.
#define ZBI_TYPE_EFI_SYSTEM_TABLE (0x53494645)  // EFIS

// EFI memory attributes table. An example of this format can be found in UEFI 2.10 section 4.6.4,
// but the consumer of this item is responsible for interpreting whatever the bootloader supplies
// (in particular the "version" field may differ as the format evolves).
#define ZBI_TYPE_EFI_MEMORY_ATTRIBUTES_TABLE (0x54414d45)  // EMAT

// Framebuffer parameters, a zbi_swfb_t entry.
#define ZBI_TYPE_FRAMEBUFFER (0x42465753)  // SWFB

// The image arguments, data is a trivial text format of one "key=value" per line
// with leading whitespace stripped and "#" comment lines and blank lines ignored.
// It is processed by bootsvc and parsed args are shared to others via Arguments service.
// TODO: the format can be streamlined after the /config/additional_boot_args compat support is
// removed.
#define ZBI_TYPE_IMAGE_ARGS (0x47524149)  // IARG

// A copy of the boot version stored within the sysconfig
// partition
#define ZBI_TYPE_BOOT_VERSION (0x53525642)  // BVRS

// ZBI_TYPE_DRV_* types (LSB is 'm') contain driver metadata.
#define ZBI_TYPE_DRV_METADATA(type) (((type)&0xFF) == 0x6D)  // 'm'

// MAC address for Ethernet, Wifi, Bluetooth, etc.  zbi_header_t.extra
// is a board-specific index to specify which device the MAC address
// applies to.  zbi_header_t.length gives the size in bytes, which
// varies depending on the type of address appropriate for the device.
#define ZBI_TYPE_DRV_MAC_ADDRESS (0x43414D6D)  // mMAC

// A partition map for a storage device, a zbi_partition_map_t header
// followed by one or more zbi_partition_t entries.  zbi_header_t.extra
// is a board-specific index to specify which device this applies to.
#define ZBI_TYPE_DRV_PARTITION_MAP (0x5452506D)  // mPRT

// Private information for the board driver.
#define ZBI_TYPE_DRV_BOARD_PRIVATE (0x524F426D)  // mBOR

#define ZBI_TYPE_HW_REBOOT_REASON (0x42525748)  // HWRB

// The serial number, an unterminated ASCII string of printable non-whitespace
// characters with length zbi_header_t.length.
#define ZBI_TYPE_SERIAL_NUMBER (0x4e4c5253)  // SRLN

// This type specifies a binary file passed in by the bootloader.
// The first byte specifies the length of the filename without a NUL terminator.
// The filename starts on the second byte.
// The file contents are located immediately after the filename.
//
// Layout: | name_len |        name       |   payload
//           ^(1 byte)  ^(name_len bytes)     ^(length of file)
#define ZBI_TYPE_BOOTLOADER_FILE (0x4C465442)  // BTFL

// The devicetree blob from the legacy boot loader, if any.  This is used only
// for diagnostic and development purposes.  Zircon kernel and driver
// configuration is entirely driven by specific ZBI items from the boot
// loader.  The boot shims for legacy boot loaders pass the raw devicetree
// along for development purposes, but extract information from it to populate
// specific ZBI items such as ZBI_TYPE_KERNEL_DRIVER et al.
#define ZBI_TYPE_DEVICETREE (0xd00dfeed)

// An arbitrary number of random bytes attested to have high entropy.  Any
// number of items of any size can be provided, but no data should be provided
// that is not true entropy of cryptographic quality.  This is used to seed
// secure cryptographic pseudo-random number generators.
#define ZBI_TYPE_SECURE_ENTROPY (0x444e4152)  // RAND

// This provides a data dump and associated logging from a boot loader,
// shim, or earlier incarnation that wants its data percolated up by the
// booting Zircon kernel. See zbi_debugdata_t for a description of the
// payload.
#define ZBI_TYPE_DEBUGDATA (0x44474244)  // DBGD

#endif  // LIB_ZBI_FORMAT_ZBI_H_
