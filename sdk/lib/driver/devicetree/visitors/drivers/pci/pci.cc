// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/devicetree/visitors/drivers/pci/pci.h"

#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace pci_dt {

namespace {

zx::result<PciRange> ParsePciRange(devicetree::ByteView bytes, uint32_t num_size_cells,
                                   uint32_t num_parent_address_cells) {
  // First byte is the high cell of the bus address. Store it separately.
  auto bus_address_high_cell = devicetree::PropertyValue(bytes.subspan(0, sizeof(uint32_t)));

  // Parse the rest as if it was a 2-address-cell range.
  bytes = bytes.subspan(sizeof(uint32_t), bytes.size() - sizeof(uint32_t));

  return zx::ok(PciRange{
      .range = devicetree::RangesPropertyElement(
          bytes, {2 /* num_address_cells */, num_size_cells, num_parent_address_cells}),
      .bus_address_high_cell = *bus_address_high_cell.AsUint32(),
  });
}

}  // namespace

constexpr std::string_view kReg = "reg";
constexpr std::string_view kRanges = "ranges";

PciVisitor::PciVisitor() : DriverVisitor({"pci-host-cam-generic", "pci-host-ecam-generic"}) {}

zx::result<> PciVisitor::DriverVisit(fdf_devicetree::Node& node,
                                     const devicetree::PropertyDecoder& decoder) {
  auto [reg_property, ranges_property] = decoder.FindProperties(kReg, kRanges);
  if (!reg_property.has_value()) {
    FDF_LOG(ERROR, "Failed to find property\"%s\".", std::string(kReg).c_str());
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  auto reg = reg_property->AsReg(decoder);
  if (!reg.has_value()) {
    FDF_LOG(ERROR, "Property is not a reg array");
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  if (reg->size() != 1u) {
    FDF_SLOG(ERROR, "Property \"reg\" expected to have one entry", KV("entries", reg->size()));
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  reg_ = (*reg)[0u];

  if (!ranges_property.has_value()) {
    FDF_LOG(ERROR, "Failed to find property\"%s\".", std::string(kRanges).c_str());
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  // We can't view this as a RangesProperty because that type assumes that addresses
  // are encoded using at most 2 cells and the PCI bus addresses use 3 cells.
  // Instead, we can skip the first cell of each range which contains the high
  // byte of the bus address and decode the rest as a regular 2-address-cell range.
  uint32_t num_address_cells = *decoder.num_address_cells();
  if (num_address_cells != 3) {
    FDF_LOG(ERROR, "Expected #address-cells to be 3 in PCI ranges property, found %u",
            num_address_cells);
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  uint32_t num_size_cells = *decoder.num_size_cells();
  uint32_t num_parent_address_cells = *decoder.parent()->num_address_cells();
  size_t range_property_length_bytes =
      (num_address_cells + num_size_cells + num_parent_address_cells) * sizeof(uint32_t);
  auto ranges_property_bytes = ranges_property->AsBytes();
  size_t num_ranges = ranges_property_bytes.size() / range_property_length_bytes;
  for (size_t i = 0; i < num_ranges; ++i) {
    size_t range_start = i * range_property_length_bytes;
    auto range_bytes = ranges_property_bytes.subspan(range_start, range_property_length_bytes);
    auto range = ParsePciRange(range_bytes, num_size_cells, num_parent_address_cells);
    if (!range.is_ok()) {
      FDF_LOG(ERROR, "Could not parse range %lu", i);
      return range.take_error();
    }
    ranges_.push_back(*range);
  }

  return zx::ok();
}

}  // namespace pci_dt
