// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/stat.h>
#include <zircon/types.h>

#include <iostream>

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/fix/command_line_options.h"
#include "tools/fidl/fidlc/include/fidl/diagnostics.h"
#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/fixes.h"
#include "tools/fidl/fidlc/include/fidl/reporter.h"
#include "tools/fidl/fidlc/include/fidl/source_file.h"
#include "tools/fidl/fidlc/include/fidl/source_manager.h"
#include "tools/fidl/fidlc/include/fidl/versioning_types.h"
#include "tools/fidl/fidlc/tests/error_test.h"

namespace {

using namespace fidl;
using namespace fidl::fix;

static const std::string kBadFileName = "bad.fidl";
static const std::string kGoodFileName = "good.fidl";

template <typename T, Fixable::Kind Fixing>
void GoodParsedFix(const ExperimentalFlags experimental_flags, const std::string& before,
                   const std::string& after) {
  static_assert(std::is_base_of_v<ParsedFix, T>,
                "Only classes derived from |ParsedFix| may be used by |GoodTransform()}");

  Reporter reporter;
  auto manager = std::make_unique<SourceManager>();
  manager->AddSourceFile(std::make_unique<SourceFile>(kGoodFileName, before));
  auto fix = T(std::move(manager), experimental_flags);
  EXPECT_EQ(fix.ValidateFlags(), Status::kOk);

  auto result = fix.Transform(&reporter);
  ASSERT_TRUE(result.is_ok());

  OutputMap outputs = result.value();
  EXPECT_EQ(outputs.size(), 1);

  auto transformed = outputs[kGoodFileName];
  EXPECT_STREQ(transformed, after);
}

// This template assumes that all libraries and dependencies are in a single file.
template <typename T, Fixable::Kind Fixing>
void GoodCompiledFix(const ExperimentalFlags experimental_flags, const std::string& before,
                     const std::vector<std::string>& deps, const std::string& after) {
  static_assert(std::is_base_of_v<CompiledFix, T>,
                "Only classes derived from |CompiledFix| may be used by |GoodTransform()}");

  Reporter reporter;
  auto library_manager = std::make_unique<SourceManager>();
  library_manager->AddSourceFile(std::make_unique<SourceFile>(kGoodFileName, before));

  std::vector<std::unique_ptr<SourceManager>> deps_managers;
  for (size_t i = 0; i < deps.size(); i++) {
    deps_managers.emplace_back(std::make_unique<SourceManager>());
    std::unique_ptr<SourceManager>& manager = deps_managers.back();
    manager->AddSourceFile(
        std::make_unique<SourceFile>("dep_" + std::to_string(i) + ".fidl", deps[i]));
  }

  VersionSelection version_selection;
  version_selection.Insert(Platform::Parse("example").value(), Version::Head());

  auto fix = T(std::move(library_manager), std::move(deps_managers), &version_selection,
               experimental_flags);
  EXPECT_EQ(fix.ValidateFlags(), Status::kOk);

  auto result = fix.Transform(&reporter);
  ASSERT_TRUE(result.is_ok());

  OutputMap outputs = result.value();
  ASSERT_EQ(outputs.size(), 1);

  auto transformed = outputs[kGoodFileName];
  EXPECT_STREQ(transformed, after);
}

// Immediately fail - used to test transform failure reporting.
class FailingTransformer final : public ParsedTransformer {
 public:
  FailingTransformer(const std::vector<const SourceFile*>& source_files,
                     const ExperimentalFlags& experimental_flags, Reporter* reporter)
      : ParsedTransformer(source_files, experimental_flags, reporter) {}

  void WhenFile(raw::File* el, TokenSlice& token_slice) override {
    // Immediately fail, and do nothing else.
    AddError("failure!");
  }
};

// A fix that fails during transformation.
class FailingParsedFix final : public ParsedFix {
 public:
  FailingParsedFix(std::unique_ptr<SourceManager> library,
                   const ExperimentalFlags experimental_flags)
      : ParsedFix(Fixable::Get(Fixable::Kind::kNoop), std::move(library), experimental_flags) {}

  std::unique_ptr<ParsedTransformer> GetParsedTransformer(
      const std::vector<const SourceFile*>& source_files,
      const fidl::ExperimentalFlags& experimental_flags, Reporter* reporter) override {
    return std::make_unique<FailingTransformer>(source_files, experimental_flags, reporter);
  }
};

TEST(FixTests, BadCommandLineFlagsEmpty) {
  fidl::fix::CommandLineOptions options;
  std::vector<std::string> filepaths;
  std::unique_ptr<fidl::fix::Fix> fix;
  auto status = ProcessCommandLine(options, filepaths, fix);
  EXPECT_EQ(status.has_error(), true);
}

TEST(FixTests, GoodCommandLineFlags) {
  fidl::fix::CommandLineOptions options;
  std::vector<std::string> filepaths;
  std::unique_ptr<fidl::fix::Fix> fix;

  options.fix = "noop";

  struct stat buffer;
  if (stat("host_x64/fidlc-tests/good/fi-0001.test.fidl", &buffer) == 0) {
    filepaths.emplace_back("host_x64/fidlc-tests/good/fi-0001.test.fidl");
  } else if (stat("host_arm64/fidlc-tests/good/fi-0001.test.fidl", &buffer) == 0) {
    filepaths.emplace_back("host_arm64/fidlc-tests/good/fi-0001.test.fidl");
  }
  // If the path is something else, there will be an error.

  auto status = ProcessCommandLine(options, filepaths, fix);
  EXPECT_EQ(false, status.has_error());
}

TEST(FixTests, BadInvalidFlags) {
  ExperimentalFlags experimental_flags;
  auto manager = std::make_unique<SourceManager>();
  manager->AddSourceFile(std::make_unique<SourceFile>("bad.fidl", "library;"));

  auto fix = FailingParsedFix(std::move(manager), experimental_flags);
  EXPECT_EQ(fix.ValidateFlags(), Status::kErrorOther);
}

TEST(FixTests, BadParsingFailure) {
  Reporter reporter;
  ExperimentalFlags experimental_flags;
  experimental_flags.EnableFlag(ExperimentalFlags::Flag::kNoop);
  auto manager = std::make_unique<SourceManager>();
  manager->AddSourceFile(std::make_unique<SourceFile>("bad.fidl", "library;"));

  auto fix = NoopParsedFix(std::move(manager), experimental_flags);
  EXPECT_EQ(fix.ValidateFlags(), Status::kOk);

  auto result = fix.Transform(&reporter);
  ASSERT_FALSE(result.is_ok());

  auto failure = result.error_value();
  EXPECT_EQ(failure.status, Status::kErrorPreFix);
  ASSERT_EQ(failure.errors.size(), 1);
  ASSERT_EQ(reporter.errors().size(), 1);
  EXPECT_EQ(failure.errors[0].step, fidl::fix::Step::kPreparing);
  EXPECT_ERR(reporter.errors()[0], ErrUnexpectedTokenOfKind);
  EXPECT_SUBSTR(failure.errors[0].msg, "bad.fidl");
}

TEST(FixTests, BadTransformFailure) {
  Reporter reporter;
  ExperimentalFlags experimental_flags;
  experimental_flags.EnableFlag(ExperimentalFlags::Flag::kNoop);
  auto manager = std::make_unique<SourceManager>();
  manager->AddSourceFile(std::make_unique<SourceFile>("valid.fidl", "library example;"));

  auto fix = FailingParsedFix(std::move(manager), experimental_flags);
  EXPECT_EQ(fix.ValidateFlags(), Status::kOk);

  auto result = fix.Transform(&reporter);
  ASSERT_FALSE(result.is_ok());

  auto failure = result.error_value();
  EXPECT_EQ(failure.status, Status::kErrorDuringFix);
  ASSERT_EQ(failure.errors.size(), 1);
  ASSERT_TRUE(reporter.errors().empty());
  EXPECT_EQ(failure.errors[0].step, fidl::fix::Step::kTransforming);
  EXPECT_SUBSTR(failure.errors[0].msg, "failure!");
}

TEST(FixTests, BadProtocolModifierFixMissingExperiment) {
  ExperimentalFlags experimental_flags;
  auto manager = std::make_unique<SourceManager>();
  manager->AddSourceFile(std::make_unique<SourceFile>("valid.fidl", "library example;"));

  auto fix = ProtocolModifierFix(std::move(manager), experimental_flags);
  EXPECT_EQ(fix.ValidateFlags(), Status::kErrorOther);
}

void GoodProtocolModifierFix(const std::string& before, const std::string& after) {
  GoodParsedFix<ProtocolModifierFix, Fixable::Kind::kProtocolModifier>(
      ExperimentalFlags(ExperimentalFlags::Flag::kUnknownInteractions), before, after);
}

TEST(FixTests, GoodProtocolModifierFixEmptyProtocolAlreadyClosed) {
  std::string noop = R"FIDL(library example;
closed protocol MyProtocol {};
)FIDL";
  GoodProtocolModifierFix(noop, noop);
}

TEST(FixTests, GoodProtocolModifierFixEmptyProtocolAlreadyAjar) {
  std::string noop = R"FIDL(library example;
ajar protocol MyProtocol {};
)FIDL";
  GoodProtocolModifierFix(noop, noop);
}

TEST(FixTests, GoodProtocolModifierFixEmptyProtocolAlreadyOpen) {
  std::string noop = R"FIDL(library example;
open protocol MyProtocol {};
)FIDL";
  GoodProtocolModifierFix(noop, noop);
}

TEST(FixTests, GoodProtocolModifierFixEmptyProtocolUnmodified) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixEmptyProtocolWithComment) {
  GoodProtocolModifierFix(R"FIDL(library example;
// comment
protocol MyProtocol {};
)FIDL",
                          R"FIDL(library example;
// comment
closed protocol MyProtocol {};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixEmptyProtocolWithDocComment) {
  GoodProtocolModifierFix(R"FIDL(library example;
/// doc comment
protocol MyProtocol {};
)FIDL",
                          R"FIDL(library example;
/// doc comment
closed protocol MyProtocol {};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixEmptyProtocolWithAttribute) {
  GoodProtocolModifierFix(R"FIDL(library example;
@attr
protocol MyProtocol {};
)FIDL",
                          R"FIDL(library example;
@attr
closed protocol MyProtocol {};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixEmptyProtocolWithAllPrefixes) {
  GoodProtocolModifierFix(R"FIDL(library example;
// comment
/// doc comment
@attr
protocol MyProtocol {};
)FIDL",
                          R"FIDL(library example;
// comment
/// doc comment
@attr
closed protocol MyProtocol {};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixMultipleEmptyProtocols) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {};

protocol MyOtherProtocol {};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {};

closed protocol MyOtherProtocol {};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixMethodsAlreadyStrict) {
  std::string noop = R"FIDL(library example;
closed protocol MyProtocol {
    strict OneWay();
    strict TwoWay() -> ();
    strict WithErr() -> () error uint32;
    strict -> OnEvent();
};
)FIDL";
  GoodProtocolModifierFix(noop, noop);
}

TEST(FixTests, GoodProtocolModifierFixMethodsAlreadyFlexible) {
  std::string noop = R"FIDL(library example;
open protocol MyProtocol {
    flexible OneWay();
    flexible TwoWay() -> ();
    flexible WithErr() -> () error uint32;
    flexible -> OnEvent();
};
)FIDL";
  GoodProtocolModifierFix(noop, noop);
}

TEST(FixTests, GoodProtocolModifierFixMethodsUnmodified) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {
    OneWay();
    TwoWay() -> ();
    WithErr() -> () error uint32;
    -> OnEvent();
};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {
    strict OneWay();
    strict TwoWay() -> ();
    strict WithErr() -> () error uint32;
    strict -> OnEvent();
};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixMethodsWithComments) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {
    // comment
    OneWay();
    // comment
    TwoWay() -> ();
    // comment
    WithErr() -> () error uint32;
    // comment
    -> OnEvent();
};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {
    // comment
    strict OneWay();
    // comment
    strict TwoWay() -> ();
    // comment
    strict WithErr() -> () error uint32;
    // comment
    strict -> OnEvent();
};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixMethodsWithDocComments) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {
    /// doc comment
    OneWay();
    /// doc comment
    TwoWay() -> ();
    /// doc comment
    WithErr() -> () error uint32;
    /// doc comment
    -> OnEvent();
};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {
    /// doc comment
    strict OneWay();
    /// doc comment
    strict TwoWay() -> ();
    /// doc comment
    strict WithErr() -> () error uint32;
    /// doc comment
    strict -> OnEvent();
};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixMethodsWithAttributes) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {
    @attr
    OneWay();
    @attr
    TwoWay() -> ();
    @attr
    WithErr() -> () error uint32;
    @attr
    -> OnEvent();
};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {
    @attr
    strict OneWay();
    @attr
    strict TwoWay() -> ();
    @attr
    strict WithErr() -> () error uint32;
    @attr
    strict -> OnEvent();
};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixMethodsWithAllPrefixes) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {
    // comment
    /// doc comment
    @attr
    OneWay();
    // comment
    /// doc comment
    @attr
    TwoWay() -> ();
    // comment
    /// doc comment
    @attr
    WithErr() -> () error uint32;
    // comment
    /// doc comment
    @attr
    -> OnEvent();
};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {
    // comment
    /// doc comment
    @attr
    strict OneWay();
    // comment
    /// doc comment
    @attr
    strict TwoWay() -> ();
    // comment
    /// doc comment
    @attr
    strict WithErr() -> () error uint32;
    // comment
    /// doc comment
    @attr
    strict -> OnEvent();
};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixMultiplePopulatedProtocols) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {
    OneWay();
    TwoWay() -> ();
};

protocol MyOtherProtocol {
    WithErr() -> () error uint32;
    -> OnEvent();
};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {
    strict OneWay();
    strict TwoWay() -> ();
};

closed protocol MyOtherProtocol {
    strict WithErr() -> () error uint32;
    strict -> OnEvent();
};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixProtocolModifierOnly) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {
    strict OneWay();
    strict TwoWay() -> ();
    strict WithErr() -> () error uint32;
    strict -> OnEvent();
};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {
    strict OneWay();
    strict TwoWay() -> ();
    strict WithErr() -> () error uint32;
    strict -> OnEvent();
};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixMethodModifiersOnly) {
  GoodProtocolModifierFix(R"FIDL(library example;
closed protocol MyProtocol {
    OneWay();
    TwoWay() -> ();
    WithErr() -> () error uint32;
    -> OnEvent();
};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {
    strict OneWay();
    strict TwoWay() -> ();
    strict WithErr() -> () error uint32;
    strict -> OnEvent();
};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixSomeExistingModifiers) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {
    strict OneWay();
    strict TwoWay() -> ();
    WithErr() -> () error uint32;
    -> OnEvent();
};

open protocol MyOtherProtocol {
    OneWay();
    TwoWay() -> ();
    strict WithErr() -> () error uint32;
    flexible -> OnEvent();
};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {
    strict OneWay();
    strict TwoWay() -> ();
    strict WithErr() -> () error uint32;
    strict -> OnEvent();
};

open protocol MyOtherProtocol {
    strict OneWay();
    strict TwoWay() -> ();
    strict WithErr() -> () error uint32;
    flexible -> OnEvent();
};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixOtherProtocolIdentifiers) {
  GoodProtocolModifierFix(R"FIDL(library example;

const protocol bool = true;

@attr(protocol)
protocol MyProtocol {};

@protocol(protocol=protocol)
protocol MyOtherProtocol {};
)FIDL",
                          R"FIDL(library example;

const protocol bool = true;

@attr(protocol)
closed protocol MyProtocol {};

@protocol(protocol=protocol)
closed protocol MyOtherProtocol {};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixProtocolNamedProtocol) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol protocol {};
)FIDL",
                          R"FIDL(library example;
closed protocol protocol {};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixMethodsWithCompose) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {
    compose MyOtherProtocol;
    OneWay();
    TwoWay() -> ();
    strict WithErr() -> () error uint32;
    flexible -> OnEvent();
};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {
    compose MyOtherProtocol;
    strict OneWay();
    strict TwoWay() -> ();
    strict WithErr() -> () error uint32;
    flexible -> OnEvent();
};
)FIDL");
}

TEST(FixTests, GoodProtocolModifierFixMethodsWithComposeAndAllPrefixes) {
  GoodProtocolModifierFix(R"FIDL(library example;
protocol MyProtocol {
    compose MyOtherProtocol;
    // comment
    /// doc comment
    @attr
    OneWay();
    // comment
    /// doc comment
    @attr
    TwoWay() -> ();
    // comment
    /// doc comment
    @attr
    WithErr() -> () error uint32;
    // comment
    /// doc comment
    @attr
    -> OnEvent();
};
)FIDL",
                          R"FIDL(library example;
closed protocol MyProtocol {
    compose MyOtherProtocol;
    // comment
    /// doc comment
    @attr
    strict OneWay();
    // comment
    /// doc comment
    @attr
    strict TwoWay() -> ();
    // comment
    /// doc comment
    @attr
    strict WithErr() -> () error uint32;
    // comment
    /// doc comment
    @attr
    strict -> OnEvent();
};
)FIDL");
}

TEST(FixTests, BadEmptyStructResponseFixMissingBothExperiments) {
  ExperimentalFlags experimental_flags;
  VersionSelection version_selection;
  version_selection.Insert(Platform::Parse("example").value(), Version::Head());
  auto manager = std::make_unique<SourceManager>();
  manager->AddSourceFile(std::make_unique<SourceFile>("valid.fidl", "library example;"));
  auto fix = EmptyStructResponseFix(std::move(manager), {}, &version_selection, experimental_flags);
  EXPECT_EQ(fix.ValidateFlags(), Status::kErrorOther);
}

TEST(FixTests, BadEmptyStructResponseFixMissingSimpleEmptyResponseExperiment) {
  auto experimental_flags = ExperimentalFlags(ExperimentalFlags::Flag::kUnknownInteractions);
  VersionSelection version_selection;
  version_selection.Insert(Platform::Parse("example").value(), Version::Head());
  auto manager = std::make_unique<SourceManager>();
  manager->AddSourceFile(std::make_unique<SourceFile>("valid.fidl", "library example;"));
  auto fix = EmptyStructResponseFix(std::move(manager), {}, &version_selection, experimental_flags);
  EXPECT_EQ(fix.ValidateFlags(), Status::kErrorOther);
}

TEST(FixTests, BadEmptyStructResponseFixMissingUnknownInteractionsExperiment) {
  auto experimental_flags = ExperimentalFlags(ExperimentalFlags::Flag::kSimpleEmptyResponseSyntax);
  VersionSelection version_selection;
  version_selection.Insert(Platform::Parse("example").value(), Version::Head());
  auto manager = std::make_unique<SourceManager>();
  manager->AddSourceFile(std::make_unique<SourceFile>("valid.fidl", "library example;"));
  auto fix = EmptyStructResponseFix(std::move(manager), {}, &version_selection, experimental_flags);
  EXPECT_EQ(fix.ValidateFlags(), Status::kErrorOther);
}

void GoodEmptyStructResponseFix(const std::string& before, std::vector<std::string> deps,
                                const std::string& after) {
  GoodCompiledFix<EmptyStructResponseFix, Fixable::Kind::kEmptyStructResponse>(
      ExperimentalFlags({ExperimentalFlags::Flag::kSimpleEmptyResponseSyntax,
                         ExperimentalFlags::Flag::kUnknownInteractions}),
      before, deps, after);
}

void GoodEmptyStructResponseFix(const std::string& before, const std::string& after) {
  GoodEmptyStructResponseFix(before, {}, after);
}

TEST(FixTests, GoodEmptyStructResponseFixAlreadyCorrect) {
  std::string noop = R"FIDL(library example;
open protocol MyProtocol {
    strict StrictWithError() -> () error uint32;
    flexible FlexibleNoError() -> ();
    flexible FlexibleWithError() -> () error uint32;
};
)FIDL";
  GoodEmptyStructResponseFix(noop, noop);
}

TEST(FixTests, GoodEmptyStructResponseFixNoopForNonEmptyStructs) {
  std::string noop = R"FIDL(library example;
type MyStruct = struct {
    arg bool;
};

open protocol MyProtocol {
    strict OneWay(MyStruct);
    strict -> Event(MyStruct);
    strict TwoWay(MyStruct) -> (MyStruct);
    strict TwoWayWithError(MyStruct) -> (MyStruct) error uint32;

    flexible FlexibleOneWay(MyStruct);
    flexible -> FlexibleEvent(MyStruct);
    flexible FlexibleTwoWay(MyStruct) -> (MyStruct);
    flexible FlexibleTwoWayWithError(MyStruct) -> (MyStruct) error uint32;
};
)FIDL";
  GoodEmptyStructResponseFix(noop, noop);
}

TEST(FixTests, GoodEmptyStructResponseFixNoopForNonStructPayloads) {
  std::string noop = R"FIDL(library example;
type MyTable = table {};
type MyUnion = flexible union {};

open protocol MyProtocol {
    strict OneWay(MyTable);
    strict -> Event(MyUnion);
    strict TwoWay(MyTable) -> (MyUnion);
    strict TwoWayWithError(MyUnion) -> (MyTable) error uint32;

    flexible FlexibleOneWay(MyUnion);
    flexible -> FlexibleEvent(MyTable);
    flexible FlexibleTwoWay(MyUnion) -> (MyTable);
    flexible FlexibleTwoWayWithError(MyTable) -> (MyUnion) error uint32;
};
)FIDL";
  GoodEmptyStructResponseFix(noop, noop);
}

TEST(FixTests, GoodEmptyStructResponseFixStrictWithErrorInlinePayload) {
  GoodEmptyStructResponseFix(R"FIDL(library example;
open protocol MyProtocol {
    strict StrictWithError() -> (struct {}) error uint32;
};
)FIDL",
                             R"FIDL(library example;
open protocol MyProtocol {
    strict StrictWithError() -> () error uint32;
};
)FIDL");
}

TEST(FixTests, GoodEmptyStructResponseFixStrictWithErrorNamedPayload) {
  GoodEmptyStructResponseFix(R"FIDL(library example;
type MyEmptyStruct = struct {};
open protocol MyProtocol {
    strict StrictWithError() -> (MyEmptyStruct) error uint32;
};
)FIDL",
                             R"FIDL(library example;
type MyEmptyStruct = struct {};
open protocol MyProtocol {
    strict StrictWithError() -> () error uint32;
};
)FIDL");
}

TEST(FixTests, GoodEmptyStructResponseFixStrictWithErrorImportedPayload) {
  std::vector<std::string> deps = {R"FIDL(library dependency;
type MyEmptyStruct = struct {};
alias MyAliasedEmptyStruct = MyEmptyStruct;
)FIDL"};

  GoodEmptyStructResponseFix(R"FIDL(library example;
using dependency;
open protocol MyProtocol {
    strict StrictWithError() -> (dependency.MyEmptyStruct) error uint32;
    strict StrictWithErrorAliased() -> (dependency.MyAliasedEmptyStruct) error uint32;
};
)FIDL",
                             deps,
                             R"FIDL(library example;
using dependency;
open protocol MyProtocol {
    strict StrictWithError() -> () error uint32;
    strict StrictWithErrorAliased() -> () error uint32;
};
)FIDL");
}

TEST(FixTests, BadEmptyStructResponseFixStrictWithErrorVersionedPayload) {
  Reporter reporter;
  reporter.ignore_fixables();
  VersionSelection version_selection;
  version_selection.Insert(Platform::Parse("example").value(), Version::Head());
  auto library_manager = std::make_unique<SourceManager>();
  library_manager->AddSourceFile(
      std::make_unique<SourceFile>(kBadFileName, R"FIDL(@available(added=1)
library example;
open protocol MyProtocol {
    strict StrictWithError() -> (struct {
      @available(removed=2)
      removed bool;
    }) error uint32;
};
)FIDL"));

  auto fix =
      EmptyStructResponseFix(std::move(library_manager), {}, &version_selection,
                             ExperimentalFlags({ExperimentalFlags::Flag::kSimpleEmptyResponseSyntax,
                                                ExperimentalFlags::Flag::kUnknownInteractions}));
  EXPECT_EQ(fix.ValidateFlags(), Status::kOk);

  auto result = fix.Transform(&reporter);
  EXPECT_TRUE(reporter.errors().empty());
  EXPECT_TRUE(reporter.warnings().empty());
  ASSERT_FALSE(result.is_ok());

  const std::vector<Error> errors = result.error_value().errors;
  EXPECT_EQ(result.error_value().status, Status::kErrorDuringFix);
  ASSERT_EQ(errors.size(), 1);
  EXPECT_EQ(errors[0].step, Step::kTransforming);
  EXPECT_SUBSTR(errors[0].msg, "bad.fidl:4:12");
  EXPECT_SUBSTR(errors[0].msg, "MyProtocol.StrictWithError");
  EXPECT_SUBSTR(errors[0].msg, "at version 1");
}

TEST(FixTests, GoodEmptyStructResponseFixFlexibleWithErrorInlinePayload) {
  GoodEmptyStructResponseFix(R"FIDL(library example;
open protocol MyProtocol {
    flexible FlexibleWithError() -> (struct {}) error uint32;
};
)FIDL",
                             R"FIDL(library example;
open protocol MyProtocol {
    flexible FlexibleWithError() -> () error uint32;
};
)FIDL");
}

TEST(FixTests, GoodEmptyStructResponseFixFlexibleWithErrorNamedPayload) {
  GoodEmptyStructResponseFix(R"FIDL(library example;
type MyEmptyStruct = struct {};
open protocol MyProtocol {
    flexible FlexibleWithError() -> (MyEmptyStruct) error uint32;
};
)FIDL",
                             R"FIDL(library example;
type MyEmptyStruct = struct {};
open protocol MyProtocol {
    flexible FlexibleWithError() -> () error uint32;
};
)FIDL");
}

TEST(FixTests, GoodEmptyStructResponseFixFlexibleWithErrorImportedPayload) {
  std::vector<std::string> deps = {R"FIDL(library dependency;
type MyEmptyStruct = struct {};
alias MyAliasedEmptyStruct = MyEmptyStruct;
)FIDL"};

  GoodEmptyStructResponseFix(R"FIDL(library example;
using dependency;
open protocol MyProtocol {
    flexible FlexibleWithError() -> (dependency.MyEmptyStruct) error uint32;
    flexible FlexibleWithErrorAliased() -> (dependency.MyAliasedEmptyStruct) error uint32;
};
)FIDL",
                             deps,
                             R"FIDL(library example;
using dependency;
open protocol MyProtocol {
    flexible FlexibleWithError() -> () error uint32;
    flexible FlexibleWithErrorAliased() -> () error uint32;
};
)FIDL");
}

TEST(FixTests, BadEmptyStructResponseFixFlexibleWithErrorVersionedPayload) {
  Reporter reporter;
  reporter.ignore_fixables();
  VersionSelection version_selection;
  version_selection.Insert(Platform::Parse("example").value(), Version::Head());
  auto library_manager = std::make_unique<SourceManager>();
  library_manager->AddSourceFile(
      std::make_unique<SourceFile>(kBadFileName, R"FIDL(@available(added=1)
library example;
open protocol MyProtocol {
    flexible FlexibleWithError() -> (struct {
      @available(removed=2)
      removed bool;
    }) error uint32;
};
)FIDL"));

  auto fix =
      EmptyStructResponseFix(std::move(library_manager), {}, &version_selection,
                             ExperimentalFlags({ExperimentalFlags::Flag::kSimpleEmptyResponseSyntax,
                                                ExperimentalFlags::Flag::kUnknownInteractions}));
  EXPECT_EQ(fix.ValidateFlags(), Status::kOk);

  auto result = fix.Transform(&reporter);
  EXPECT_TRUE(reporter.errors().empty());
  EXPECT_TRUE(reporter.warnings().empty());
  ASSERT_FALSE(result.is_ok());

  const std::vector<Error> errors = result.error_value().errors;
  EXPECT_EQ(result.error_value().status, Status::kErrorDuringFix);
  ASSERT_EQ(errors.size(), 1);
  EXPECT_EQ(errors[0].step, Step::kTransforming);
  EXPECT_SUBSTR(errors[0].msg, "bad.fidl:4:14");
  EXPECT_SUBSTR(errors[0].msg, "MyProtocol.FlexibleWithError");
  EXPECT_SUBSTR(errors[0].msg, "at version 1");
}

TEST(FixTests, GoodEmptyStructResponseFixFlexibleNoErrorInlinePayload) {
  GoodEmptyStructResponseFix(R"FIDL(library example;
open protocol MyProtocol {
    flexible FlexibleNoError() -> (struct {});
};
)FIDL",
                             R"FIDL(library example;
open protocol MyProtocol {
    flexible FlexibleNoError() -> ();
};
)FIDL");
}

TEST(FixTests, GoodEmptyStructResponseFixFlexibleNoErrorNamedPayload) {
  GoodEmptyStructResponseFix(R"FIDL(library example;
type MyEmptyStruct = struct {};
open protocol MyProtocol {
    flexible FlexibleNoError() -> (MyEmptyStruct);
};
)FIDL",
                             R"FIDL(library example;
type MyEmptyStruct = struct {};
open protocol MyProtocol {
    flexible FlexibleNoError() -> ();
};
)FIDL");
}

TEST(FixTests, GoodEmptyStructResponseFixFlexibleNoErrorImportedPayload) {
  std::vector<std::string> deps = {R"FIDL(library dependency;
type MyEmptyStruct = struct {};
alias MyAliasedEmptyStruct = MyEmptyStruct;
)FIDL"};

  GoodEmptyStructResponseFix(R"FIDL(library example;
using dependency;
open protocol MyProtocol {
    flexible FlexibleNoError() -> (dependency.MyEmptyStruct);
    flexible FlexibleNoErrorAliased() -> (dependency.MyAliasedEmptyStruct);
};
)FIDL",
                             deps,
                             R"FIDL(library example;
using dependency;
open protocol MyProtocol {
    flexible FlexibleNoError() -> ();
    flexible FlexibleNoErrorAliased() -> ();
};
)FIDL");
}

TEST(FixTests, BadEmptyStructResponseFixFlexibleNoErrorVersionedPayload) {
  Reporter reporter;
  reporter.ignore_fixables();
  VersionSelection version_selection;
  version_selection.Insert(Platform::Parse("example").value(), Version::Head());
  auto library_manager = std::make_unique<SourceManager>();
  library_manager->AddSourceFile(
      std::make_unique<SourceFile>(kBadFileName, R"FIDL(@available(added=1)
library example;
open protocol MyProtocol {
    flexible FlexibleNoError() -> (struct {
      @available(removed=2)
      removed bool;
    });
};
)FIDL"));

  auto fix =
      EmptyStructResponseFix(std::move(library_manager), {}, &version_selection,
                             ExperimentalFlags({ExperimentalFlags::Flag::kSimpleEmptyResponseSyntax,
                                                ExperimentalFlags::Flag::kUnknownInteractions}));
  EXPECT_EQ(fix.ValidateFlags(), Status::kOk);

  auto result = fix.Transform(&reporter);
  EXPECT_TRUE(reporter.errors().empty());
  EXPECT_TRUE(reporter.warnings().empty());
  ASSERT_FALSE(result.is_ok());

  const std::vector<Error> errors = result.error_value().errors;
  EXPECT_EQ(result.error_value().status, Status::kErrorDuringFix);
  ASSERT_EQ(errors.size(), 1);
  EXPECT_EQ(errors[0].step, Step::kTransforming);
  EXPECT_SUBSTR(errors[0].msg, "bad.fidl:4:14");
  EXPECT_SUBSTR(errors[0].msg, "MyProtocol.FlexibleNoError");
  EXPECT_SUBSTR(errors[0].msg, "at version 1");
}

TEST(FixTests, GoodEmptyStructResponseFixMultipleMethodsSingleProtocol) {
  GoodEmptyStructResponseFix(R"FIDL(library example;
type MyEmptyStruct = struct {};
open protocol MyProtocol {
    strict StrictWithError() -> (struct {}) error uint32;
    flexible FlexibleNoError() -> ();
    flexible FlexibleWithError() -> (MyEmptyStruct) error uint32;
};
)FIDL",
                             R"FIDL(library example;
type MyEmptyStruct = struct {};
open protocol MyProtocol {
    strict StrictWithError() -> () error uint32;
    flexible FlexibleNoError() -> ();
    flexible FlexibleWithError() -> () error uint32;
};
)FIDL");
}

TEST(FixTests, GoodEmptyStructResponseFixMultipleMethodsMultipleProtocols) {
  GoodEmptyStructResponseFix(R"FIDL(library example;
type MyEmptyStruct = struct {};
open protocol MyProtocol {
    strict StrictWithError() -> (struct {}) error uint32;
    flexible FlexibleNoError() -> ();
    flexible FlexibleWithError() -> (MyEmptyStruct) error uint32;
};
closed protocol MyOtherProtocol {
    strict StrictWithError() -> (struct {}) error uint32;
};
)FIDL",
                             R"FIDL(library example;
type MyEmptyStruct = struct {};
open protocol MyProtocol {
    strict StrictWithError() -> () error uint32;
    flexible FlexibleNoError() -> ();
    flexible FlexibleWithError() -> () error uint32;
};
closed protocol MyOtherProtocol {
    strict StrictWithError() -> () error uint32;
};
)FIDL");
}

}  // namespace
