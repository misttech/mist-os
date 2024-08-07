// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/ftl/ftln/ndm-driver.h"

#include <stdarg.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>

#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>

#include "src/storage/lib/ftl/ftl.h"
#include "src/storage/lib/ftl/ftln/ftlnp.h"
#include "src/storage/lib/ftl/ftln/logger.h"
#include "src/storage/lib/ftl/ndm/ndmp.h"

namespace ftl {

namespace {

// Many of the NDM driver functions and tests require that these constants match. If they ever
// are updated to be unique constants, the various API contracts need to be updated accordingly.
static_assert(kNdmUncorrectableEcc == kNdmError, "kNdmUncorrectableEcc should map to kNdmError!");

bool g_init_performed = false;

// Extra configuration data saved to the partition info.
struct UserData {
  uint16_t major_version = 1;
  uint16_t minor_version = 0;
  uint32_t ftl_flags;   // Flags used to create the FtlNdmVol structure.
  uint32_t extra_free;  // Overallocation for the FTL.
  uint32_t reserved_1[5];
  VolumeOptions options;
  uint32_t reserved_2[10];
};
static_assert(sizeof(UserData) == 96);

// This structure exposes the two views into the partition data.
// See ftl.h for the definition of NDMPartitionInfo.
union PartitionInfo {
  NDMPartitionInfo ndm;
  struct {
    // This is the equivalent structure, with an explicit |data| field of the
    // "correct" type, instead of just a placeholder. In this case, |data_size|
    // tracks |data|.
    NDMPartition basic_data;
    uint32_t data_size = sizeof(UserData);
    UserData data;
  } exploded;
};
static_assert(sizeof(NDMPartition) + sizeof(uint32_t) == sizeof(NDMPartitionInfo));
static_assert(sizeof(NDMPartitionInfo) + sizeof(UserData) == sizeof(PartitionInfo));

// Fills |data| with the desired configuration info.
void CopyConfigData(const VolumeOptions& options, const FtlNdmVol& ftl, UserData* data) {
  data->ftl_flags = ftl.flags;
  data->extra_free = ftl.extra_free;
  data->options = options;
}

// Implementation of the driver interface:

// Returns kNdmOk, kNdmUncorrectableEcc, kNdmFatalError or kNdmUnsafeEcc.
int ReadPagesImpl(uint32_t page, uint32_t count, uint8_t* data, uint8_t* spare, void* dev) {
  NdmDriver* device = reinterpret_cast<NdmDriver*>(dev);
  return device->NandRead(page, count, data, spare);
}

// Returns kNdmOk, kNdmUncorrectableEcc, kNdmFatalError or kNdmUnsafeEcc.
int ReadPages(uint32_t page, uint32_t count, uint8_t* data, uint8_t* spare, void* dev) {
  return ReadPagesImpl(page, count, data, nullptr, dev);
}

// Returns kNdmOk, kNdmUncorrectableEcc, kNdmFatalError or kNdmUnsafeEcc.
int ReadPage(uint32_t page, uint8_t* data, uint8_t* spare, void* dev) {
  return ReadPagesImpl(page, 1, data, nullptr, dev);
}

// Returns kNdmOk, kNdmFatalError, or kNdmUncorrectableEcc on ECC decode failure.
int ReadSpare(uint32_t page, uint8_t* spare, void* dev) {
  int result = ReadPagesImpl(page, 1, nullptr, spare, dev);
  // Ensure we translate errors to the correct values above, since
  // kNdmUnsafeEcc is OK as the data is still correct.
  return result == kNdmUnsafeEcc ? kNdmOk : result;
}

// Returns kNdmOk, kNdmError or kNdmFatalError. kNdmError triggers marking the block as bad.
int WritePages(uint32_t page, uint32_t count, const uint8_t* data, uint8_t* spare, int action,
               void* dev) {
  NdmDriver* device = reinterpret_cast<NdmDriver*>(dev);
  for (uint32_t i = 0; i < count; i++) {
    FtlnSetSpareValidity(&spare[i * device->SpareSize()]);
  }
  return device->NandWrite(page, count, data, spare);
}

// Returns kNdmOk, kNdmError or kNdmFatalError. kNdmError triggers marking the block as bad.
int WritePage(uint32_t page, const uint8_t* data, uint8_t* spare, int action, void* dev) {
  return WritePages(page, 1, data, spare, action, dev);
}

// Returns kNdmOk or kNdmError. kNdmError triggers marking the block as bad.
int EraseBlock(uint32_t page, void* dev) {
  NdmDriver* device = reinterpret_cast<NdmDriver*>(dev);
  return device->NandErase(page);
}

// Returns kTrue, kFalse or kNdmError.
int IsBadBlockImpl(uint32_t page, void* dev) {
  NdmDriver* device = reinterpret_cast<NdmDriver*>(dev);
  return device->IsBadBlock(page);
}

// Returns kTrue or kFalse (kFalse on error).
int IsEmpty(uint32_t page, uint8_t* data, uint8_t* spare, void* dev) {
  int result = ReadPagesImpl(page, 1, data, spare, dev);

  // kNdmUncorrectableEcc and kNdmUnsafeEcc are ok.
  if (result == kNdmFatalError) {
    return kFalse;
  }

  const NdmDriver* device = reinterpret_cast<const NdmDriver*>(dev);
  return device->IsEmptyPage(page, data, spare) ? kTrue : kFalse;
}

// Returns kNdmOk or kNdmFatalError, where kNdmFatalError implies aborting initialization.
int CheckPage(uint32_t page, uint8_t* data, uint8_t* spare, int* status, void* dev) {
  int result = ReadPagesImpl(page, 1, data, spare, dev);

  if (result == kNdmFatalError) {
    *status = NDM_PAGE_INVALID;
    return kNdmFatalError;
  }

  // Mark page as invalid if the data fails ECC or if we detected a partial page write, since both
  // the page and spare data should not be used in that case.
  const NdmDriver* device = reinterpret_cast<const NdmDriver*>(dev);
  if (result == kNdmUncorrectableEcc || device->IncompletePageWrite(spare)) {
    *status = NDM_PAGE_INVALID;
    return kNdmOk;
  }

  *status = device->IsEmptyPage(page, data, spare) ? NDM_PAGE_ERASED : NDM_PAGE_VALID;
  return kNdmOk;
}

__PRINTFLIKE(3, 4) void LogTrace(const char* file, int line, const char* format, ...) {
  fprintf(stderr, "[FTL] TRACE: ");
  va_list args;
  va_start(args, format);
  vfprintf(stderr, format, args);
  va_end(args);
  fprintf(stderr, "\n");
}

__PRINTFLIKE(3, 4) void LogDebug(const char* file, int line, const char* format, ...) {
  fprintf(stderr, "[FTL] DEBUG: ");
  va_list args;
  va_start(args, format);
  vfprintf(stderr, format, args);
  va_end(args);
  fprintf(stderr, "\n");
}

__PRINTFLIKE(3, 4) void LogInfo(const char* file, int line, const char* format, ...) {
  fprintf(stderr, "[FTL] INFO: ");
  va_list args;
  va_start(args, format);
  vfprintf(stderr, format, args);
  va_end(args);
  fprintf(stderr, "\n");
}

__PRINTFLIKE(3, 4) void LogWarning(const char* file, int line, const char* format, ...) {
  fprintf(stderr, "[FTL] WARNING: ");
  va_list args;
  va_start(args, format);
  vfprintf(stderr, format, args);
  va_end(args);
  fprintf(stderr, "\n");
}

__PRINTFLIKE(3, 4) void LogError(const char* file, int line, const char* format, ...) {
  fprintf(stderr, "[FTL] ERROR: ");
  va_list args;
  va_start(args, format);
  vfprintf(stderr, format, args);
  va_end(args);
  fprintf(stderr, "\n");
}

}  // namespace

FtlLogger DefaultLogger() {
  return FtlLogger{
      .trace = &LogTrace,
      .debug = &LogDebug,
      .info = &LogInfo,
      .warn = &LogWarning,
      .error = &LogError,
  };
}

NdmBaseDriver::~NdmBaseDriver() { RemoveNdmVolume(); }

bool NdmBaseDriver::IsNdmDataPresent(const VolumeOptions& options, bool use_format_v2) {
  NDMDrvr driver;
  FillNdmDriver(options, use_format_v2, &driver);

  SetFsErrCode(NDM_OK);
  ndm_ = ndmAddDev(&driver);
  return ndm_ || GetFsErrCode() != NDM_NO_META_BLK;
}

bool NdmBaseDriver::BadBbtReservation() const {
  if (ndm_) {
    return false;
  }
  FsErrorCode error = static_cast<FsErrorCode>(GetFsErrCode());
  switch (error) {
    case NDM_TOO_MANY_IBAD:
    case NDM_TOO_MANY_RBAD:
    case NDM_RBAD_LOCATION:
      return true;
    default:
      return false;
  }
}

const char* NdmBaseDriver::CreateNdmVolume(const Volume* ftl_volume, const VolumeOptions& options,
                                           bool save_volume_data) {
  if (!ndm_) {
    IsNdmDataPresent(options, save_volume_data);
  }

  if (!ndm_) {
    return "ndmAddDev failed";
  }

  PartitionInfo partition = {};
  partition.exploded = {};  // Initialize the "real" structure.
  FtlNdmVol ftl = {};
  XfsVol xfs = {};

  ftl.flags = FSF_EXTRA_FREE;
  ftl.cached_map_pages = options.num_blocks * (options.block_size / options.page_size);
  ftl.extra_free = 6;  // Over-provision 6% of the device.
  xfs.ftl_volume = const_cast<Volume*>(ftl_volume);
  ftl.logger = logger_;

  partition.exploded.basic_data.num_blocks = ndmGetNumVBlocks(ndm_);
  strncpy(partition.exploded.basic_data.name, "ftl", sizeof(partition.exploded.basic_data.name));
  CopyConfigData(options, ftl, &partition.exploded.data);

  if (save_volume_data) {
    const NDMPartitionInfo* info = ndmGetPartitionInfo(ndm_);
    if (info) {
      volume_data_saved_ = true;
    }

    if (ndmWritePartitionInfo(ndm_, &partition.ndm) != 0) {
      return "ndmWritePartitionInfo failed";
    }

    if (!info && !(options.flags & kReadOnlyInit)) {
      // There was no volume information saved, save it now.
      if (ndmSavePartitionTable(ndm_) != 0) {
        return "ndmSavePartitionTable failed";
      }
      volume_data_saved_ = true;
    }
  } else {
    // This call also allocates the partition data, but old style.
    if (ndmSetNumPartitions(ndm_, 1) != 0) {
      return "ndmSetNumPartitions failed";
    }

    if (ndmWritePartition(ndm_, &partition.ndm.basic_data, 0, "ftl") != 0) {
      return "ndmWritePartition failed";
    }
  }

  if (ndmAddVolFTL(ndm_, 0, &ftl, &xfs) == NULL) {
    return "ndmAddVolFTL failed";
  }

  return nullptr;
}

bool NdmBaseDriver::RemoveNdmVolume() {
  if (ndm_ && ndmDelDev(ndm_) == 0) {
    ndm_ = nullptr;
    return true;
  }
  return false;
}

bool NdmBaseDriver::SaveBadBlockData() { return ndmExtractBBL(ndm_) >= 0 ? true : false; }

bool NdmBaseDriver::RestoreBadBlockData() { return ndmInsertBBL(ndm_) == 0 ? true : false; }

bool NdmBaseDriver::IsEmptyPageImpl(const uint8_t* data, uint32_t data_len, const uint8_t* spare,
                                    uint32_t spare_len) const {
  const int64_t* pointer = reinterpret_cast<const int64_t*>(data);
  ZX_DEBUG_ASSERT(data_len % sizeof(*pointer) == 0);
  for (size_t i = 0; i < data_len / sizeof(*pointer); i++) {
    if (pointer[i] != -1) {
      return false;
    }
  }

  ZX_DEBUG_ASSERT(spare_len % sizeof(*pointer) == 0);
  pointer = reinterpret_cast<const int64_t*>(spare);
  for (size_t i = 0; i < spare_len / sizeof(*pointer); i++) {
    if (pointer[i] != -1) {
      return false;
    }
  }
  return true;
}

const VolumeOptions* NdmBaseDriver::GetSavedOptions() const {
  const NDMPartitionInfo* partition = ndmGetPartitionInfo(ndm_);
  if (!partition) {
    return nullptr;
  }

  auto info = reinterpret_cast<const PartitionInfo*>(partition);
  if (info->exploded.data_size != sizeof(UserData)) {
    return nullptr;
  }

  if (info->exploded.data.major_version != 1) {
    return nullptr;
  }

  return &info->exploded.data.options;
}

bool NdmBaseDriver::WriteVolumeData() {
  if (ndmSavePartitionTable(ndm_) != 0) {
    return false;
  }
  volume_data_saved_ = true;
  return true;
}

void NdmBaseDriver::FillNdmDriver(const VolumeOptions& options, bool use_format_v2,
                                  NDMDrvr* driver) const {
  *driver = {};
  driver->num_blocks = options.num_blocks;
  driver->max_bad_blocks = options.max_bad_blocks;
  driver->block_size = options.block_size;
  driver->page_size = options.page_size;
  driver->eb_size = options.eb_size;
  driver->flags = FSF_MULTI_ACCESS | FSF_FREE_SPARE_ECC | options.flags;
  driver->format_version_2 = use_format_v2;
  driver->dev = const_cast<NdmBaseDriver*>(this);
  driver->type = NDM_SLC;
  driver->read_pages = ReadPages;
  driver->write_pages = WritePages;
  driver->write_data_and_spare = WritePage;
  driver->read_decode_data = ReadPage;
  driver->read_decode_spare = ReadSpare;
  // Since we have "free" decoding, we use the same ReadSpare function for this callback (which
  // should never be invoked anyways if FSF_FREE_SPARE_ECC is set). If this changes in the future,
  // this callback should be updated and the the FSF_FREE_SPARE_ECC flag cleared.
  driver->read_spare = ReadSpare;
  driver->data_and_spare_erased = IsEmpty;
  driver->data_and_spare_check = CheckPage;
  driver->erase_block = EraseBlock;
  driver->is_block_bad = IsBadBlockImpl;
  driver->logger = logger_;
}

uint32_t NdmBaseDriver::PageSize() const { return ndm_->page_size; }

uint8_t NdmBaseDriver::SpareSize() const { return ndm_->eb_size; }

bool NdmBaseDriver::IncompletePageWrite(uint8_t* spare) const { return FtlnIncompleteWrite(spare); }

__EXPORT
bool InitModules() {
  if (!g_init_performed) {
    // Unfortunately, module initialization is a global affair, and there is
    // no cleanup. At least, make sure no re-initialization takes place.
    if (NdmInit() != 0 || FtlInit() != 0) {
      return false;
    }
    g_init_performed = true;
  }
  return true;
}

}  // namespace ftl
