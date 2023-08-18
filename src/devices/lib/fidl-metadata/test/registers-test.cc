// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/lib/fidl-metadata/registers.h"

#include <fidl/fuchsia.hardware.registers/cpp/wire.h>

#include <zxtest/zxtest.h>

template <typename T>
static void check_encodes(
    const cpp20::span<const fidl_metadata::registers::Register<T>> register_entries) {
  // Encode.
  auto result = fidl_metadata::registers::RegistersMetadataToFidl(register_entries);
  ASSERT_OK(result.status_value());
  std::vector<uint8_t>& data = result.value();

  // Decode.
  fit::result decoded =
      fidl::InplaceUnpersist<fuchsia_hardware_registers::wire::Metadata>(cpp20::span(data));
  ASSERT_TRUE(decoded.is_ok(), "%s", decoded.error_value().FormatDescription().c_str());

  auto metadata = *decoded.value();

  // Check everything looks sensible.
  ASSERT_TRUE(metadata.has_registers());
  auto registers = metadata.registers();
  ASSERT_EQ(registers.count(), register_entries.size());

  for (size_t i = 0; i < register_entries.size(); i++) {
    ASSERT_TRUE(registers[i].has_bind_id());
    ASSERT_EQ(registers[i].bind_id(), register_entries[i].bind_id);

    ASSERT_TRUE(registers[i].has_mmio_id());
    ASSERT_EQ(registers[i].mmio_id(), register_entries[i].mmio_id);

    ASSERT_TRUE(registers[i].has_masks());
    ASSERT_EQ(registers[i].masks().count(), register_entries[i].masks.size());
    for (size_t j = 0; j < register_entries[i].masks.size(); j++) {
      ASSERT_TRUE(registers[i].masks()[j].has_mask());
      if constexpr (std::is_same_v<T, uint8_t>) {
        ASSERT_EQ(registers[i].masks()[j].mask().Which(),
                  fuchsia_hardware_registers::wire::Mask::Tag::kR8);
        ASSERT_EQ(registers[i].masks()[j].mask().r8(), register_entries[i].masks[j].value);
      } else if constexpr (std::is_same_v<T, uint16_t>) {
        ASSERT_EQ(registers[i].masks()[j].mask().Which(),
                  fuchsia_hardware_registers::wire::Mask::Tag::kR16);
        ASSERT_EQ(registers[i].masks()[j].mask().r16(), register_entries[i].masks[j].value);
      } else if constexpr (std::is_same_v<T, uint32_t>) {
        ASSERT_EQ(registers[i].masks()[j].mask().Which(),
                  fuchsia_hardware_registers::wire::Mask::Tag::kR32);
        ASSERT_EQ(registers[i].masks()[j].mask().r32(), register_entries[i].masks[j].value);
      } else if constexpr (std::is_same_v<T, uint64_t>) {
        ASSERT_EQ(registers[i].masks()[j].mask().Which(),
                  fuchsia_hardware_registers::wire::Mask::Tag::kR64);
        ASSERT_EQ(registers[i].masks()[j].mask().r64(), register_entries[i].masks[j].value);
      } else {
        ASSERT_TRUE(false);
      }

      ASSERT_TRUE(registers[i].masks()[j].has_mmio_offset());
      ASSERT_EQ(registers[i].masks()[j].mmio_offset(), register_entries[i].masks[j].mmio_offset);

      ASSERT_TRUE(registers[i].masks()[j].has_count());
      ASSERT_EQ(registers[i].masks()[j].count(), register_entries[i].masks[j].count);

      ASSERT_TRUE(registers[i].masks()[j].has_overlap_check_on());
      ASSERT_EQ(registers[i].masks()[j].overlap_check_on(),
                register_entries[i].masks[j].overlap_check_on);
    }
  }
}

TEST(RegistersMetadataTest, TestEncode8) {
  static const fidl_metadata::registers::Register<uint8_t> kRegisters[]{
      {
          .bind_id = 0,
          .mmio_id = 1,
          .masks =
              {
                  {
                      .value = 0x98,
                      .mmio_offset = 0x20,
                  },
              },
      },
  };

  ASSERT_NO_FATAL_FAILURE(check_encodes<uint8_t>(kRegisters));
}

TEST(RegistersMetadataTest, TestEncode16) {
  static const fidl_metadata::registers::Register<uint16_t> kRegisters[]{
      {
          .bind_id = 0,
          .mmio_id = 1,
          .masks =
              {
                  {
                      .value = 0xBEEF,
                      .mmio_offset = 0x20,
                  },
              },
      },
  };

  ASSERT_NO_FATAL_FAILURE(check_encodes<uint16_t>(kRegisters));
}

TEST(RegistersMetadataTest, TestEncode32) {
  static const fidl_metadata::registers::Register<uint32_t> kRegisters[]{
      {
          .bind_id = 0,
          .mmio_id = 1,
          .masks =
              {
                  {
                      .value = 0xFFFF0000,
                      .mmio_offset = 0x20,
                  },
              },
      },
  };

  ASSERT_NO_FATAL_FAILURE(check_encodes<uint32_t>(kRegisters));
}

TEST(RegistersMetadataTest, TestEncode64) {
  static const fidl_metadata::registers::Register<uint64_t> kRegisters[]{
      {
          .bind_id = 0,
          .mmio_id = 1,
          .masks =
              {
                  {
                      .value = 0x012345678ABCDEF0,
                      .mmio_offset = 0x20,
                  },
              },
      },
  };

  ASSERT_NO_FATAL_FAILURE(check_encodes<uint64_t>(kRegisters));
}

TEST(RegistersMetadataTest, TestEncodeCountAndOverlapCheck) {
  static const fidl_metadata::registers::Register<uint32_t> kRegisters[]{
      {
          .bind_id = 0,
          .mmio_id = 1,
          .masks =
              {
                  {
                      .value = 0xFFFF0000,
                      .mmio_offset = 0x20,
                      .count = 8,
                      .overlap_check_on = false,
                  },
              },
      },
  };

  ASSERT_NO_FATAL_FAILURE(check_encodes<uint32_t>(kRegisters));
}

TEST(RegistersMetadataTest, TestEncodeMany) {
  static const fidl_metadata::registers::Register<uint16_t> kRegisters[]{
      {
          .bind_id = 0,
          .mmio_id = 1,
          .masks =
              {
                  {
                      .value = 0xFFFF,
                      .mmio_offset = 0x20,
                      .count = 5,
                      .overlap_check_on = false,
                  },
              },
      },
      {
          .bind_id = 1,
          .mmio_id = 4,
          .masks =
              {
                  {
                      .value = 0xABCD,
                      .mmio_offset = 0x7,
                  },
                  {
                      .value = 0x8765,
                      .mmio_offset = 0x0,
                      .count = 2,
                  },
              },
      },
  };

  ASSERT_NO_FATAL_FAILURE(check_encodes<uint16_t>(kRegisters));
}

TEST(RegistersMetadataTest, TestEncodeNoRegisters) {
  ASSERT_NO_FATAL_FAILURE(
      check_encodes(cpp20::span<const fidl_metadata::registers::Register<uint32_t>>()));
}
