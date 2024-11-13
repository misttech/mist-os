// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/edid/edid.h"

#include <lib/driver/logging/cpp/logger.h>
#include <lib/fit/result.h>
#include <zircon/assert.h>

#include <cmath>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <iterator>
#include <memory>
#include <sstream>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>
#include <fbl/string_buffer.h>

#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
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

namespace {

// Unpacks the ID Manufacturer Name Field specified in the base EDID.
//
// The ID Manufacturer name is a 3-character code containing three upper case
// letters. They are encoded in the base EDID byte 08h and 09h based on a 5-bit
// compressed ASCII code.
//
// E-EDID standard Section 3.4.1 "ID Manufacture Name", page 21.
std::string UnpackIdManufacturerName(uint8_t byte_08h, uint8_t byte_09h) {
  int compressed_character1 = (byte_08h & 0b01111100) >> 2;
  int compressed_character2 = ((byte_08h & 0b00000011) << 3) | ((byte_09h & 0b11100000) >> 5);
  int compressed_character3 = byte_09h & 0b0011111;

  // Some EDIDs may contain invalid manufacturer name codes. We replace the
  // invalid characters with the fallback character 'A'.
  if (compressed_character1 < 1 || compressed_character1 > 26) {
    FDF_LOG(WARNING, "Invalid manufacturer name code character #1: %d", compressed_character1);
    compressed_character1 = 1;
  }
  if (compressed_character2 < 1 || compressed_character2 > 26) {
    FDF_LOG(WARNING, "Invalid manufacturer name code character #2: %d", compressed_character2);
    compressed_character2 = 1;
  }
  if (compressed_character3 < 1 || compressed_character3 > 26) {
    FDF_LOG(WARNING, "Invalid manufacturer name code character #3: %d", compressed_character3);
    compressed_character3 = 1;
  }

  // The cast won't overflow because the compressed_character values are
  // guaranteed to be in the range [1, 26].
  const char characters[3] = {
      static_cast<char>(compressed_character1 + 'A' - 1),
      static_cast<char>(compressed_character2 + 'A' - 1),
      static_cast<char>(compressed_character3 + 'A' - 1),
  };
  return std::string(characters, std::size(characters));
}

bool IsDisplayDescriptor(const Descriptor& descriptor) {
  // A Descriptor can be either a Detailed Timing Descriptor or a Display
  // Descriptor. For Display Descriptors, its first two bytes must be 0x0000,
  // while for Detailed Timing Descriptors the first two bytes
  // (`pixel_clock_10khz`) must not be 0x0000.
  //
  // Accessing any field within the union before figuring out its underlying
  // type is an undefined behavior in C++. So we use reinterpret_cast to access
  // its first two bytes.
  const uint8_t* descriptor_bytes = reinterpret_cast<const uint8_t*>(&descriptor);

  // sizeof(Descriptor) > 2, so it's always valid to access the first two bytes.
  const uint8_t descriptor_type_indicator_low_byte = *descriptor_bytes;
  const uint8_t descriptor_type_indicator_high_byte = *(descriptor_bytes + 1);
  return descriptor_type_indicator_low_byte == 0x00 && descriptor_type_indicator_high_byte == 0x00;
}

}  // namespace

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
    return fit::error("Failed to read base edid");
  }
  if (!base_edid.validate()) {
    return fit::error("Failed to validate base edid");
  }

  uint16_t edid_length = static_cast<uint16_t>((base_edid.num_extensions + 1) * kBlockSize);

  fbl::AllocChecker ac;
  fbl::Vector<uint8_t> edid;
  edid.resize(edid_length, &ac);
  if (!ac.check()) {
    return fit::error("Failed to allocate edid storage");
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
      return fit::error("Failed to read full edid");
    }
  }

  return fit::ok(std::move(edid));
}

// static
fit::result<const char*, Edid> Edid::Create(void* ctx, ddc_i2c_transact transact) {
  auto read_edid_result = ReadEdidFromDdcForTesting(ctx, transact);
  if (read_edid_result.is_error()) {
    return read_edid_result.take_error();
  }
  fbl::Vector<uint8_t> bytes = std::move(read_edid_result).value();

  if (bytes.is_empty()) {
    return fit::error("EDID is empty");
  }
  if (bytes.size() % kBlockSize != 0) {
    return fit::error("EDID size is not a multiple of block size");
  }
  static constexpr int kMaxAllowedEdidBytesSize = kBlockSize * kMaxEdidBlockCount;
  if (bytes.size() > kMaxAllowedEdidBytesSize) {
    return fit::error("EDID size exceeds the maximum allowed size");
  }

  Edid edid(std::move(bytes));
  fit::result<const char*> validate_result = edid.Validate();
  if (validate_result.is_error()) {
    return validate_result.take_error();
  }
  return fit::ok(std::move(edid));
}

// static
fit::result<const char*, Edid> Edid::Create(cpp20::span<const uint8_t> bytes) {
  if (bytes.empty()) {
    return fit::error("EDID is empty");
  }
  if (bytes.size() % kBlockSize != 0) {
    return fit::error("EDID size is not a multiple of block size");
  }
  static constexpr int kMaxAllowedEdidBytesSize = kBlockSize * kMaxEdidBlockCount;
  if (bytes.size() > kMaxAllowedEdidBytesSize) {
    return fit::error("EDID size exceeds the maximum allowed size");
  }

  fbl::Vector<uint8_t> bytes_vec;
  fbl::AllocChecker alloc_checker;
  bytes_vec.resize(bytes.size(), 0, &alloc_checker);
  if (!alloc_checker.check()) {
    return fit::error("Failed to allocate memory for EDID");
  }
  std::copy(bytes.begin(), bytes.end(), bytes_vec.begin());

  Edid edid(std::move(bytes_vec));
  fit::result<const char*> validate_result = edid.Validate();
  if (validate_result.is_error()) {
    return validate_result.take_error();
  }
  return fit::ok(std::move(edid));
}

Edid::Edid(fbl::Vector<uint8_t> bytes) : bytes_(std::move(bytes)) {
  ZX_DEBUG_ASSERT(!bytes_.is_empty());
  ZX_DEBUG_ASSERT(bytes_.size() % kBlockSize == 0);
  ZX_DEBUG_ASSERT(bytes_.size() <= size_t{kBlockSize} * kMaxEdidBlockCount);
}

std::string Edid::GetManufacturerId() const {
  const BaseEdid& base = base_edid();
  return UnpackIdManufacturerName(base.manufacturer_id1, base.manufacturer_id2);
}

const char* Edid::GetManufacturerName() const {
  std::string manufacturer_id = GetManufacturerId();
  return lookup_eisa_vid(EISA_ID(manufacturer_id[0], manufacturer_id[1], manufacturer_id[2]));
}

std::string Edid::GetDisplayProductName() const {
  for (auto it = internal::descriptor_iterator(this); it.is_valid(); ++it) {
    const Descriptor* descriptor_raw = it.get();
    ZX_DEBUG_ASSERT(descriptor_raw != nullptr);

    // `descriptor` may not be aligned correctly. Copy the bytes to a locally-
    // constructed `Descriptor` first.
    Descriptor descriptor;
    memcpy(&descriptor, descriptor_raw, sizeof(Descriptor));

    if (!IsDisplayDescriptor(descriptor)) {
      continue;
    }

    const Descriptor::Monitor& display_descriptor = descriptor.monitor;
    if (display_descriptor.type == Descriptor::Monitor::kName) {
      std::string_view name(reinterpret_cast<const char*>(display_descriptor.data),
                            std::size(display_descriptor.data));

      // The E-EDID standard requires that the display product name data string
      // is terminated with ASCII code 0Ah (line feed) if there are less than
      // 13 characters in the string. Thus, we truncate the string at the first
      // line feed character.
      //
      // E-EDID standard, Section 3.10.3.4 "Display Product Name (ASCII) String
      // Descriptor Definition", page 44.
      static constexpr char kStringTerminatorCharacter = 0x0a;
      size_t terminator_pos = name.find(kStringTerminatorCharacter);
      std::string_view actual_name = name.substr(/*pos=*/0, /*n=*/terminator_pos);

      return std::string(actual_name);
    }
  }

  // No display product name is provided in the Display Descriptors. Return
  // an empty string.
  return {};
}

std::string Edid::GetDisplayProductSerialNumber() const {
  for (auto it = internal::descriptor_iterator(this); it.is_valid(); ++it) {
    const Descriptor* descriptor_raw = it.get();
    ZX_DEBUG_ASSERT(descriptor_raw != nullptr);

    // `descriptor` may not be aligned correctly. Copy the bytes to a locally-
    // constructed `Descriptor` first.
    Descriptor descriptor;
    memcpy(&descriptor, descriptor_raw, sizeof(Descriptor));

    if (!IsDisplayDescriptor(descriptor)) {
      continue;
    }

    const Descriptor::Monitor& display_descriptor = descriptor.monitor;
    if (display_descriptor.type == Descriptor::Monitor::kSerial) {
      // The E-EDID standard requires that the serial number data string must be
      // ASCII-encoded. So `data` can be directly casted to a string_view.
      //
      // E-EDID standard, Section 3.10.3.1 "Display Product Serial Number
      // Descriptor Definition", page 38.
      std::string_view serial_number(reinterpret_cast<const char*>(display_descriptor.data),
                                     std::size(display_descriptor.data));

      // The E-EDID standard requires that the serial number data string must be
      // terminated with ASCII code 0Ah (line feed) if there are less than 13
      // characters in the string.
      //
      // E-EDID standard, Section 3.10.3.1 "Display Product Serial Number
      // Descriptor Definition", page 38.
      static constexpr char kStringTerminatorCharacter = 0x0a;
      size_t terminator_pos = serial_number.find(kStringTerminatorCharacter);
      std::string_view actual_serial_number = serial_number.substr(/*pos=*/0, /*n=*/terminator_pos);

      return std::string(actual_serial_number);
    }
  }

  // No display product serial number is provided in the Display Descriptors.
  // Fall back to the "ID Serial Number" field defined in the base EDID.
  //
  // E-EDID standard, Section 3.4.3 "ID Serial Number", page 22.
  std::ostringstream fallback_serial_number;
  const BaseEdid& base = base_edid();

  fallback_serial_number << base.serial_number;
  return fallback_serial_number.str();
}

fit::result<const char*> Edid::Validate() {
  const BaseEdid& base = base_edid();
  if (!base.validate()) {
    return fit::error("Failed to validate base edid");
  }
  if (((base.num_extensions + 1) * kBlockSize) != bytes_.size()) {
    return fit::error("Bad extension count");
  }
  if (!base.digital()) {
    return fit::error("Analog displays not supported");
  }

  for (uint8_t i = 1; i < bytes_.size() / kBlockSize; i++) {
    if (bytes_[i * kBlockSize] == CeaEdidTimingExtension::kTag) {
      if (!GetBlock<CeaEdidTimingExtension>(i)->validate()) {
        return fit::error("Failed to validate extensions");
      }
    }
  }
  return fit::ok();
}

int Edid::horizontal_size_mm() const {
  static constexpr int kMillimetersPerCentimeter = 10;
  // The multiplication result meets the API contract because
  // `horizontal_size_cm` is at most 255.
  return int{base_edid().horizontal_size_cm} * kMillimetersPerCentimeter;
}

int Edid::vertical_size_mm() const {
  static constexpr int kMillimetersPerCentimeter = 10;
  // The multiplication result meets the API contract because
  // `vertical_size_cm` is at most 255.
  return int{base_edid().vertical_size_cm} * kMillimetersPerCentimeter;
}

bool Edid::is_hdmi() const {
  internal::data_block_iterator dbs(this);
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
    FDF_LOG(WARNING, "The detailed timing descriptor uses composite sync; this is not supported.");
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
    FDF_LOG(WARNING,
            "Invalid standard timing descriptor: %" PRId32 " x %" PRId32 "@ %" PRId32 " Hz", width,
            height, v_rate);
    return std::nullopt;
  }

  for (const display::DisplayTiming& dmt_timing : internal::kDmtDisplayTimings) {
    if (dmt_timing.horizontal_active_px == width && dmt_timing.vertical_active_lines == height &&
        ((dmt_timing.vertical_field_refresh_rate_millihertz() + 500) / 1000) == v_rate) {
      return dmt_timing;
    }
  }

  FDF_LOG(
      WARNING,
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
    while (++state_index_ < std::size(edid_->base_edid().standard_timings)) {
      const StandardTimingDescriptor* desc = edid_->base_edid().standard_timings + state_index_;
      if (desc->byte1 == 0x01 && desc->byte2 == 0x01) {
        continue;
      }
      std::optional<display::DisplayTiming> display_timing =
          StandardTimingDescriptorToDisplayTiming(edid_->base_edid(), *desc);
      if (display_timing.has_value()) {
        display_timing_ = *display_timing;
      }
      return;
    }

    state_ = kDone;
  }
}

void Edid::Print(void (*print_fn)(const char* str)) const {
  char str_buf[128];
  print_fn("Raw edid:\n");
  for (size_t i = 0; i < edid_length(); i++) {
    constexpr int kBytesPerLine = 16;
    char* b = str_buf;
    if (i % kBytesPerLine == 0) {
      b += sprintf(b, "%04zx: ", i);
    }
    sprintf(b, "%02x%s", edid_bytes()[i], i % kBytesPerLine == kBytesPerLine - 1 ? "\n" : " ");
    print_fn(str_buf);
  }
}

bool Edid::supports_basic_audio() const {
  uint8_t block_idx = 1;  // Skip block 1, since it can't be a CEA block
  const int num_blocks = static_cast<int>(bytes_.size() / kBlockSize);
  while (block_idx < num_blocks) {
    auto cea_extn_block = GetBlock<CeaEdidTimingExtension>(block_idx);
    if (cea_extn_block && cea_extn_block->revision_number >= 2) {
      return cea_extn_block->basic_audio();
    }
    block_idx++;
  }
  return false;
}

}  // namespace edid
