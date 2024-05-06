// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/edid/edid.h"

#include <cmath>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <iterator>
#include <memory>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/logging/zxlogf.h"
#include "src/graphics/display/lib/edid/eisa_vid_lut.h"
#include "src/graphics/display/lib/edid/timings.h"

namespace {

template <typename T>
bool base_validate(const T* block) {
  static_assert(sizeof(T) == edid::kBlockSize, "Size check for Edid struct");

  const uint8_t* edid_bytes = reinterpret_cast<const uint8_t*>(block);
  if (edid_bytes[0] != T::kTag) {
    return false;
  }

  // The last byte of the 128-byte EDID data is a checksum byte which
  // should make the 128 bytes sum to zero.
  uint8_t sum = 0;
  for (uint32_t i = 0; i < edid::kBlockSize; ++i) {
    sum = static_cast<uint8_t>(sum + edid_bytes[i]);
  }
  return sum == 0;
}

}  // namespace

namespace edid {

const char* GetEisaVendorName(uint16_t manufacturer_name_code) {
  uint8_t c1 = static_cast<uint8_t>((((manufacturer_name_code >> 8) & 0x7c) >> 2) + 'A' - 1);
  uint8_t c2 = static_cast<uint8_t>(
      ((((manufacturer_name_code >> 8) & 0x03) << 3) | (manufacturer_name_code & 0xe0) >> 5) + 'A' -
      1);
  uint8_t c3 = static_cast<uint8_t>(((manufacturer_name_code & 0x1f)) + 'A' - 1);
  return lookup_eisa_vid(EISA_ID(c1, c2, c3));
}

bool BaseEdid::validate() const {
  static const uint8_t kEdidHeader[8] = {0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0};
  return base_validate<BaseEdid>(this) && memcmp(header, kEdidHeader, sizeof(kEdidHeader)) == 0;
}

bool CeaEdidTimingExtension::validate() const {
  if (!(dtd_start_idx <= sizeof(payload) && base_validate<CeaEdidTimingExtension>(this))) {
    return false;
  }

  // If this is zero, there is no DTDs present and no non-DTD data.
  if (dtd_start_idx == 0) {
    return true;
  }

  if (dtd_start_idx > 0 && dtd_start_idx < offsetof(CeaEdidTimingExtension, payload)) {
    return false;
  }

  size_t offset = 0;
  size_t dbc_end = dtd_start_idx - offsetof(CeaEdidTimingExtension, payload);
  while (offset < dbc_end) {
    const DataBlock* data_block = reinterpret_cast<const DataBlock*>(payload + offset);
    offset += (1 + data_block->length());  // Length doesn't include the header
    // Check that the block doesn't run past the end if the dbc
    if (offset > dbc_end) {
      return false;
    }
  }
  return true;
}

ReadEdidResult ReadEdidFromDdcForTesting(void* ctx, ddc_i2c_transact transact) {
  uint8_t segment_address = 0;
  uint8_t segment_offset = 0;
  ddc_i2c_msg_t msgs[3] = {
      {.is_read = false, .addr = kDdcSegmentI2cAddress, .buf = &segment_address, .length = 1},
      {.is_read = false, .addr = kDdcDataI2cAddress, .buf = &segment_offset, .length = 1},
      {.is_read = true, .addr = kDdcDataI2cAddress, .buf = nullptr, .length = kBlockSize},
  };

  BaseEdid base_edid;
  msgs[2].buf = reinterpret_cast<uint8_t*>(&base_edid);

  // The VESA E-DDC standard claims that the segment pointer is reset to its
  // default value (00h) at the completion of each command sequence.
  // (Section 2.2.5 "Segment Pointer", Page 18, VESA E-DDC Standard,
  //  Version 1.3)
  //
  // Note that we are not following the recommended reading patterns in the
  // E-DDC standard, which requires drivers always issue segment writes for
  // each read and ignore the NACKs (Section 5.1.1 "Basic Operation for E-DDC
  // Access of EDID", S 6.5 "E-DDC Sequential Read", VESA E-DDC Standard,
  // Version 1.3). Instead, when reading the first block (base EDID), we skip
  // writing the segment address register and rely on display devices' reset
  // mechanism.
  //
  // This makes the following EDID read procedure compatible with display
  // devices that don't support Enhanced DDC standard; otherwise these devices
  // will issue NACKs and some display controllers (e.g. Intel HD Display)
  // might not be able to handle it correctly. It is possible that drivers may
  // fail connecting to monitors that only recognizes some fixed structures or
  // I2C command patterns (segment write always precedes data read), though the
  // chance is rare.
  if (!transact(ctx, msgs + 1, 2)) {
    return ReadEdidResult::MakeError("Failed to read base edid");
  }
  if (!base_edid.validate()) {
    return ReadEdidResult::MakeError("Failed to validate base edid");
  }

  uint16_t edid_length = static_cast<uint16_t>((base_edid.num_extensions + 1) * kBlockSize);

  fbl::AllocChecker ac;
  fbl::Vector<uint8_t> edid;
  edid.resize(edid_length, &ac);
  if (!ac.check()) {
    return ReadEdidResult::MakeError("Failed to allocate edid storage");
  }

  memcpy(edid.data(), reinterpret_cast<void*>(&base_edid), kBlockSize);
  for (uint8_t i = 1; i && i <= base_edid.num_extensions; i++) {
    segment_address = i / 2;
    segment_offset = i % 2 ? kBlockSize : 0;
    msgs[2].buf = edid.data() + i * kBlockSize;

    // The segment pointer is reset to zero every time after a command sequence.
    // As long as the segment number is not zero, we should issue a DDC segment
    // read before we read / write a piece of data.
    bool include_segment = segment_address != 0;

    bool transact_success = include_segment ? transact(ctx, msgs, 3) : transact(ctx, msgs + 1, 2);
    if (!transact_success) {
      return ReadEdidResult::MakeError("Failed to read full edid");
    }
  }

  return ReadEdidResult::MakeEdidBytes(std::move(edid));
}

bool Edid::Init(void* ctx, ddc_i2c_transact transact, const char** err_msg) {
  auto read_edid_result = ReadEdidFromDdcForTesting(ctx, transact);
  if (read_edid_result.is_error) {
    ZX_DEBUG_ASSERT(read_edid_result.error_message != nullptr);
    *err_msg = read_edid_result.error_message;
    return false;
  }
  edid_bytes_ = std::move(read_edid_result.edid_bytes);
  return Init(edid_bytes_.data(), static_cast<uint16_t>(edid_bytes_.size()), err_msg);
}

bool Edid::Init(const uint8_t* bytes, uint16_t len, const char** err_msg) {
  // The maximum size of an edid is 255 * 128 bytes, so any 16 bit multiple is fine.
  if (len == 0 || len % kBlockSize != 0) {
    *err_msg = "Invalid edid length";
    return false;
  }
  bytes_ = bytes;
  len_ = len;
  if (!(base_edid_ = GetBlock<BaseEdid>(0)) || !base_edid_->validate()) {
    *err_msg = "Failed to validate base edid";
    return false;
  }
  if (((base_edid_->num_extensions + 1) * kBlockSize) != len) {
    *err_msg = "Bad extension count";
    return false;
  }
  if (!base_edid_->digital()) {
    *err_msg = "Analog displays not supported";
    return false;
  }

  for (uint8_t i = 1; i < len / kBlockSize; i++) {
    if (bytes_[i * kBlockSize] == CeaEdidTimingExtension::kTag) {
      if (!GetBlock<CeaEdidTimingExtension>(i)->validate()) {
        *err_msg = "Failed to validate extensions";
        return false;
      }
    }
  }

  monitor_serial_[0] = monitor_name_[0] = '\0';
  for (auto it = descriptor_iterator(this); it.is_valid(); ++it) {
    char* dest;
    if (it->timing.pixel_clock_10khz != 0) {
      continue;
    } else if (it->monitor.type == Descriptor::Monitor::kName) {
      dest = monitor_name_;
    } else if (it->monitor.type == Descriptor::Monitor::kSerial) {
      dest = monitor_serial_;
    } else {
      continue;
    }

    // Look for '\n' if it exists, otherwise take the whole string.
    uint32_t len;
    for (len = 0; len < sizeof(Descriptor::Monitor::data) && it->monitor.data[len] != 0x0A; ++len) {
      // Empty body
    }

    // Copy the string and remember to null-terminate.
    memcpy(dest, it->monitor.data, len);
    dest[len] = '\0';
  }

  // If we didn't find a valid serial descriptor, use the base serial number
  if (monitor_serial_[0] == '\0') {
    sprintf(monitor_serial_, "%d", base_edid_->serial_number);
  }

  uint8_t c1 = static_cast<uint8_t>(((base_edid_->manufacturer_id1 & 0x7c) >> 2) + 'A' - 1);
  uint8_t c2 = static_cast<uint8_t>(
      (((base_edid_->manufacturer_id1 & 0x03) << 3) | (base_edid_->manufacturer_id2 & 0xe0) >> 5) +
      'A' - 1);
  uint8_t c3 = static_cast<uint8_t>(((base_edid_->manufacturer_id2 & 0x1f)) + 'A' - 1);

  manufacturer_id_[0] = c1;
  manufacturer_id_[1] = c2;
  manufacturer_id_[2] = c3;
  manufacturer_id_[3] = '\0';
  manufacturer_name_ = lookup_eisa_vid(EISA_ID(c1, c2, c3));

  return true;
}

template <typename T>
const T* Edid::GetBlock(uint8_t block_num) const {
  const uint8_t* bytes = bytes_ + block_num * kBlockSize;
  return bytes[0] == T::kTag ? reinterpret_cast<const T*>(bytes) : nullptr;
}

bool Edid::is_hdmi() const {
  data_block_iterator dbs(this);
  if (!dbs.is_valid() || dbs.cea_revision() < 0x03) {
    return false;
  }

  do {
    if (dbs->type() == VendorSpecificBlock::kType) {
      // HDMI's 24-bit IEEE registration is 0x000c03 - vendor_number is little endian
      if (dbs->payload.vendor.vendor_number[0] == 0x03 &&
          dbs->payload.vendor.vendor_number[1] == 0x0c &&
          dbs->payload.vendor.vendor_number[2] == 0x00) {
        return true;
      }
    }
  } while ((++dbs).is_valid());
  return false;
}

display::DisplayTiming DetailedTimingDescriptorToDisplayTiming(
    const DetailedTimingDescriptor& dtd) {
  // A valid DetailedTimingDescriptor guarantees that
  // horizontal_blanking >= horizontal_front_porch + horizontal_sync_pulse, and
  // all of them are non-negative and fit in [0, kMaxTimingValue].
  //
  // This constraint guarantees that
  // horizontal_blanking - (horizontal_front_porch + horizontal_sync_pulse)
  // will also fit in [0, kMaxTimingValue]; the calculation won't overflow,
  // causing undefined behaviors.
  int32_t horizontal_back_porch_px =
      static_cast<int32_t>(dtd.horizontal_blanking() -
                           (dtd.horizontal_front_porch() + dtd.horizontal_sync_pulse_width()));

  // A similar argument holds for the vertical back porch.
  int32_t vertical_back_porch_lines = static_cast<int32_t>(
      dtd.vertical_blanking() - (dtd.vertical_front_porch() + dtd.vertical_sync_pulse_width()));

  if (dtd.type() != TYPE_DIGITAL_SEPARATE) {
    // TODO(https://fxbug.dev/42086615): Displays using composite syncs are not
    // supported. We treat them as if they were using separate sync signals.
    zxlogf(WARNING, "The detailed timing descriptor uses composite sync; this is not supported.");
  }

  return display::DisplayTiming{
      .horizontal_active_px = static_cast<int32_t>(dtd.horizontal_addressable()),
      .horizontal_front_porch_px = static_cast<int32_t>(dtd.horizontal_front_porch()),
      .horizontal_sync_width_px = static_cast<int32_t>(dtd.horizontal_sync_pulse_width()),
      .horizontal_back_porch_px = horizontal_back_porch_px,

      .vertical_active_lines = static_cast<int32_t>(dtd.vertical_addressable()),
      .vertical_front_porch_lines = static_cast<int32_t>(dtd.vertical_front_porch()),
      .vertical_sync_width_lines = static_cast<int32_t>(dtd.vertical_sync_pulse_width()),
      .vertical_back_porch_lines = vertical_back_porch_lines,

      .pixel_clock_frequency_hz = int64_t{dtd.pixel_clock_10khz} * 10'000,
      .fields_per_frame = dtd.interlaced() ? display::FieldsPerFrame::kInterlaced
                                           : display::FieldsPerFrame::kProgressive,
      .hsync_polarity = dtd.hsync_polarity() ? display::SyncPolarity::kPositive
                                             : display::SyncPolarity::kNegative,
      .vsync_polarity = dtd.vsync_polarity() ? display::SyncPolarity::kPositive
                                             : display::SyncPolarity::kNegative,
      .vblank_alternates = false,
      .pixel_repetition = 0,
  };
}

// Returns std::nullopt if the standard timing descriptor `std` is invalid or
// unsupported.
// Otherwise, returns the DisplayTiming converted from the descriptor.
std::optional<display::DisplayTiming> StandardTimingDescriptorToDisplayTiming(
    const BaseEdid& edid, const StandardTimingDescriptor& std) {
  // Pick the largest resolution advertised by the display and then use the
  // generalized timing formula to compute the timing parameters.
  // TODO(stevensd): Support interlaced modes and margins
  int32_t width = static_cast<int32_t>(std.horizontal_resolution());
  int32_t height =
      static_cast<int32_t>(std.vertical_resolution(edid.edid_version, edid.edid_revision));
  int32_t v_rate = static_cast<int32_t>(std.vertical_freq()) + 60;

  if (!width || !height || !v_rate) {
    zxlogf(WARNING, "Invalid standard timing descriptor: %" PRId32 " x %" PRId32 "@ %" PRId32 " Hz",
           width, height, v_rate);
    return std::nullopt;
  }

  for (const display::DisplayTiming& dmt_timing : internal::kDmtDisplayTimings) {
    if (dmt_timing.horizontal_active_px == width && dmt_timing.vertical_active_lines == height &&
        ((dmt_timing.vertical_field_refresh_rate_millihertz() + 500) / 1000) == v_rate) {
      return dmt_timing;
    }
  }

  zxlogf(WARNING,
         "This EDID contains a non-DMT standard timing (%" PRIu32 "x%" PRIu32 " @%" PRIu32
         "Hz). The timing is not supported and will be ignored. See https://fxbug.dev/42085380 for "
         "details.",
         width, height, v_rate);
  return std::nullopt;
}

timing_iterator& timing_iterator::operator++() {
  while (state_ != kDone) {
    Advance();
    // If either of these are 0, then the timing value is definitely wrong
    if (display_timing_.vertical_active_lines != 0 && display_timing_.horizontal_active_px != 0) {
      break;
    }
  }
  return *this;
}

void timing_iterator::Advance() {
  if (state_ == kDtds) {
    while (descriptors_.is_valid()) {
      if (descriptors_->timing.pixel_clock_10khz != 0) {
        display_timing_ = DetailedTimingDescriptorToDisplayTiming(descriptors_->timing);
        ++descriptors_;
        return;
      }
      ++descriptors_;
    }
    state_ = kSvds;
    state_index_ = UINT16_MAX;
  }

  if (state_ == kSvds) {
    while (dbs_.is_valid()) {
      if (dbs_->type() == ShortVideoDescriptor::kType) {
        state_index_++;
        uint32_t modes_to_skip = state_index_;
        for (unsigned i = 0; i < dbs_->length(); i++) {
          uint32_t idx = dbs_->payload.video[i].standard_mode_idx() - 1;
          if (idx >= internal::kCtaDisplayTimings.size()) {
            continue;
          }
          if (modes_to_skip == 0) {
            display_timing_ = internal::kCtaDisplayTimings[idx];
            return;
          }

          // For timings with refresh rates that are multiples of 6, there are
          // corresponding timings adjusted by a factor of 1000/1001.
          //
          // TODO(https://fxbug.dev/42086617): Revise the refresh rate adjustment
          // logic to make sure that it complies with the CTA-861 standards.
          uint32_t rounded_refresh =
              (internal::kCtaDisplayTimings[idx].vertical_field_refresh_rate_millihertz() + 999) /
              1000;
          if (rounded_refresh % 6 == 0) {
            if (modes_to_skip == 1) {
              display_timing_ = internal::kCtaDisplayTimings[idx];
              // `pixel_clock_frequency_hz` is less than 2^41, and `double` has
              // 51 fractional bits, so `pixel_clock_frequency_hz` can be
              // converted to a double value without losing precision.
              double clock = static_cast<double>(display_timing_.pixel_clock_frequency_hz);
              // 240/480 height entries are already multiplied by 1000/1001
              double mult = display_timing_.vertical_active_lines == 240 ||
                                    display_timing_.vertical_active_lines == 480
                                ? 1.001
                                : (1000. / 1001.);
              display_timing_.pixel_clock_frequency_hz = static_cast<int64_t>(round(clock * mult));
              return;
            }
            modes_to_skip -= 2;
          } else {
            modes_to_skip--;
          }
        }
      }

      ++dbs_;
      // Reset the index for either the next SVD block or the STDs.
      state_index_ = UINT16_MAX;
    }

    state_ = kStds;
  }

  if (state_ == kStds) {
    while (++state_index_ < std::size(edid_->base_edid_->standard_timings)) {
      const StandardTimingDescriptor* desc = edid_->base_edid_->standard_timings + state_index_;
      if (desc->byte1 == 0x01 && desc->byte2 == 0x01) {
        continue;
      }
      std::optional<display::DisplayTiming> display_timing =
          StandardTimingDescriptorToDisplayTiming(*edid_->base_edid_, *desc);
      if (display_timing.has_value()) {
        display_timing_ = *display_timing;
      }
      return;
    }

    state_ = kDone;
  }
}

audio_data_block_iterator& audio_data_block_iterator::operator++() {
  while (dbs_.is_valid()) {
    uint32_t num_sads = static_cast<uint32_t>(dbs_->length() / sizeof(ShortAudioDescriptor));
    if (dbs_->type() != ShortAudioDescriptor::kType || ++sad_idx_ > num_sads) {
      ++dbs_;
      sad_idx_ = UINT8_MAX;
      continue;
    }
    descriptor_ = dbs_->payload.audio[sad_idx_];
    return *this;
  }

  edid_ = nullptr;
  return *this;
}

Edid::descriptor_iterator& Edid::descriptor_iterator::operator++() {
  if (!edid_) {
    return *this;
  }

  if (block_idx_ == 0) {
    descriptor_idx_++;

    if (descriptor_idx_ < std::size(edid_->base_edid_->detailed_descriptors)) {
      descriptor_ = edid_->base_edid_->detailed_descriptors + descriptor_idx_;
      if (descriptor_->timing.pixel_clock_10khz != 0 || descriptor_->monitor.type != 0x10) {
        return *this;
      }
    }

    block_idx_++;
    descriptor_idx_ = UINT32_MAX;
  }

  while (block_idx_ < (edid_->len_ / kBlockSize)) {
    auto cea_extn_block = edid_->GetBlock<CeaEdidTimingExtension>(block_idx_);
    size_t offset = sizeof(CeaEdidTimingExtension::payload);
    if (cea_extn_block &&
        cea_extn_block->dtd_start_idx > offsetof(CeaEdidTimingExtension, payload)) {
      offset = cea_extn_block->dtd_start_idx - offsetof(CeaEdidTimingExtension, payload);
    }

    descriptor_idx_++;
    offset += sizeof(Descriptor) * descriptor_idx_;

    // Return if the descriptor is within bounds and either a timing descriptor or not
    // a dummy monitor descriptor, otherwise advance to the next block
    if (offset + sizeof(DetailedTimingDescriptor) <= sizeof(CeaEdidTimingExtension::payload)) {
      descriptor_ = reinterpret_cast<const Descriptor*>(cea_extn_block->payload + offset);
      if (descriptor_->timing.pixel_clock_10khz != 0 ||
          descriptor_->monitor.type != Descriptor::Monitor::kDummyType) {
        return *this;
      }
    }

    block_idx_++;
    descriptor_idx_ = UINT32_MAX;
  }

  edid_ = nullptr;
  return *this;
}

Edid::data_block_iterator::data_block_iterator(const Edid* edid) : edid_(edid) {
  ++(*this);
  if (is_valid()) {
    cea_revision_ = edid_->GetBlock<CeaEdidTimingExtension>(block_idx_)->revision_number;
  }
}

Edid::data_block_iterator& Edid::data_block_iterator::operator++() {
  if (!edid_) {
    return *this;
  }

  while (block_idx_ < (edid_->len_ / kBlockSize)) {
    auto cea_extn_block = edid_->GetBlock<CeaEdidTimingExtension>(block_idx_);
    size_t dbc_end = 0;
    if (cea_extn_block &&
        cea_extn_block->dtd_start_idx > offsetof(CeaEdidTimingExtension, payload)) {
      dbc_end = cea_extn_block->dtd_start_idx - offsetof(CeaEdidTimingExtension, payload);
    }

    db_idx_++;
    uint32_t db_to_skip = db_idx_;

    uint32_t offset = 0;
    while (offset < dbc_end) {
      auto* dblk = reinterpret_cast<const DataBlock*>(cea_extn_block->payload + offset);
      if (db_to_skip == 0) {
        db_ = dblk;
        return *this;
      }
      db_to_skip--;
      offset += (dblk->length() + 1);  // length doesn't include the data block header byte
    }

    block_idx_++;
    db_idx_ = UINT32_MAX;
  }

  edid_ = nullptr;
  return *this;
}

void Edid::Print(void (*print_fn)(const char* str)) const {
  char str_buf[128];
  print_fn("Raw edid:\n");
  for (auto i = 0; i < edid_length(); i++) {
    constexpr int kBytesPerLine = 16;
    char* b = str_buf;
    if (i % kBytesPerLine == 0) {
      b += sprintf(b, "%04x: ", i);
    }
    sprintf(b, "%02x%s", edid_bytes()[i], i % kBytesPerLine == kBytesPerLine - 1 ? "\n" : " ");
    print_fn(str_buf);
  }
}

bool Edid::supports_basic_audio() const {
  uint8_t block_idx = 1;  // Skip block 1, since it can't be a CEA block
  while (block_idx < (len_ / kBlockSize)) {
    auto cea_extn_block = GetBlock<CeaEdidTimingExtension>(block_idx);
    if (cea_extn_block && cea_extn_block->revision_number >= 2) {
      return cea_extn_block->basic_audio();
    }
    block_idx++;
  }
  return false;
}

}  // namespace edid
