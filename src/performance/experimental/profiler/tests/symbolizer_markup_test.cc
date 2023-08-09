// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/experimental/profiler/symbolizer_markup.h"

#include <elf.h>

#include <gtest/gtest.h>

#include "src/performance/experimental/profiler/sampler.h"

TEST(SymbolizMarkupTest, FormatSample) {
  profiler::Sample sample{.pid = 1, .tid = 2, .stack = {1, 2, 3, 0xa, 0xb, 0xc}};

  std::string formatted = profiler::symbolizer_markup::FormatSample(sample);
  std::string expected =
      "1\n"
      "2\n"
      "{{{bt:0:0x1:pc}}}\n"
      "{{{bt:1:0x2:ra}}}\n"
      "{{{bt:2:0x3:ra}}}\n"
      "{{{bt:3:0xa:ra}}}\n"
      "{{{bt:4:0xb:ra}}}\n"
      "{{{bt:5:0xc:ra}}}\n";
  EXPECT_EQ(expected, formatted);
}

TEST(SymbolizMarkupTest, FormatModule) {
  profiler::Module mod{
      .module_id = 1,
      .module_name = "test_module.so",
      .build_id = std::vector<std::byte>{std::byte{0x01}, std::byte{0x23}, std::byte{0xca},
                                         std::byte{0xfe}},
      .vaddr = 0x1000,
      .loads =
          {
              profiler::Segment{
                  .p_vaddr = 0x2000,
                  .p_memsz = 0x2400,
                  .p_flags = PF_X | PF_R,
              },
              profiler::Segment{
                  .p_vaddr = 0x8000,
                  .p_memsz = 0x3200,
                  .p_flags = PF_W,
              },
              profiler::Segment{
                  .p_vaddr = 0x10000,
                  .p_memsz = 0x1000,
                  .p_flags = PF_X,
              },
          },
  };

  std::string formatted = profiler::symbolizer_markup::FormatModule(mod);
  // Pages sizes should be rounded up to the nearest page (0x1000);
  std::string expected =
      "{{{module:1:test_module.so:elf:0123cafe}}}\n"
      "{{{mmap:0x3000:0x3000:load:1:rx:0x2000}}}\n"
      "{{{mmap:0x9000:0x4000:load:1:w:0x8000}}}\n"
      "{{{mmap:0x11000:0x1000:load:1:x:0x10000}}}\n";
  EXPECT_EQ(expected, formatted);

  profiler::Module mod22{
      .module_id = 22,
      .module_name = "test_module.so",
      .build_id = std::vector<std::byte>{std::byte{0x01}, std::byte{0x23}, std::byte{0xca},
                                         std::byte{0xfe}},
      .vaddr = 0x1000,
      .loads =
          {
              profiler::Segment{
                  .p_vaddr = 0x2000,
                  .p_memsz = 0x400,
                  .p_flags = PF_X | PF_R,
              },
          },
  };

  std::string formatted22 = profiler::symbolizer_markup::FormatModule(mod22);
  std::string expected22 =
      "{{{module:22:test_module.so:elf:0123cafe}}}\n"
      "{{{mmap:0x3000:0x1000:load:22:rx:0x2000}}}\n";
  EXPECT_EQ(expected22, formatted22);
}
