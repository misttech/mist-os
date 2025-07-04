// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/ftl/ftln/volume.h"

#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdint>
#include <cstring>
#include <memory>
#include <string>
#include <utility>

#include "src/storage/lib/ftl/ftl.h"
#include "src/storage/lib/ftl/ftln/diagnose.h"
#include "src/storage/lib/ftl/ftln/ftlnp.h"
#include "src/storage/lib/ftl/ftln/ndm-driver.h"

namespace ftl {

const char* VolumeImpl::Init(std::unique_ptr<NdmDriver> driver) {
  ZX_DEBUG_ASSERT(!driver_);
  driver_ = std::move(driver);

  if (!InitModules()) {
    return "Module initialization failed";
  }

  return Attach();
}

const char* VolumeImpl::ReAttach() {
  if (!driver_->Detach()) {
    return "Failed to remove volume";
  }
  name_ = nullptr;

  return Attach();
}

zx_status_t VolumeImpl::Read(uint32_t first_page, int num_pages, void* buffer) {
  if (read_pages_(buffer, first_page, num_pages, vol_) != 0) {
    return ZX_ERR_IO;
  }
  return ZX_OK;
}

zx_status_t VolumeImpl::Write(uint32_t first_page, int num_pages, const void* buffer) {
  if (write_pages_(const_cast<void*>(buffer), first_page, num_pages, vol_) != 0) {
    return ZX_ERR_IO;
  }
  return ZX_OK;
}

zx_status_t VolumeImpl::Format() {
  if (report_(vol_, FS_FORMAT) != 0) {
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

zx_status_t VolumeImpl::FormatAndLevel() {
  if (report_(vol_, FS_FORMAT_RESET_WC) != 0) {
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

zx_status_t VolumeImpl::Mount() {
  if (report_(vol_, FS_MOUNT) != 0) {
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

zx_status_t VolumeImpl::Unmount() {
  if (report_(vol_, FS_UNMOUNT) != 0) {
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

zx_status_t VolumeImpl::Flush() {
  if (report_(vol_, FS_SYNC) != 0) {
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

zx_status_t VolumeImpl::Trim(uint32_t first_page, uint32_t num_pages) {
  if (report_(vol_, FS_MARK_UNUSED, first_page, num_pages) != 0) {
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

zx_status_t VolumeImpl::GarbageCollect() {
  int result = report_(vol_, FS_VCLEAN);
  if (result < 0) {
    return ZX_ERR_BAD_STATE;
  }

  if (result == 0) {
    return ZX_ERR_STOP;
  }
  return ZX_OK;
}

zx_status_t VolumeImpl::GetStats(Stats* stats) {
  vstat buffer;
  if (report_(vol_, FS_VSTAT, &buffer) != 0) {
    return ZX_ERR_BAD_STATE;
  }
  stats->ram_used = buffer.ndm.ram_used;
  stats->wear_count = buffer.ndm.wear_count;
  stats->initial_bad_blocks = buffer.initial_bad_blocks;
  stats->running_bad_blocks = buffer.running_bad_blocks;
  stats->worn_blocks_detected = buffer.worn_blocks_detected;
  stats->garbage_level = buffer.garbage_level;
  memcpy(stats->wear_histogram, buffer.wear_histogram, sizeof(stats->wear_histogram));
  stats->num_blocks = buffer.num_blocks;

  static_assert(sizeof(stats->map_block_end_page_failure_reasons) ==
                    sizeof(buffer.map_block_end_page_failure_reasons) &&
                sizeof(*stats->map_block_end_page_failure_reasons) ==
                    sizeof(*buffer.map_block_end_page_failure_reasons));
  memcpy(stats->map_block_end_page_failure_reasons, buffer.map_block_end_page_failure_reasons,
         sizeof(stats->map_block_end_page_failure_reasons));

  return ZX_OK;
}

zx_status_t VolumeImpl::GetCounters(Counters* counters) {
  FtlCounters ftl_counters = {};
  if (report_(vol_, FS_COUNTERS, &ftl_counters) != 0) {
    return ZX_ERR_BAD_STATE;
  }
  counters->wear_count = ftl_counters.wear_count;
  counters->initial_bad_blocks = ftl_counters.initial_bad_blocks;
  counters->running_bad_blocks = ftl_counters.running_bad_blocks;
  counters->worn_blocks_detected = ftl_counters.worn_blocks_detected;
  return ZX_OK;
}

zx_status_t VolumeImpl::GetNewWearLeveling(bool* state) {
  int state_ptr;
  if (report_(vol_, FS_GET_NEW_WEAR_LEVELING, &state_ptr) != 0) {
    return ZX_ERR_BAD_STATE;
  }
  *state = (state_ptr != 0);
  return ZX_OK;
}

zx_status_t VolumeImpl::SetNewWearLeveling(bool state) {
  if (report_(vol_, FS_SET_NEW_WEAR_LEVELING, state ? 1 : 0) != 0) {
    return ZX_ERR_BAD_STATE;
  }
  return ZX_OK;
}

bool VolumeImpl::OnVolumeAdded(const XfsVol* ftl) {
  ZX_DEBUG_ASSERT(!Created());
  vol_ = ftl->vol;
  name_ = ftl->name;
  report_ = ftl->report;
  write_pages_ = ftl->write_pages;
  read_pages_ = ftl->read_pages;

  return owner_->OnVolumeAdded(ftl->page_size, ftl->num_pages);
}

std::string VolumeImpl::DiagnoseKnownIssues() const {
  return ftl::FtlnDiagnoseIssues(static_cast<FTLN>(vol_));
}

void* VolumeImpl::GetInternalVolumeForTest() { return vol_; }

bool VolumeImpl::Created() const { return name_; }

const char* VolumeImpl::Attach() {
  const char* error = driver_->Attach(this);
  if (error) {
    return error;
  }

  if (!Created()) {
    return "No volume added";
  }

  if (Mount() != ZX_OK) {
    return "Mount failed";
  }
  return nullptr;
}

}  // namespace ftl

// Callback from the FTL.
int XfsAddVol(XfsVol* ftl) {
  ftl::VolumeImpl* volume = reinterpret_cast<ftl::VolumeImpl*>(ftl->ftl_volume);
  if (volume && !volume->OnVolumeAdded(ftl)) {
    return -1;
  }
  return 0;
}
