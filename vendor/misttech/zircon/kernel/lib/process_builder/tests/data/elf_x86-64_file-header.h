// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_TESTS_DATA_ELF_X86_64_FILE_HEADER_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_TESTS_DATA_ELF_X86_64_FILE_HEADER_H_

// ```
// `hexdump -v -e '23/1 "\\x%02x" "\n"'
// src/lib/process_builder/test-utils/elf_x86-64_file-header.bin`
// ```
alignas(8) constexpr char HEADER_DATA_X86_64[] =
    "\x7f\x45\x4c\x46\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x03\x00\x3e\x00\x01\x00\x00"
    "\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"
    "\x00\x00\x00\x00\x00\x00\x40\x00\x38\x00\x00\x00\x00\x00\x00\x00\x00\x00";

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_TESTS_DATA_ELF_X86_64_FILE_HEADER_H_
