// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/fidl_codec/printer.h"

#include <fuchsia/io/cpp/fidl.h>

#include <sstream>

#include <gtest/gtest.h>

namespace fidl_codec {

TEST(PrettyPrinter, Uint64Print) {
  std::stringstream out;
  PrettyPrinter printer(out, WithoutColors, false, "", 100, false);
  // We use variables to get the proper type to <<.
  constexpr uint64_t n = 255;
  constexpr uint64_t zero = 0;
  constexpr uint64_t sixteen = 16;
  constexpr uint64_t ten = 10;
  ASSERT_EQ(printer.remaining_size(), 100U);
  printer << n;
  ASSERT_EQ(printer.remaining_size(), 97U);
  printer << zero;
  ASSERT_EQ(printer.remaining_size(), 96U);
  printer << std::hex << n;
  ASSERT_EQ(printer.remaining_size(), 94U);
  printer << zero;
  ASSERT_EQ(printer.remaining_size(), 93U);
  printer << sixteen;
  ASSERT_EQ(printer.remaining_size(), 91U);
  printer << std::dec << ten;
  ASSERT_EQ(printer.remaining_size(), 89U);
  ASSERT_EQ(out.str(), "2550ff01010");
}

TEST(PrettyPrinter, OpenModePrint) {
  std::stringstream out;
  PrettyPrinter printer(out, WithoutColors, false, "", 100, false);
  printer.DisplayDirectoryOpenMode(fuchsia::io::MODE_PROTECTION_MASK);
  out << '\n';
  printer.DisplayDirectoryOpenMode(fuchsia::io::MODE_TYPE_SERVICE);
  out << '\n';
  printer.DisplayDirectoryOpenMode(fuchsia::io::MODE_TYPE_FILE);
  out << '\n';
  printer.DisplayDirectoryOpenMode(fuchsia::io::MODE_TYPE_BLOCK_DEVICE);
  out << '\n';
  printer.DisplayDirectoryOpenMode(fuchsia::io::MODE_TYPE_DIRECTORY);
  out << '\n';
  printer.DisplayDirectoryOpenMode(0);
  out << '\n';
  ASSERT_EQ(out.str(),
            "S_ISUID | S_ISGID | S_IRWXU | S_IRWXG | S_IRWXO | 0x200\n"
            "MODE_TYPE_SERVICE\n"
            "MODE_TYPE_FILE\n"
            "MODE_TYPE_BLOCK_DEVICE\n"
            "MODE_TYPE_DIRECTORY\n"
            "0\n");
}

}  // namespace fidl_codec
