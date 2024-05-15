// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/ld/remote-abi-stub.h>
#include <lib/ld/remote-dynamic-linker.h>

#include <gtest/gtest.h>

#include "ld-remote-process-tests.h"

namespace {

// These tests reuse the fixture that supports the LdLoadTests (load-tests.cc)
// for the common handling of creating and launching a Zircon process.  The
// Load method is not used here, since that itself uses the RemoteDynamicLinker
// API under the covers, and the tests here are for that API surface itself.
using LdRemoteTests = ld::testing::LdRemoteProcessTests;

TEST_F(LdRemoteTests, RemoteAbiStub) {
  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  // Acquire the layout details from the stub.  The same values collected here
  // can be reused along with the decoded RemoteLoadModule for the stub for
  // creating and populating the RemoteLoadModule for the passive ABI of any
  // number of separate dynamic linking domains in however many processes.
  ld::RemoteAbiStub<>::Ptr abi_stub = ld::RemoteAbiStub<>::Create(diag, TakeStubLdVmo(), kPageSize);
  ASSERT_TRUE(abi_stub);
  EXPECT_GE(abi_stub->data_size(), sizeof(ld::abi::Abi<>) + sizeof(elfldltl::Elf<>::RDebug<>));
  EXPECT_LT(abi_stub->data_size(), kPageSize);
  EXPECT_LE(abi_stub->abi_offset(), abi_stub->data_size() - sizeof(ld::abi::Abi<>));
  EXPECT_LE(abi_stub->rdebug_offset(), abi_stub->data_size() - sizeof(elfldltl::Elf<>::RDebug<>));
  EXPECT_NE(abi_stub->rdebug_offset(), abi_stub->abi_offset())
      << "with data_size() " << abi_stub->data_size();

  // Verify that the TLSDESC entry points were found in the stub and that
  // their addresses pass some basic smell tests.
  std::set<elfldltl::Elf<>::size_type> tlsdesc_entrypoints;
  const auto segment_is_executable = [](const auto& segment) -> bool {
    return segment.executable();
  };
  const Linker::Module::Decoded& stub_module = *abi_stub->decoded_module();
  for (const elfldltl::Elf<>::size_type entry : abi_stub->tlsdesc_runtime()) {
    // Must be nonzero.
    EXPECT_NE(entry, 0u);

    // Must lie within the module bounds.
    EXPECT_GT(entry, stub_module.load_info().vaddr_start());
    EXPECT_LT(entry - stub_module.load_info().vaddr_start(), stub_module.load_info().vaddr_size());

    // Must be inside an executable segment.
    auto segment = stub_module.load_info().FindSegment(entry);
    ASSERT_NE(segment, stub_module.load_info().segments().end());
    EXPECT_TRUE(std::visit(segment_is_executable, *segment));

    // Must be unique.
    auto [it, inserted] = tlsdesc_entrypoints.insert(entry);
    EXPECT_TRUE(inserted) << "duplicate entry point " << entry;
  }
  EXPECT_EQ(tlsdesc_entrypoints.size(), ld::kTlsdescRuntimeCount);
}

}  // namespace
