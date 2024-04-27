// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/ld/abi.h>
#include <lib/stdcompat/string_view.h>
#include <unistd.h>

#include <cinttypes>
#include <string>

#include <gtest/gtest.h>

#ifdef __Fuchsia__
#include "ld-remote-process-tests.h"
#include "ld-startup-create-process-tests.h"
#include "ld-startup-in-process-tests-zircon.h"
#include "ld-startup-spawn-process-tests-zircon.h"
#else
#include "ld-startup-in-process-tests-posix.h"
#include "ld-startup-spawn-process-tests-posix.h"
#endif

namespace {

using namespace std::literals;

using elfldltl::testing::ExpectedErrorList;
using elfldltl::testing::ExpectReport;

template <class Fixture>
using LdLoadTests = Fixture;

template <class Fixture>
using LdLoadFailureTests = Fixture;

// This lists all the types that are compatible with both LdLoadTests and LdLoadFailureTests.
template <class... Tests>
using TestTypes = ::testing::Types<
// TODO(https://fxbug.dev/42080760): The separate-process tests require symbolic
// relocation so they can make the syscall to exit. The spawn-process
// tests also need a loader service to get ld.so.1 itself.
#ifdef __Fuchsia__
    ld::testing::LdStartupCreateProcessTests<>,  //
    ld::testing::LdRemoteProcessTests,
#else
    ld::testing::LdStartupSpawnProcessTests,
#endif
    Tests...>;

// This types are meaningul for the successful tests, LdLoadTests.
using LoadTypes = TestTypes<ld::testing::LdStartupInProcessTests>;

// These types are the types which are compatible with the failure tests, LdLoadFailureTests.
using FailTypes = TestTypes<>;

TYPED_TEST_SUITE(LdLoadTests, LoadTypes);
TYPED_TEST_SUITE(LdLoadFailureTests, FailTypes);

TYPED_TEST(LdLoadTests, Basic) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("ret17"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, PartialPageBss) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("partial-page-bss"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, PartialPageBssGarbageAtEof) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("partial-page-bss-garbage-at-eof"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, Relative) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("relative-reloc"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, Symbolic) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("symbolic-reloc"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, LoadWithNeeded) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  // There is only a reference to ld.so which doesn't need to be loaded to satisfy.
  ASSERT_NO_FATAL_FAILURE(this->Needed(std::initializer_list<std::string_view>{}));

  ASSERT_NO_FATAL_FAILURE(this->Load("ld-dep"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, BasicDep) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libld-dep-a.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("basic-dep"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, IndirectDeps) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({
      "libindirect-deps-a.so",
      "libindirect-deps-b.so",
      "libindirect-deps-c.so",
  }));

  ASSERT_NO_FATAL_FAILURE(this->Load("indirect-deps"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, PassiveAbiBasic) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("passive-abi-basic"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, PassiveAbiRdebug) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("passive-abi-rdebug"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, SymbolicNamespace) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libld-dep-a.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("symbolic-namespace"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, ManyDeps) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({
      "libld-dep-a.so",
      "libld-dep-b.so",
      "libld-dep-f.so",
      "libld-dep-c.so",
      "libld-dep-d.so",
      "libld-dep-e.so",
  }));

  ASSERT_NO_FATAL_FAILURE(this->Load("many-deps"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, PassiveAbiManyDeps) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({
      "libld-dep-a.so",
      "libld-dep-b.so",
      "libld-dep-f.so",
      "libld-dep-c.so",
      "libld-dep-d.so",
      "libld-dep-e.so",
  }));

  ASSERT_NO_FATAL_FAILURE(this->Load("passive-abi-many-deps"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, InitFini) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("init-fini"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, TlsExecOnly) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("tls-exec-only"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, TlsShlibOnly) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libtls-dep.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("tls-shlib-only"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, TlsExecShlib) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libtls-dep.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("tls-exec-shlib"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, TlsInitialExecAccess) {
  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libtls-ie-dep.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("tls-ie"));

  EXPECT_EQ(this->Run(), kReturnValue);

  this->ExpectLog("");
}

TYPED_TEST(LdLoadTests, TlsGlobalDynamicAccess) {
  constexpr int64_t kReturnValue = 17;
  constexpr int64_t kSkipReturnValue = 77;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libtls-dep.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("tls-gd"));

  const int64_t return_value = this->Run();

  // Check the log before the return value so we've handled it in case we skip.
  this->ExpectLog("");

  if (return_value == kSkipReturnValue) {
    GTEST_SKIP() << "tls-gd module compiled with TLSDESC";
  }

  EXPECT_EQ(return_value, kReturnValue);
}

TYPED_TEST(LdLoadTests, TlsDescAccess) {
  constexpr int64_t kReturnValue = 17;
  constexpr int64_t kSkipReturnValue = 77;

  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libtls-desc-dep.so"}));

  ASSERT_NO_FATAL_FAILURE(this->Load("tls-desc"));

  const int64_t return_value = this->Run();

  // Check the log before the return value so we've handled it in case we skip.
  this->ExpectLog("");

  if (return_value == kSkipReturnValue) {
    GTEST_SKIP() << "tls-desc module compiled without TLSDESC";
  }

  EXPECT_EQ(return_value, kReturnValue);
}

std::vector<std::string> SplitLogLines(std::string_view log) {
  std::vector<std::string> log_lines;
  while (!log.empty()) {
    size_t n = log.find('\n');
    EXPECT_NE(n, std::string_view::npos) << "last log line unterminated: " << log;
    if (n == std::string_view::npos) {
      break;
    }
    log_lines.emplace_back(log.substr(0, n));
    log.remove_prefix(n + 1);
  }
  return log_lines;
}

struct TestModule {
  std::string_view name, build_id;
  std::vector<elfldltl::Elf<>::Phdr> phdrs;
};

struct TestModuleNameCmp {
  using is_transparent = std::true_type;

  bool operator()(const TestModule& module1, const TestModule& module2) const {
    return module1.name < module2.name;
  }

  bool operator()(const TestModule& module, std::string_view name) const {
    return module.name < name;
  }

  bool operator()(std::string_view name, const TestModule& module) const {
    return name < module.name;
  }
};

const std::set<TestModule, TestModuleNameCmp> kTestModules{{
#include "load-test-modules.inc"
}};

const TestModule& GetTestModule(std::string_view name) {
  auto it = kTestModules.find(name);
  EXPECT_NE(it, kTestModules.end()) << name << " not found in kTestModules!";
  if (it == kTestModules.end()) {
    // Just return something rather than crashing.
    it = kTestModules.begin();
  }
  return *it;
}

std::string TestModuleMarkup(const TestModule& module, size_t idx,
                             std::optional<std::string_view> name = std::nullopt) {
  return std::string("{{{module:") + std::to_string(idx) + ":" +
         std::string(name.value_or(module.name)) + ":elf:" + std::string(module.build_id) + "}}}";
}

TYPED_TEST(LdLoadTests, SymbolizerMarkup) {
  if constexpr (!TestFixture::kCanCollectLog) {
    GTEST_SKIP() << "test requires log capture";
  }

  constexpr int64_t kReturnValue = 17;

  ASSERT_NO_FATAL_FAILURE(this->Init({}, {"LD_DEBUG=1"}));

  ASSERT_NO_FATAL_FAILURE(this->Needed({
      "libindirect-deps-a.so",
      "libindirect-deps-b.so",
      "libindirect-deps-c.so",
  }));

  ASSERT_NO_FATAL_FAILURE(this->Load("indirect-deps"));

  EXPECT_EQ(this->Run(), kReturnValue);

  // Collect the log and slice it into lines.
  std::vector<std::string> log_lines = SplitLogLines(this->CollectLog());

  // There should be at least one module line and one mmap line per module.
  EXPECT_GE(log_lines.size(), 2u * 5u);

  cpp20::span log_lines_left{log_lines};
  auto next_line = [left = cpp20::span{log_lines}]() mutable -> std::optional<std::string_view> {
    if (left.empty()) {
      return std::nullopt;
    }
    std::string_view line = left.front();
    left = left.subspan(1);
    return line;
  };

  auto expect_mmap = [page_size = static_cast<size_t>(sysconf(_SC_PAGE_SIZE)), &next_line](
                         const TestModule& module, size_t idx) {
    for (const auto& phdr : module.phdrs) {
      const uint64_t vaddr = phdr.vaddr & -page_size;
      const uint64_t memsz = ((phdr.vaddr + phdr.memsz + page_size - 1) & -page_size) - vaddr;
      std::string flags;
      if (phdr.flags & elfldltl::Elf<>::Phdr::kRead) {
        flags += 'r';
      }
      if (phdr.flags & elfldltl::Elf<>::Phdr::kWrite) {
        flags += 'w';
      }
      if (phdr.flags & elfldltl::Elf<>::Phdr::kExecute) {
        flags += 'x';
      }
      uint64_t mmap_memsz, mmap_file_vaddr;
      size_t mmap_modid;
      char mmap_flags[5];
      std::optional<std::string_view> line = next_line();
      ASSERT_TRUE(line) << "missing mmap line for " << module.name << " after "
                        << &phdr - module.phdrs.data() << " phdrs";
      ASSERT_EQ(sscanf(std::string(*line).c_str(),
                       "{{{mmap:%*x:%" PRIx64 ":load:%zi:%4[^:]:%" PRIx64 " }}}", &mmap_memsz,
                       &mmap_modid, mmap_flags, &mmap_file_vaddr),
                4)
          << *line << " for " << module.name;
      EXPECT_EQ(mmap_modid, idx) << *line << " for " << module.name;
      EXPECT_EQ(mmap_file_vaddr, vaddr) << *line << " for " << module.name;
      EXPECT_EQ(mmap_memsz, memsz) << *line << " for " << module.name;
      EXPECT_EQ(std::string(mmap_flags), flags) << *line << " for " << module.name;
    }
  };

  auto expect_module = [&expect_mmap, &next_line](
                           std::string_view name, size_t idx,

                           std::optional<std::string_view> modname = std::nullopt,
                           bool skip_mmap = false) {
    const TestModule& module = GetTestModule(name);
    std::optional<std::string_view> line = next_line();

    // If skip_mmap is true, we just saw a module line for a module whose
    // segment details we don't know, so just expect to see some number of mmap
    // lines for it before the next module we're looking for.
    while (skip_mmap && line && cpp20::starts_with(*line, "{{{mmap:")) {
      line = next_line();
    }

    EXPECT_THAT(line, ::testing::Optional(::testing::StrEq(TestModuleMarkup(module, idx, modname))))
        << "Expected " << name << " (" << modname.value_or("<nullopt>") << ") at ID " << idx
        << " but got: " << line.value_or("<EOF>"sv);
    expect_mmap(module, idx);
  };

  expect_module("indirect-deps"s + std::string(TestFixture::kTestExecutableSuffix), 0,
                "<application>");

  size_t idx = 1;
  if constexpr (!TestFixture::kTestExecutableNeedsVdso.empty()) {
    // The vDSO will be the first dependency.  We don't know its build ID,
    // so just check the name.
    std::optional<std::string_view> line = next_line();
    EXPECT_THAT(
        line, ::testing::Optional(::testing::StartsWith(
                  "{{{module:1:"s + std::string(TestFixture::kTestExecutableNeedsVdso) + ":elf:")))
        << line.value_or("<EOF>"sv);
    ++idx;
  }

  expect_module("libindirect-deps-a.so", idx++, std::nullopt,
                // If we expected the vDSO module line, we expect mmap lines
                // for it too, though we don't know what they'll say.
                !TestFixture::kTestExecutableNeedsVdso.empty());

  expect_module("libindirect-deps-b.so", idx++);

  expect_module("libindirect-deps-c.so", idx++);

  // None of these modules link against ld.so itself, and on non-Fuchsia none
  // links against the vDSO.  But those appear at the end of the list anyway.
  const TestModule& ld_module = GetTestModule(ld::abi::Abi<>::kSoname.str());
  std::optional<std::string_view> module_line = next_line();
  ASSERT_THAT(module_line, ::testing::Optional(
                               ::testing::StartsWith("{{{module:"s + std::to_string(idx) + ":")));
  if (TestFixture::kTestExecutableNeedsVdso.empty() &&
      !cpp20::starts_with(*module_line, "{{{module:"s + std::to_string(idx) + ":ld.so.1")) {
    // This must be the vDSO.  The ld.so module will be after it.  We don't
    // know what the vDSO's mmap lines should look like, so just skip them all.
    ++idx;
    std::optional<std::string_view> line;
    do {
      line = next_line();
    } while (line && cpp20::starts_with(*line, "{{{mmap:"));
    module_line = line;
  }

  // Finally the ld.so module will appear.
  EXPECT_EQ(*module_line, TestModuleMarkup(ld_module, idx, ld::abi::Abi<>::kSoname.str()));
  expect_mmap(ld_module, idx);

  // There should be no more log lines after that.
  std::optional<std::string_view> tail = next_line();
  EXPECT_EQ(tail, std::nullopt) << *tail;
}

TYPED_TEST(LdLoadFailureTests, MissingSymbol) {
  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({"libld-dep-a.so"}));

  ASSERT_NO_FATAL_FAILURE(  //
      this->LoadAndFail("missing-sym", ExpectedErrorList{
                                           ExpectReport{
                                               "undefined symbol: ",
                                               "b",
                                           },
                                       }));
}

TYPED_TEST(LdLoadFailureTests, MissingDependency) {
  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Needed({std::pair{"libmissing-dep-dep.so", false}}));

  ASSERT_NO_FATAL_FAILURE(  //
      this->LoadAndFail("missing-dep", ExpectedErrorList{
                                           ExpectReport{
                                               "cannot open dependency: ",
                                               "libmissing-dep-dep.so",
                                           },
                                       }));
}

TYPED_TEST(LdLoadFailureTests, Relro) {
  ASSERT_NO_FATAL_FAILURE(this->Init());

  ASSERT_NO_FATAL_FAILURE(this->Load("relro"));

  EXPECT_EQ(this->Run(), this->kRunFailureForBadPointer);

  this->ExpectLog("");
}

}  // namespace
