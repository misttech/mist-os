// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/sdmmc/c/banjo.h>
#include <inttypes.h>
#include <lib/fit/defer.h>
#include <lib/sdmmc/hw.h>
#include <lib/zx/time.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <pretty/hexdump.h>

#include "sdmmc-block-device.h"

namespace {

constexpr uint32_t kFreq200MHz = 200'000'000;
constexpr uint32_t kFreq52MHz = 52'000'000;
constexpr uint32_t kFreq26MHz = 26'000'000;

constexpr uint64_t kMmcSectorSize = 512;  // physical sector size
constexpr uint64_t kMmcBlockSize = 512;   // block size is 512 bytes always because it is the
                                          // required value if the card is in DDR mode

constexpr uint32_t kSwitchTimeMultiplierMs = 10;
constexpr uint32_t kSwitchStatusRetries = 3;

}  // namespace

namespace {

zx_status_t DecodeCid(const std::array<uint8_t, SDMMC_CID_SIZE>& raw_cid, fdf::Logger& logger) {
  FDF_LOGL(INFO, logger, "product name=%c%c%c%c%c%c", raw_cid[MMC_CID_PRODUCT_NAME_START],
           raw_cid[MMC_CID_PRODUCT_NAME_START + 1], raw_cid[MMC_CID_PRODUCT_NAME_START + 2],
           raw_cid[MMC_CID_PRODUCT_NAME_START + 3], raw_cid[MMC_CID_PRODUCT_NAME_START + 4],
           raw_cid[MMC_CID_PRODUCT_NAME_START + 5]);
  FDF_LOGL(INFO, logger, "       revision=%u.%u", (raw_cid[MMC_CID_REVISION] >> 4) & 0xf,
           raw_cid[MMC_CID_REVISION] & 0xf);
  uint32_t serial;
  memcpy(&serial, reinterpret_cast<const std::byte*>(&raw_cid[MMC_CID_SERIAL]), sizeof(uint32_t));
  FDF_LOGL(INFO, logger, "       serial=%u", serial);
  return ZX_OK;
}

zx_status_t DecodeCsd(const std::array<uint8_t, SDMMC_CSD_SIZE>& raw_csd, fdf::Logger& logger) {
  uint8_t spec_vrsn = (raw_csd[MMC_CSD_SPEC_VERSION] >> 2) & 0xf;
  // Only support spec version > 4.0
  if (spec_vrsn < MMC_CID_SPEC_VRSN_40) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  FDF_LOGL(TRACE, logger, "CSD version %u spec version %u",
           (raw_csd[MMC_CSD_SPEC_VERSION] >> 6) & 0x3, spec_vrsn);
  if (fdf::Logger::GlobalInstance()->GetSeverity() <= FUCHSIA_LOG_TRACE) {
    FDF_LOGL(TRACE, logger, "CSD:");
    hexdump8_ex(raw_csd.data(), SDMMC_CSD_SIZE, 0);
  }

  // Only support high capacity (> 2GB) cards
  uint16_t c_size = static_cast<uint16_t>(((raw_csd[MMC_CSD_SIZE_START] >> 6) & 0x3) |
                                          (raw_csd[MMC_CSD_SIZE_START + 1] << 2) |
                                          ((raw_csd[MMC_CSD_SIZE_START + 2] & 0x3) << 10));
  if (c_size != 0xfff) {
    FDF_LOGL(ERROR, logger, "unsupported C_SIZE 0x%04x", c_size);
    return ZX_ERR_NOT_SUPPORTED;
  }
  return ZX_OK;
}

uint64_t GetCacheSizeBits(const std::array<uint8_t, MMC_EXT_CSD_SIZE>& raw_ext_csd) {
  uint64_t cache_size = raw_ext_csd[MMC_EXT_CSD_CACHE_SIZE_MSB] << 24 |
                        raw_ext_csd[MMC_EXT_CSD_CACHE_SIZE_251] << 16 |
                        raw_ext_csd[MMC_EXT_CSD_CACHE_SIZE_250] << 8 |
                        raw_ext_csd[MMC_EXT_CSD_CACHE_SIZE_LSB];  // In 1024-bit units.
  cache_size *= 1024;
  return cache_size;
}

}  // namespace

namespace sdmmc {

zx_status_t SdmmcBlockDevice::MmcDoSwitch(uint8_t index, uint8_t value) {
  // Send the MMC_SWITCH command
  zx_status_t st = sdmmc_->MmcSwitch(index, value);
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to MMC_SWITCH (0x%x=%d): %s", index, value,
             zx_status_get_string(st));
    return st;
  }

  return MmcWaitForSwitch(index, value);
}

zx_status_t SdmmcBlockDevice::MmcWaitForSwitch(uint8_t index, uint8_t value) {
  uint8_t switch_time;
  if (index == MMC_EXT_CSD_FLUSH_CACHE) {
    switch_time = 0;  // Rely on the SDMMC platform driver to wait for the busy signal to clear.
  } else {
    // The GENERIC_CMD6_TIME field defines a maximum timeout value for CMD6 in tens of milliseconds.
    // There does not appear to be any other way to check the status of CMD6, so just sleep for the
    // maximum required time before issuing CMD13.
    switch_time = raw_ext_csd_[MMC_EXT_CSD_GENERIC_CMD6_TIME];
    if (index == MMC_EXT_CSD_PARTITION_CONFIG &&
        raw_ext_csd_[MMC_EXT_CSD_PARTITION_SWITCH_TIME] > 0) {
      switch_time = raw_ext_csd_[MMC_EXT_CSD_PARTITION_SWITCH_TIME];
    }
  }

  if (switch_time) {
    zx::nanosleep(zx::deadline_after(zx::msec(kSwitchTimeMultiplierMs * switch_time)));
  }

  // Check status after MMC_SWITCH
  uint32_t resp;
  zx_status_t st = ZX_ERR_BAD_STATE;
  for (uint32_t i = 0; i < kSwitchStatusRetries && st != ZX_OK; i++) {
    st = sdmmc_->SdmmcSendStatus(&resp);
  }

  if (st == ZX_OK) {
    if (resp & MMC_STATUS_SWITCH_ERR) {
      FDF_LOGL(ERROR, logger(), "mmc switch error after MMC_SWITCH (0x%x=%d), status = 0x%08x",
               index, value, resp);
      st = ZX_ERR_INTERNAL;
    } else if ((index == MMC_EXT_CSD_FLUSH_CACHE) && (resp & MMC_STATUS_ERR)) {
      FDF_LOGL(ERROR, logger(), "mmc status error after MMC_SWITCH (0x%x=%d), status = 0x%08x",
               index, value, resp);
      st = ZX_ERR_IO;
    }
  } else {
    FDF_LOGL(ERROR, logger(), "failed to MMC_SEND_STATUS (%x=%d): %s", index, value,
             zx_status_get_string(st));
  }

  return st;
}

zx_status_t SdmmcBlockDevice::MmcSetBusWidth(sdmmc_bus_width_t bus_width,
                                             uint8_t mmc_ext_csd_bus_width) {
  // Switch the card to the new bus width
  zx_status_t st = MmcDoSwitch(MMC_EXT_CSD_BUS_WIDTH, mmc_ext_csd_bus_width);
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to switch bus width to EXT_CSD %d: %s", mmc_ext_csd_bus_width,
             zx_status_get_string(st));
    return ZX_ERR_INTERNAL;
  }

  if (bus_width != bus_width_) {
    // Switch the host to the new bus width
    if ((st = sdmmc_->SetBusWidth(bus_width)) != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "failed to switch the host bus width to %d: %s", bus_width,
               zx_status_get_string(st));
      return ZX_ERR_INTERNAL;
    }
  }
  bus_width_ = bus_width;
  return ZX_OK;
}

uint8_t SdmmcBlockDevice::MmcSelectBusWidth() {
  // TODO verify host 8-bit support
  uint8_t bus_widths[] = {SDMMC_BUS_WIDTH_EIGHT, MMC_EXT_CSD_BUS_WIDTH_8,
                          SDMMC_BUS_WIDTH_FOUR,  MMC_EXT_CSD_BUS_WIDTH_4,
                          SDMMC_BUS_WIDTH_ONE,   MMC_EXT_CSD_BUS_WIDTH_1};
  for (unsigned i = 0; i < (sizeof(bus_widths) / sizeof(uint8_t)); i += 2) {
    if (MmcSetBusWidth(bus_widths[i], bus_widths[i + 1]) == ZX_OK) {
      break;
    }
  }
  return bus_width_;
}

zx_status_t SdmmcBlockDevice::MmcSwitchTiming(sdmmc_timing_t new_timing) {
  // Switch the device timing
  uint8_t ext_csd_timing;
  switch (new_timing) {
    case SDMMC_TIMING_LEGACY:
      ext_csd_timing = MMC_EXT_CSD_HS_TIMING_LEGACY;
      break;
    case SDMMC_TIMING_HS:
      ext_csd_timing = MMC_EXT_CSD_HS_TIMING_HS;
      break;
    case SDMMC_TIMING_HSDDR:
      // sdhci has a different timing constant for HSDDR vs HS
      ext_csd_timing = MMC_EXT_CSD_HS_TIMING_HS;
      break;
    case SDMMC_TIMING_HS200:
      ext_csd_timing = MMC_EXT_CSD_HS_TIMING_HS200;
      break;
    case SDMMC_TIMING_HS400:
      ext_csd_timing = MMC_EXT_CSD_HS_TIMING_HS400;
      break;
    default:
      return ZX_ERR_INVALID_ARGS;
  };

  zx_status_t st = MmcDoSwitch(MMC_EXT_CSD_HS_TIMING, ext_csd_timing);
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to switch device timing to %d", new_timing);
    return st;
  }

  // Switch the host timing
  if ((st = sdmmc_->SetTiming(new_timing)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to switch host timing to %d", new_timing);
    return st;
  }

  timing_ = new_timing;
  return st;
}

zx_status_t SdmmcBlockDevice::MmcSwitchTimingHs200ToHs() {
  zx_status_t st = sdmmc_->MmcSwitch(MMC_EXT_CSD_HS_TIMING, MMC_EXT_CSD_HS_TIMING_HS);
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to MMC_SWITCH (0x%x=%d): %s", MMC_EXT_CSD_HS_TIMING,
             MMC_EXT_CSD_HS_TIMING_HS, zx_status_get_string(st));
    return st;
  }

  // The host must switch to HS timing/frequency before checking the status of MMC_SWITCH command.
  if ((st = sdmmc_->SetTiming(SDMMC_TIMING_HS)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to switch host timing to %d", SDMMC_TIMING_HS);
    return st;
  }

  if ((st = MmcSwitchFreq(kFreq52MHz)) != ZX_OK) {
    return st;
  }

  if ((st = MmcWaitForSwitch(MMC_EXT_CSD_HS_TIMING, MMC_EXT_CSD_HS_TIMING_HS)) != ZX_OK) {
    return st;
  }

  timing_ = SDMMC_TIMING_HS;
  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::MmcSwitchFreq(uint32_t new_freq) {
  zx_status_t st;
  if ((st = sdmmc_->SetBusFreq(new_freq)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "failed to set host bus frequency: %s", zx_status_get_string(st));
    return st;
  }
  clock_rate_ = new_freq;
  return ZX_OK;
}

zx_status_t SdmmcBlockDevice::MmcDecodeExtCsd() {
  FDF_LOGL(TRACE, logger(), "EXT_CSD version %u CSD version %u", raw_ext_csd_[192],
           raw_ext_csd_[194]);

  // Get the capacity for the card
  uint32_t sectors = (raw_ext_csd_[212] << 0) | (raw_ext_csd_[213] << 8) |
                     (raw_ext_csd_[214] << 16) | (raw_ext_csd_[215] << 24);
  block_info_.block_count = sectors * kMmcSectorSize / kMmcBlockSize;
  block_info_.block_size = kMmcBlockSize;

  FDF_LOGL(DEBUG, logger(), "found card with capacity = %" PRIu64 "B",
           block_info_.block_count * block_info_.block_size);

  return ZX_OK;
}

bool SdmmcBlockDevice::MmcSupportsHs() {
  uint8_t device_type = raw_ext_csd_[MMC_EXT_CSD_DEVICE_TYPE];
  return (device_type & (1 << 1));
}

bool SdmmcBlockDevice::MmcSupportsHsDdr() {
  uint8_t device_type = raw_ext_csd_[MMC_EXT_CSD_DEVICE_TYPE];
  // Only support HSDDR @ 1.8V/3V
  return (device_type & (1 << 2));
}

bool SdmmcBlockDevice::MmcSupportsHs200() {
  uint8_t device_type = raw_ext_csd_[MMC_EXT_CSD_DEVICE_TYPE];
  // Only support HS200 @ 1.8V
  return (device_type & (1 << 4));
}

bool SdmmcBlockDevice::MmcSupportsHs400() {
  uint8_t device_type = raw_ext_csd_[MMC_EXT_CSD_DEVICE_TYPE];
  // Only support HS400 @ 1.8V
  return (device_type & (1 << 6));
}

zx_status_t SdmmcBlockDevice::ProbeMmcLocked(
    const fuchsia_hardware_sdmmc::SdmmcMetadata& metadata) {
  sdmmc_->SetRequestRetries(10);

  auto reset_retries = fit::defer([this]() { sdmmc_->SetRequestRetries(0); });

  // Query OCR
  bool removable = metadata.removable().value();
  zx::result<uint32_t> ocr = sdmmc_->MmcSendOpCond(/*suppress_error_messages=*/removable);
  if (ocr.is_error()) {
    if (removable) {
      // This error is expected if no card is inserted.
      FDF_LOGL(DEBUG, logger(), "MMC_SEND_OP_COND failed: %s", ocr.status_string());
    } else {
      FDF_LOGL(ERROR, logger(), "MMC_SEND_OP_COND failed: %s", ocr.status_string());
    }
    return ocr.status_value();
  }

  // Indicate support for sector mode addressing. Byte mode addressing is not implemented, which
  // effectively limits us to >2GB devices. The capacity is validated later when reading the CSD
  // register.
  *ocr = (*ocr & ~MMC_OCR_ACCESS_MODE_MASK) | MMC_OCR_SECTOR_MODE;

  zx_status_t st = sdmmc_->MmcWaitForReadyState(*ocr);
  if (st != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "MMC_SEND_OP_COND failed: %s", zx_status_get_string(st));
    return st;
  }

  // Get CID from card
  // Only supports 1 card currently so no need to loop
  if ((st = sdmmc_->MmcAllSendCid(raw_cid_)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "MMC_ALL_SEND_CID failed: %s", zx_status_get_string(st));
    return st;
  }
  FDF_LOGL(TRACE, logger(), "MMC_ALL_SEND_CID cid 0x%08x 0x%08x 0x%08x 0x%08x", raw_cid_[0],
           raw_cid_[1], raw_cid_[2], raw_cid_[3]);

  DecodeCid(raw_cid_, logger());

  // Set relative card address
  if ((st = sdmmc_->MmcSetRelativeAddr(1)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "MMC_SET_RELATIVE_ADDR failed: %s", zx_status_get_string(st));
    return st;
  }

  // Read CSD register
  std::array<uint8_t, SDMMC_CSD_SIZE> raw_csd;
  if ((st = sdmmc_->MmcSendCsd(raw_csd)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "MMC_SEND_CSD failed: %s", zx_status_get_string(st));
    return st;
  }

  if ((st = DecodeCsd(raw_csd, logger())) != ZX_OK) {
    return st;
  }

  // Select the card
  if ((st = sdmmc_->MmcSelectCard()) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "MMC_SELECT_CARD failed: %s", zx_status_get_string(st));
    return st;
  }

  // Read extended CSD register
  if ((st = sdmmc_->MmcSendExtCsd(raw_ext_csd_)) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "MMC_SEND_EXT_CSD failed: %s", zx_status_get_string(st));
    return st;
  }

  if ((st = MmcDecodeExtCsd()) != ZX_OK) {
    return st;
  }
  bus_width_ = SDMMC_BUS_WIDTH_ONE;

  // Switch to high-speed timing
  if (MmcSupportsHs() || MmcSupportsHsDdr() || MmcSupportsHs200()) {
    // Switch to 1.8V signal voltage
    sdmmc_voltage_t new_voltage = SDMMC_VOLTAGE_V180;
    if ((st = sdmmc_->SetSignalVoltage(new_voltage)) != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "failed to switch to 1.8V signalling: %s",
               zx_status_get_string(st));
      return st;
    }

    MmcSelectBusWidth();

    // Must perform tuning at HS200 first if HS400 is supported
    fuchsia_hardware_sdmmc::SdmmcHostPrefs speed_capabilities =
        metadata.speed_capabilities().value();
    if (MmcSupportsHs200() && bus_width_ != SDMMC_BUS_WIDTH_ONE &&
        !(speed_capabilities & fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs200)) {
      if ((st = MmcSwitchTiming(SDMMC_TIMING_HS200)) != ZX_OK) {
        return st;
      }

      if ((st = MmcSwitchFreq(kFreq200MHz)) != ZX_OK) {
        return st;
      }

      if ((st = sdmmc_->PerformTuning(MMC_SEND_TUNING_BLOCK)) != ZX_OK) {
        FDF_LOGL(ERROR, logger(), "tuning failed: %s", zx_status_get_string(st));
        return st;
      }

      if (MmcSupportsHs400() && bus_width_ == SDMMC_BUS_WIDTH_EIGHT &&
          !(speed_capabilities & fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs400)) {
        if ((st = MmcSwitchTimingHs200ToHs()) != ZX_OK) {
          return st;
        }

        if ((st = MmcSetBusWidth(SDMMC_BUS_WIDTH_EIGHT, MMC_EXT_CSD_BUS_WIDTH_8_DDR)) != ZX_OK) {
          return st;
        }

        if ((st = MmcSwitchTiming(SDMMC_TIMING_HS400)) != ZX_OK) {
          return st;
        }

        if ((st = MmcSwitchFreq(kFreq200MHz)) != ZX_OK) {
          return st;
        }
      }
    } else {
      if ((st = MmcSwitchTiming(SDMMC_TIMING_HS)) != ZX_OK) {
        return st;
      }

      if (MmcSupportsHsDdr() && (bus_width_ != SDMMC_BUS_WIDTH_ONE) &&
          !(speed_capabilities & fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHsddr)) {
        if ((st = MmcSwitchTiming(SDMMC_TIMING_HSDDR)) != ZX_OK) {
          return st;
        }

        uint8_t mmc_bus_width = (bus_width_ == SDMMC_BUS_WIDTH_FOUR) ? MMC_EXT_CSD_BUS_WIDTH_4_DDR
                                                                     : MMC_EXT_CSD_BUS_WIDTH_8_DDR;
        if ((st = MmcSetBusWidth(bus_width_, mmc_bus_width)) != ZX_OK) {
          return st;
        }
      }

      if ((st = MmcSwitchFreq(kFreq52MHz)) != ZX_OK) {
        return st;
      }
    }
  } else {
    // Set the bus frequency to legacy timing
    if ((st = MmcSwitchFreq(kFreq26MHz)) != ZX_OK) {
      return st;
    }
    timing_ = SDMMC_TIMING_LEGACY;
  }

  FDF_LOGL(INFO, logger(), "initialized mmc @ %u MHz, bus width %d, timing %d",
           clock_rate_ / 1000000, bus_width_, timing_);

  if (raw_ext_csd_[MMC_EXT_CSD_SEC_FEATURE_SUPPORT] &
      (0x1 << MMC_EXT_CSD_SEC_FEATURE_SUPPORT_SEC_GB_CL_EN)) {
    block_info_.flags |= FLAG_TRIM_SUPPORT;
  }

  if (GetCacheSizeBits(raw_ext_csd_) && metadata.enable_cache().value()) {
    // Enable the cache.
    st = MmcDoSwitch(MMC_EXT_CSD_CACHE_CTRL, MMC_EXT_CSD_CACHE_EN_MASK);
    if (st != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "Failed to enable the cache: %s", zx_status_get_string(st));
      return st;
    }
    // Read extended CSD register again to verify that the cache has been enabled.
    if ((st = sdmmc_->MmcSendExtCsd(raw_ext_csd_)) != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "MMC_SEND_EXT_CSD failed: %s", zx_status_get_string(st));
      return st;
    }
    if (!(raw_ext_csd_[MMC_EXT_CSD_CACHE_CTRL] & MMC_EXT_CSD_CACHE_EN_MASK)) {
      FDF_LOGL(ERROR, logger(), "Cache is unexpectedly disabled.");
      return ZX_ERR_BAD_STATE;
    }
    cache_enabled_ = true;
  } else {
    // The cache should be off by default upon device power-on. Check that this is the case.
    if (raw_ext_csd_[MMC_EXT_CSD_CACHE_CTRL] & MMC_EXT_CSD_CACHE_EN_MASK) {
      FDF_LOGL(ERROR, logger(), "Cache is unexpectedly enabled.");
      return ZX_ERR_BAD_STATE;
    }
  }

  if (removable) {
    block_info_.flags |= FLAG_REMOVABLE;
  }

  auto get_max_packed_commands_effective =
      [](uint32_t max_packed_commands, const fuchsia_hardware_sdmmc::SdmmcMetadata& metadata) {
        uint32_t max_packed_commands_effective =
            std::min(kMaxPackedCommandsFor512ByteBlockSize, max_packed_commands);
        return std::min(max_packed_commands_effective, metadata.max_command_packing().value());
      };
  max_packed_reads_effective_ =
      get_max_packed_commands_effective(raw_ext_csd_[MMC_EXT_CSD_MAX_PACKED_READS], metadata);
  max_packed_writes_effective_ =
      get_max_packed_commands_effective(raw_ext_csd_[MMC_EXT_CSD_MAX_PACKED_WRITES], metadata);
  if (max_packed_reads_effective_ > 1 || max_packed_writes_effective_ > 1) {
    const uint32_t buffer_region_count =
        std::max(max_packed_reads_effective_, max_packed_writes_effective_) +
        1;  // +1 for header block.
    st = readwrite_metadata_.InitForPackedCommands(buffer_region_count, block_info_.block_size);
    if (st != ZX_OK) {
      FDF_LOGL(ERROR, logger(), "Failed to initialize readwrite metadata for packed commands: %s",
               zx_status_get_string(st));
      return st;
    }
  }

  return ZX_OK;
}

void SdmmcBlockDevice::MmcSetInspectProperties() {
  const uint8_t type_a = std::min<uint8_t>(raw_ext_csd_[MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A],
                                           MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_INVALID);
  const uint8_t type_b = std::min<uint8_t>(raw_ext_csd_[MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B],
                                           MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_INVALID);
  uint8_t lifetime_max = std::max(type_a, type_b);
  if (lifetime_max >= MMC_EXT_CSD_DEVICE_LIFE_TIME_EST_INVALID) {
    // The device reported an invalid value for at least one of its lifetime estimates. Attempt to
    // report useful data by choosing the valid value, if there is one.
    lifetime_max = std::min(type_a, type_b);
  }

  properties_.type_a_lifetime_used_ = root_.CreateUint("type_a_lifetime_used", type_a);
  properties_.type_b_lifetime_used_ = root_.CreateUint("type_b_lifetime_used", type_b);
  properties_.max_lifetime_used_ = root_.CreateUint("max_lifetime_used", lifetime_max);
  properties_.cache_size_bits_ =
      root_.CreateUint("cache_size_bits", GetCacheSizeBits(raw_ext_csd_));
  properties_.cache_enabled_ = root_.CreateBool("cache_enabled", cache_enabled_);
  properties_.cache_flush_fifo_ =
      root_.CreateBool("cache_flush_fifo", raw_ext_csd_[MMC_EXT_CSD_CACHE_FLUSH_POLICY] &
                                               MMC_EXT_CSD_CACHE_FLUSH_POLICY_FIFO);
  properties_.barrier_supported_ =
      root_.CreateBool("barrier_supported",
                       raw_ext_csd_[MMC_EXT_CSD_BARRIER_SUPPORT] & MMC_EXT_CSD_BARRIER_SUPPORTED);
  properties_.trim_enabled_ =
      root_.CreateBool("trim_enabled", block_info_.flags & FLAG_TRIM_SUPPORT);
  properties_.max_packed_reads_ =
      root_.CreateUint("max_packed_reads", raw_ext_csd_[MMC_EXT_CSD_MAX_PACKED_READS]);
  properties_.max_packed_writes_ =
      root_.CreateUint("max_packed_writes", raw_ext_csd_[MMC_EXT_CSD_MAX_PACKED_WRITES]);
  properties_.max_packed_reads_effective_ =
      root_.CreateUint("max_packed_reads_effective", max_packed_reads_effective_);
  properties_.max_packed_writes_effective_ =
      root_.CreateUint("max_packed_writes_effective", max_packed_writes_effective_);
  properties_.using_fidl_ = root_.CreateBool("using_fidl", sdmmc_->using_fidl());
  properties_.power_suspended_ = root_.CreateBool("power_suspended", power_suspended_);
}

}  // namespace sdmmc
