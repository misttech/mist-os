// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/lib/fidl-metadata/registers.h"

#include <fidl/fuchsia.hardware.registers/cpp/wire.h>

namespace fidl_metadata::registers {

namespace {

template <typename T>
zx::result<std::vector<uint8_t>> ToFidl(cpp20::span<const Register<T>> registers) {
  fidl::Arena allocator;
  fidl::VectorView<fuchsia_hardware_registers::wire::RegistersMetadataEntry> registers_metadata(
      allocator, registers.size());

  for (size_t i = 0; i < registers.size(); i++) {
    auto& src_reg = registers[i];

    fidl::VectorView<fuchsia_hardware_registers::wire::MaskEntry> mask_entries(
        allocator, src_reg.masks.size());
    for (size_t j = 0; j < src_reg.masks.size(); j++) {
      auto& src_mask = src_reg.masks[j];

      auto builder = fuchsia_hardware_registers::wire::MaskEntry::Builder(allocator)
                         .mmio_offset(src_mask.mmio_offset)
                         .count(src_mask.count)
                         .overlap_check_on(src_mask.overlap_check_on);
      if constexpr (std::is_same_v<T, uint8_t>) {
        builder.mask(std::move(fuchsia_hardware_registers::wire::Mask::WithR8(src_mask.value)));
      } else if constexpr (std::is_same_v<T, uint16_t>) {
        builder.mask(std::move(fuchsia_hardware_registers::wire::Mask::WithR16(src_mask.value)));
      } else if constexpr (std::is_same_v<T, uint32_t>) {
        builder.mask(std::move(fuchsia_hardware_registers::wire::Mask::WithR32(src_mask.value)));
      } else if constexpr (std::is_same_v<T, uint64_t>) {
        builder.mask(
            std::move(fuchsia_hardware_registers::wire::Mask::WithR64(allocator, src_mask.value)));
      } else {
        static_assert(false);
      }

      mask_entries[j] = std::move(builder.Build());
    }

    registers_metadata[i] =
        std::move(fuchsia_hardware_registers::wire::RegistersMetadataEntry::Builder(allocator)
                      .bind_id(src_reg.bind_id)
                      .mmio_id(src_reg.mmio_id)
                      .masks(std::move(mask_entries))
                      .Build());
  }

  return zx::result<std::vector<uint8_t>>{
      fidl::Persist(std::move(fuchsia_hardware_registers::wire::Metadata::Builder(allocator)
                                  .registers(std::move(registers_metadata))
                                  .Build()))
          .map_error(std::mem_fn(&fidl::Error::status))};
}

}  // namespace

zx::result<std::vector<uint8_t>> RegistersMetadataToFidl(cpp20::span<const Register8> registers) {
  return ToFidl(registers);
}
zx::result<std::vector<uint8_t>> RegistersMetadataToFidl(cpp20::span<const Register16> registers) {
  return ToFidl(registers);
}
zx::result<std::vector<uint8_t>> RegistersMetadataToFidl(cpp20::span<const Register32> registers) {
  return ToFidl(registers);
}
zx::result<std::vector<uint8_t>> RegistersMetadataToFidl(cpp20::span<const Register64> registers) {
  return ToFidl(registers);
}

}  // namespace fidl_metadata::registers
