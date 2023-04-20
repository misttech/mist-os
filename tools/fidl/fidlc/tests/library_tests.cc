// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <string>
#include <string_view>

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/tests/error_test.h"
#include "tools/fidl/fidlc/tests/test_library.h"

namespace {

TEST(LibraryTests, GoodLibraryMultipleFiles) {
  TestLibrary library;
  library.AddFile("good/fi-0040-a.test.fidl");
  library.AddFile("good/fi-0040-b.test.fidl");

  ASSERT_COMPILED(library);
}

TEST(LibraryTests, BadFilesDisagreeOnLibraryName) {
  TestLibrary library;
  library.AddFile("bad/fi-0040-a.test.fidl");
  library.AddFile("bad/fi-0040-b.test.fidl");

  ASSERT_ERRORED_DURING_COMPILE(library, fidl::ErrFilesDisagreeOnLibraryName);
}

// TODO(fxbug.dev/109734): Remove once kZxSelectCaseSensitivity flag is removed
// and the associated zx naming migration is complete.
TEST(LibraryTests, ZxSelectCaseInsensitivityWithCamelCaseReferences) {
  constexpr std::string_view kSource = R"FIDL(
library zx;

alias status = int32;
alias time = uint64;
alias duration = uint64;

type ObjType = enum : uint32 {
    NONE = 0;
    PROCESS = 1;
    THREAD = 2;
    VMO = 3;
    CHANNEL = 4;
    EVENT = 5;
    PORT = 6;
};

type Rights = bits : uint32 {
    DUPLICATE = 0x00000001;
    TRANSFER = 0x00000002;
};

resource_definition handle : uint32 {
    properties {
        subtype ObjType;
        rights Rights;
    };
};

type StructWithCamelCaseReferences = resource struct {
  s Status;
  t Time;
  d Duration;
  h Handle:<PROCESS, Rights.DUPLICATE>;
};

)FIDL";

  {
    TestLibrary lib(std::string{kSource});
    ASSERT_COMPILED(lib);
  }

  {
    TestLibrary lib(std::string{kSource});
    lib.EnableFlag(fidl::ExperimentalFlags::Flag::kZxSelectCaseSensitivity);
    ASSERT_FALSE(lib.Compile());
    EXPECT_FALSE(lib.errors().empty());
    for (const auto& error : lib.errors()) {
      EXPECT_ERR(error, fidl::ErrNameNotFound);
    }
  }
}

// TODO(fxbug.dev/109734): Remove once kZxSelectCaseSensitivity flag is removed
// and the associated zx naming migration is complete.
TEST(LibraryTests, ZxSelectCaseInsensitivityWithSnakeCaseReferences) {
  constexpr std::string_view kSource = R"FIDL(
library zx;

alias Status = int32;
alias Time = uint64;
alias Duration = uint64;

type ObjType = enum : uint32 {
    NONE = 0;
    PROCESS = 1;
    THREAD = 2;
    VMO = 3;
    CHANNEL = 4;
    EVENT = 5;
    PORT = 6;
};

type Rights = bits : uint32 {
    DUPLICATE = 0x00000001;
    TRANSFER = 0x00000002;
};

resource_definition Handle : uint32 {
    properties {
        subtype ObjType;
        rights Rights;
    };
};

type StructWithSnakeCaseReferences = resource struct {
  s status;
  t time;
  d duration;
  h handle:<PROCESS, Rights.DUPLICATE>;
};

)FIDL";

  {
    TestLibrary lib(std::string{kSource});
    ASSERT_COMPILED(lib);
  }

  {
    TestLibrary lib(std::string{kSource});
    lib.EnableFlag(fidl::ExperimentalFlags::Flag::kZxSelectCaseSensitivity);
    ASSERT_FALSE(lib.Compile());
    EXPECT_FALSE(lib.errors().empty());
    for (const auto& error : lib.errors()) {
      EXPECT_ERR(error, fidl::ErrNameNotFound);
    }
  }
}

}  // namespace
