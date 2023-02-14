// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifdef ZIRCON_BOOT_CUSTOM_SYSDEPS_HEADER
#include <zircon_boot_sysdeps.h>
#else
#include <assert.h>
#include <stddef.h>
#include <string.h>
#endif

#include <lib/abr/abr.h>
#include <lib/zbi/zbi.h>
#include <lib/zircon_boot/zircon_boot.h>
#include <zircon/hw/gpt.h>

#include "utils.h"
#include "zircon_ramboot.h"
#include "zircon_vboot.h"

const char* GetSlotPartitionName(const AbrSlotIndex* slot) {
  if (slot == NULL) {
    return GPT_ZIRCON_SLOTLESS_NAME;
  } else if (*slot == kAbrSlotIndexA) {
    return GPT_ZIRCON_A_NAME;
  } else if (*slot == kAbrSlotIndexB) {
    return GPT_ZIRCON_B_NAME;
  } else if (*slot == kAbrSlotIndexR) {
    return GPT_ZIRCON_R_NAME;
  }
  return NULL;
}

static bool ReadAbrMetaData(void* context, size_t size, uint8_t* buffer) {
  ZirconBootOps* ops = (ZirconBootOps*)context;
  size_t read_size;

  return ZIRCON_BOOT_OPS_CALL(ops, read_from_partition, GPT_DURABLE_BOOT_NAME, 0, size, buffer,
                              &read_size) &&
         read_size == size;
}

static bool WriteAbrMetaData(void* context, const uint8_t* buffer, size_t size) {
  ZirconBootOps* ops = (ZirconBootOps*)context;
  size_t write_size;

  return ZIRCON_BOOT_OPS_CALL(ops, write_to_partition, GPT_DURABLE_BOOT_NAME, 0, size, buffer,
                              &write_size) &&
         write_size == size;
}

AbrOps GetAbrOpsFromZirconBootOps(ZirconBootOps* ops) {
  AbrOps abr_ops = {
      .context = ops, .read_abr_metadata = ReadAbrMetaData, .write_abr_metadata = WriteAbrMetaData};
  return abr_ops;
}

static bool IsVerifiedBootOpsImplemented(ZirconBootOps* ops) {
  return ops->verified_boot_get_partition_size && ops->verified_boot_read_rollback_index &&
         ops->verified_boot_write_rollback_index && ops->verified_boot_read_is_device_locked &&
         ops->verified_boot_read_permanent_attributes &&
         ops->verified_boot_read_permanent_attributes_hash;
}

size_t ZbiCheckSize(const void* zbi, size_t max_size) {
  // Copy it locally to avoid any alignment issues.
  zbi_header_t header;
  if (max_size && max_size < sizeof(header)) {
    zircon_boot_dlog("ZBI header exceeds maximum size (%zu)\n", max_size);
    return 0;
  }
  memcpy(&header, zbi, sizeof(header));

  // Check a few of the important fields.
  if (header.type != ZBI_TYPE_CONTAINER || header.extra != ZBI_CONTAINER_MAGIC ||
      header.magic != ZBI_ITEM_MAGIC) {
    zircon_boot_dlog("Image does not look like a ZBI\n");
    return 0;
  }

  size_t result = sizeof(header) + header.length;
  if (result < header.length) {
    zircon_boot_dlog("ZBI size overflow (%zu)\n", result);
    return 0;
  }
  if (max_size && max_size < result) {
    zircon_boot_dlog("ZBI exceeds maximum size (%zu > %zu)\n", result, max_size);
    return 0;
  }

  return result;
}

// Verifies a kernel that has already been loaded into memory.
//
// @ops: boot callbacks.
// @slot: slot to load, or NULL for slotless.
// @load_address: pointer to the loaded kernel.
// @load_address_size: kernel buffer max capacity.
static ZirconBootResult VerifyKernel(ZirconBootOps* ops, const AbrSlotIndex* slot,
                                     void* load_address, size_t load_address_size) {
  // Slotless boot uses empty string for suffix and is always treated as
  // "successful". This allows us to still provide anti-rollback support so
  // that the anti-rollback version stays up-to-date with the single slotless
  // image. There's no purpose trying to actually track slotless image success
  // because:
  //   1. there's no image to fall back to on failure anyway, and
  //   2. there's nowhere to store the success flag since we don't have A/B/R metadata
  const char* ab_suffix = "";
  bool slot_is_marked_successful = true;

  if (slot != NULL) {
    ab_suffix = AbrGetSlotSuffix(*slot);
    AbrOps abr_ops = GetAbrOpsFromZirconBootOps(ops);
    AbrSlotInfo slot_info;
    AbrResult res = AbrGetSlotInfo(&abr_ops, *slot, &slot_info);
    if (res != kAbrResultOk) {
      zircon_boot_dlog("Failed to get slot info %d\n", res);
      return kBootResultErrorSlotVerification;
    }
    slot_is_marked_successful = slot_info.is_marked_successful;
  }

  if (!ZirconVBootSlotVerify(ops, load_address, load_address_size, ab_suffix,
                             slot_is_marked_successful)) {
    zircon_boot_dlog("Slot verification failed\n");
    return kBootResultErrorSlotVerification;
  }
  return kBootResultOK;
}

// Loads and validates the kernel in the given slot.
//
// @ops: boot callbacks.
// @slot: slot to load, or NULL for slotless.
// @load_address: will be filled with the kernel load address.
// @load_address_size: will be filled with the kernel load capacity.
static ZirconBootResult LoadKernel(ZirconBootOps* ops, const AbrSlotIndex* slot,
                                   void** load_address, size_t* load_address_size) {
  const char* zircon_part = GetSlotPartitionName(slot);
  if (zircon_part == NULL) {
    zircon_boot_dlog("Invalid slot idx %d\n", *slot);
    return kBootResultErrorInvalidSlotIdx;
  }
  zircon_boot_dlog("ABR: loading kernel from %s...\n", zircon_part);

  if (!ops->get_kernel_load_buffer) {
    zircon_boot_dlog("Caller must implement get_kernel_load_buffer()\n");
    return kBootResultErrorImageTooLarge;
  }

  zbi_header_t zbi_hdr __attribute__((aligned(ZBI_ALIGNMENT)));
  size_t read_size;
  // This library only deals with zircon image and assume that it always starts from 0 offset.
  if (!ZIRCON_BOOT_OPS_CALL(ops, read_from_partition, zircon_part, 0, sizeof(zbi_hdr), &zbi_hdr,
                            &read_size) ||
      read_size != sizeof(zbi_hdr)) {
    zircon_boot_dlog("Failed to read ZBI header\n");
    return kBootResultErrorReadHeader;
  }

  size_t image_size = ZbiCheckSize(&zbi_hdr, 0);
  if (image_size == 0) {
    zircon_boot_dlog("Fail to find ZBI header\n");
    return kBootResultErrorInvalidZbi;
  }

  *load_address_size = image_size;
  *load_address = ops->get_kernel_load_buffer(ops, load_address_size);
  if (*load_address == NULL) {
    *load_address_size = 0;
    zircon_boot_dlog("Cannot get kernel load buffer\n");
    return kBootResultErrorImageTooLarge;
  }

  if (!ZIRCON_BOOT_OPS_CALL(ops, read_from_partition, zircon_part, 0, image_size, *load_address,
                            &read_size) ||
      read_size != image_size) {
    zircon_boot_dlog("Fail to read ZBI image\n");
    return kBootResultErrorReadImage;
  }

  if (IsVerifiedBootOpsImplemented(ops)) {
    ZirconBootResult res = VerifyKernel(ops, slot, *load_address, *load_address_size);
    if (res != kBootResultOK) {
      return res;
    }
  }

  zircon_boot_dlog("Successfully loaded slot: %s\n", zircon_part);
  return kBootResultOK;
}

static bool ReadAbrMetaDataCache(void* context, size_t size, uint8_t* buffer) {
  assert(size <= sizeof(AbrData));
  memcpy(buffer, context, size);
  return true;
}

static bool WriteAbrMetaDataCache(void* context, const uint8_t* buffer, size_t size) {
  assert(size <= sizeof(AbrData));
  memcpy(context, buffer, size);
  return true;
}

// TODO(b/258467776): We can't use AbrGetBootSlot(.. update_metadata = false, ..) to get the slot to
// boot because it doesn't consider one shot recovery.
AbrSlotIndex AbrPeekBootSlot(const AbrOps* abr_ops) {
  // Load abr metadata into memory and simulate storage.
  uint8_t cache[sizeof(AbrData)];
  assert(abr_ops->read_abr_metadata);
  if (!abr_ops->read_abr_metadata(abr_ops->context, sizeof(cache), cache)) {
    return kAbrSlotIndexR;
  }

  AbrOps cache_ops;
  memset(&cache_ops, 0, sizeof(AbrOps));
  cache_ops.context = cache;
  cache_ops.read_abr_metadata = ReadAbrMetaDataCache;
  cache_ops.write_abr_metadata = WriteAbrMetaDataCache;
  AbrSlotIndex ret = AbrGetBootSlot(&cache_ops, true, NULL);
  return ret;
}

// Loads and validates the kernel based on the provided boot mode A/B/R behavior.
//
// @ops: boot callbacks.
// @boot_mode: where to load the kernel from.
// @load_address: will be filled with the kernel load address.
// @load_address_size: will be filled with the kernel load capacity.
static ZirconBootResult LoadAbr(ZirconBootOps* ops, ZirconBootMode boot_mode, void** load_address,
                                size_t* load_address_size) {
  // The code is simpler if we allocate some slot storage and grab the A/B/R ops
  // here, even though we won't use them in slotless boots.
  AbrSlotIndex slot_storage;
  AbrOps abr_ops = GetAbrOpsFromZirconBootOps(ops);

  AbrSlotIndex* slot = (boot_mode == kZirconBootModeSlotless ? NULL : &slot_storage);
  do {
    // If we're doing a slotted boot, find the next slot to attempt.
    if (slot != NULL) {
      if (ops->firmware_can_boot_kernel_slot) {
        // Make sure the firmware can boot the slot we're going to try. We have
        // use AbrPeekSlot() here because we don't want to modify any data (e.g.
        // boot attempt counters) since we might have to reboot first to get
        // into the matching firmware slot.
        *slot =
            boot_mode == kZirconBootModeForceRecovery ? kAbrSlotIndexR : AbrPeekBootSlot(&abr_ops);
        bool supported = false;
        if (!ZIRCON_BOOT_OPS_CALL(ops, firmware_can_boot_kernel_slot, *slot, &supported)) {
          zircon_boot_dlog("Fail to check slot supported\n");
          return kBootResultErrorIsSlotSupprotedByFirmware;
        }

        if (!supported) {
          zircon_boot_dlog(
              "Target kernel slot %s is not supported by current firmware. Rebooting...\n",
              AbrGetSlotSuffix(*slot));
          ZIRCON_BOOT_OPS_CALL(ops, reboot, boot_mode == kZirconBootModeForceRecovery);
          zircon_boot_dlog("Should not reach here. Reboot handoff failed\n");
          return kBootResultRebootReturn;
        }
      }

      if (boot_mode == kZirconBootModeForceRecovery) {
        *slot = kAbrSlotIndexR;
      } else {
        // This is the one place we call AbrGetBootSlot() which may modify the
        // data to update retry counts, mark failed, etc.
        *slot = AbrGetBootSlot(&abr_ops, true, NULL);
      }
    }

    ZirconBootResult ret = LoadKernel(ops, slot, load_address, load_address_size);
    if (ret == kBootResultOK) {
      break;
    }

    // Slotless boot failure means nothing else to try; fail out.
    if (slot == NULL) {
      zircon_boot_dlog("Failed to load kernel\n");
      return ret;
    }

    zircon_boot_dlog("Failed to load kernel in slot %d\n", *slot);

    // We always try R last, if it fails we're also out of things to try.
    if (*slot == kAbrSlotIndexR) {
      zircon_boot_dlog("Failed to boot: no valid slots\n");
      return kBootResultErrorNoValidSlot;
    }

    // Otherwise, update A/B/R metadata to mark this slot unbootable so we
    // try the next one in the next loop.
    if (AbrMarkSlotUnbootable(&abr_ops, *slot) != kAbrResultOk) {
      return kBootResultErrorMarkUnbootable;
    }
  } while (1);

  // If we got here, the kernel is loaded and validated.
  // Add device-specific ZBI items via user-provided callback.
  if (ops->add_zbi_items && !ops->add_zbi_items(ops, *load_address, *load_address_size, slot)) {
    zircon_boot_dlog("Failed to add ZBI items\n");
    return kBootResultErrorAppendZbiItems;
  }

  return kBootResultOK;
}

ZirconBootResult LoadAndBoot(ZirconBootOps* ops, ZirconBootMode boot_mode) {
  void* load_address = NULL;
  size_t load_address_size = 0;
  ZirconBootResult res = LoadAbr(ops, boot_mode, &load_address, &load_address_size);
  if (res != kBootResultOK) {
    return res;
  }

  ZIRCON_BOOT_OPS_CALL(ops, boot, load_address, load_address_size);
  zircon_boot_dlog("Should not reach here. Boot handoff failed\n");
  return kBootResultBootReturn;
}

ZirconBootResult LoadFromRam(ZirconBootOps* ops, const void* image, size_t size, zbi_header_t** zbi,
                             size_t* zbi_capacity) {
  RambootContext ramboot_context = {};
  ZirconBootOps ramboot_ops = {};
  ZirconBootResult res = SetupRambootOps(ops, image, size, &ramboot_context, &ramboot_ops);
  if (res != kBootResultOK) {
    return res;
  }

  void* load_address = NULL;
  size_t load_address_size = 0;
  res = LoadAbr(&ramboot_ops, kZirconBootModeSlotless, &load_address, &load_address_size);
  if (res != kBootResultOK) {
    return res;
  }

  *zbi = (zbi_header_t*)load_address;
  *zbi_capacity = load_address_size;
  return kBootResultOK;
}

AbrSlotIndex GetActiveBootSlot(ZirconBootOps* ops) {
  AbrOps abr_ops = GetAbrOpsFromZirconBootOps(ops);
  return AbrPeekBootSlot(&abr_ops);
}
