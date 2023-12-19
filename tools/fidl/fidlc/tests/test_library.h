// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_TESTS_TEST_LIBRARY_H_
#define TOOLS_FIDL_FIDLC_TESTS_TEST_LIBRARY_H_

#include <zircon/assert.h>

#include <cstring>
#include <fstream>
#include <type_traits>

#include <gtest/gtest.h>

#include "tools/fidl/fidlc/include/fidl/diagnostic_types.h"
#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/findings.h"
#include "tools/fidl/fidlc/include/fidl/flat/compiler.h"
#include "tools/fidl/fidlc/include/fidl/flat_ast.h"
#include "tools/fidl/fidlc/include/fidl/json_generator.h"
#include "tools/fidl/fidlc/include/fidl/lexer.h"
#include "tools/fidl/fidlc/include/fidl/linter.h"
#include "tools/fidl/fidlc/include/fidl/ordinals.h"
#include "tools/fidl/fidlc/include/fidl/parser.h"
#include "tools/fidl/fidlc/include/fidl/source_file.h"
#include "tools/fidl/fidlc/include/fidl/utils.h"
#include "tools/fidl/fidlc/include/fidl/versioning_types.h"
#include "tools/fidl/fidlc/include/fidl/virtual_source_file.h"

#define ASSERT_COMPILED(library)                         \
  {                                                      \
    TestLibrary& library_ref = (library);                \
    if (!library_ref.Compile()) {                        \
      EXPECT_EQ(library_ref.errors().size(), 0u);        \
      for (const auto& error : library_ref.errors()) {   \
        EXPECT_EQ(error->def.msg, "");                   \
      }                                                  \
      FAIL() << "stopping test, compilation failed";     \
    }                                                    \
    EXPECT_EQ(library_ref.warnings().size(), 0u);        \
    for (const auto& warning : library_ref.warnings()) { \
      EXPECT_EQ(warning->def.msg, "");                   \
    }                                                    \
  }

#define ASSERT_COMPILER_DIAGNOSTICS(library) \
  {                                          \
    TestLibrary& library_ref = (library);    \
    if (!library_ref.CheckCompile()) {       \
      FAIL() << "Diagnostics mismatch";      \
    }                                        \
  }

struct LintArgs {
 public:
  const std::set<std::string>& included_check_ids = {};
  const std::set<std::string>& excluded_check_ids = {};
  bool exclude_by_default = false;
  std::set<std::string>* excluded_checks_not_found = nullptr;
};

// Behavior that applies to SharedAmongstLibraries, but that is also provided on
// TestLibrary for convenience in single-library tests.
class SharedInterface {
 public:
  virtual fidl::Reporter* reporter() = 0;
  virtual fidl::flat::Libraries* all_libraries() = 0;
  virtual fidl::VersionSelection* version_selection() = 0;
  virtual fidl::ExperimentalFlags& experimental_flags() = 0;

  const std::vector<std::unique_ptr<fidl::Diagnostic>>& errors() { return reporter()->errors(); }
  const std::vector<std::unique_ptr<fidl::Diagnostic>>& warnings() {
    return reporter()->warnings();
  }
  std::vector<fidl::Diagnostic*> Diagnostics() { return reporter()->Diagnostics(); }
  void set_warnings_as_errors(bool value) { reporter()->set_warnings_as_errors(value); }
  void PrintReports() { reporter()->PrintReports(/*enable_color=*/false); }
  void SelectVersion(const std::string& platform, std::string_view version) {
    version_selection()->Insert(fidl::Platform::Parse(platform).value(),
                                fidl::Version::Parse(version).value());
  }
  void EnableFlag(fidl::ExperimentalFlags::Flag flag) { experimental_flags().EnableFlag(flag); }
};

// Stores data structures that are shared amongst all libraries being compiled
// together (i.e. the dependencies and the final library).
class SharedAmongstLibraries final : public SharedInterface {
 public:
  SharedAmongstLibraries() : all_libraries_(&reporter_, &virtual_file_) {}
  // Unsafe to copy/move because all_libraries_ stores a pointer to reporter_.
  SharedAmongstLibraries(const SharedAmongstLibraries&) = delete;
  SharedAmongstLibraries(SharedAmongstLibraries&&) = delete;

  // Adds and compiles a library similar to //zircon/vsdo/zx, defining "Handle",
  // "ObjType", and "Rights".
  void AddLibraryZx();

  // Adds and compiles a library defining fdf.handle and fdf.obj_type.
  void AddLibraryFdf();

  fidl::Reporter* reporter() override { return &reporter_; }
  fidl::flat::Libraries* all_libraries() override { return &all_libraries_; }
  fidl::VersionSelection* version_selection() override { return &version_selection_; }
  fidl::ExperimentalFlags& experimental_flags() override { return experimental_flags_; }

  std::vector<std::unique_ptr<fidl::SourceFile>>& all_sources_of_all_libraries() {
    return all_sources_of_all_libraries_;
  }

 private:
  fidl::Reporter reporter_;
  fidl::VirtualSourceFile virtual_file_{"generated"};
  fidl::flat::Libraries all_libraries_;
  std::vector<std::unique_ptr<fidl::SourceFile>> all_sources_of_all_libraries_;
  fidl::VersionSelection version_selection_;
  fidl::ExperimentalFlags experimental_flags_;
};

namespace internal {

// See ordinals_test.cc
inline fidl::raw::Ordinal64 GetGeneratedOrdinal64ForTesting(
    const std::vector<std::string_view>& library_name, const std::string_view& protocol_name,
    const std::string_view& selector_name, const fidl::raw::SourceElement& source_element) {
  static std::map<std::string, uint64_t> special_selectors = {
      {"ThisOneHashesToZero", 0},
      {"ClashOne", 456789},
      {"ClashOneReplacement", 987654},
      {"ClashTwo", 456789},
  };
  if (library_name.size() == 1 && library_name[0] == "methodhasher" &&
      (protocol_name == "Special" || protocol_name == "SpecialComposed")) {
    auto it = special_selectors.find(std::string(selector_name));
    ZX_ASSERT_MSG(it != special_selectors.end(), "only special selectors allowed");
    return fidl::raw::Ordinal64(source_element, it->second);
  }
  return fidl::ordinals::GetGeneratedOrdinal64(library_name, protocol_name, selector_name,
                                               source_element);
}

}  // namespace internal

// Helper template to allow passing either a string literal or `const Arg&`.
template <typename Arg>
struct StringOrArg {
  // NOLINTNEXTLINE(google-explicit-constructor)
  StringOrArg(const char* string) : string(string) {}
  template <typename From, typename = std::enable_if_t<std::is_convertible_v<From, Arg>>>
  // NOLINTNEXTLINE(google-explicit-constructor)
  StringOrArg(const From& arg) : string(fidl::internal::Display(static_cast<const Arg&>(arg))) {}
  std::string string;
};

// Test harness for a single library. To compile multiple libraries together,
// first default construct a SharedAmongstLibraries and then pass it to each
// TestLibrary, and compile them one at a time in dependency order.
class TestLibrary final : public SharedInterface {
 public:
  // Constructor for a single-library, single-file test.
  explicit TestLibrary(const std::string& raw_source_code) : TestLibrary() {
    AddSource("example.fidl", raw_source_code);
  }

  // Constructor for a single-library, multi-file test (call AddSource after).
  explicit TestLibrary() {
    owned_shared_.emplace();
    shared_ = &owned_shared_.value();
  }

  // Constructor for a multi-library, single-file test.
  explicit TestLibrary(SharedAmongstLibraries* shared, const std::string& filename,
                       const std::string& raw_source_code)
      : TestLibrary(shared) {
    AddSource(filename, raw_source_code);
  }

  // Constructor for a multi-library, multi-file test (call AddSource after).
  explicit TestLibrary(SharedAmongstLibraries* shared) : shared_(shared) {}

  ~TestLibrary() {
    ZX_ASSERT_MSG(used_,
                  "TestLibrary appears unused; did you forget to call Parse, Compile, or Lint?");
    ZX_ASSERT_MSG(
        expected_diagnostics_.empty(),
        "TestLibrary has expected diagnostics; did you forget to call ASSERT_COMPILER_DIAGNOSTICS?");
  }

  // Helper for making a single test library depend on library zx, without
  // requiring an explicit SharedAmongstLibraries.
  void UseLibraryZx() {
    ZX_ASSERT_MSG(!compilation_, "must call before compiling");
    owned_shared_.value().AddLibraryZx();
  }

  // Helper for making a single test library depend on library fdf, without
  // requiring an explicit SharedAmongstLibraries.
  void UseLibraryFdf() {
    ZX_ASSERT_MSG(!compilation_, "must call before compiling");
    owned_shared_.value().AddLibraryFdf();
  }

  fidl::Reporter* reporter() override { return shared_->reporter(); }
  fidl::flat::Libraries* all_libraries() override { return shared_->all_libraries(); }
  fidl::VersionSelection* version_selection() override { return shared_->version_selection(); }
  fidl::ExperimentalFlags& experimental_flags() override { return shared_->experimental_flags(); }

  void AddSource(const std::string& filename, const std::string& raw_source_code) {
    std::string source_code(raw_source_code);
    // NUL terminate the string.
    source_code.resize(source_code.size() + 1);
    auto file = std::make_unique<fidl::SourceFile>(filename, source_code);
    all_sources_.push_back(file.get());
    shared_->all_sources_of_all_libraries().push_back(std::move(file));
  }

  fidl::flat::AttributeSchema& AddAttributeSchema(std::string name) {
    return all_libraries()->AddAttributeSchema(std::move(name));
  }

  static std::string TestFilePath(const std::string& name) {
#ifndef TEST_DATA_ROOT
#error "TEST_DATA_ROOT must be defined"
#else
    // TEST_DATA_ROOT is expected to be the toolchain's output root path,
    // relative to the build root, e.g. "host_x64"
    return TEST_DATA_ROOT "/fidlc-tests/" + name;
#endif
  }

  // Read the source from an associated external file.
  void AddFile(const std::string& name) {
    auto path = TestFilePath(name);
    const std::ifstream reader(path);
    if (!reader) {
      ZX_PANIC("AddFile failed to read %s: errno = %s\n", path.c_str(), strerror(errno));
    }
    std::stringstream buffer;
    buffer << reader.rdbuf();
    AddSource(name, buffer.str());
  }

  // Record that a particular error is expected during the compile.
  // The args can either match the ErrorDef's argument types, or they can be string literals.
  template <fidl::ErrorId Id, typename... Args>
  void ExpectFail(const fidl::ErrorDef<Id, Args...>& def,
                  fidl::utils::identity_t<StringOrArg<Args>>... args) {
    expected_diagnostics_.push_back(fidl::internal::FormatDiagnostic(def.msg, args.string...));
  }

  // Record that a particular warning is expected during the compile.
  // The args can either match the WarningDef's argument types, or they can be string literals.
  template <fidl::ErrorId Id, typename... Args>
  void ExpectWarn(const fidl::WarningDef<Id, Args...>& def,
                  fidl::utils::identity_t<StringOrArg<Args>>... args) {
    expected_diagnostics_.push_back(fidl::internal::FormatDiagnostic(def.msg, args.string...));
  }

  // Check that the diagnostics expected with ExpectFail and ExpectWarn were recorded, in that order
  // by the compilation. This prints information about diagnostics mismatches and returns false if
  // any were found.
  bool CheckDiagnostics() {
    bool ok = true;
    size_t num_expected = expected_diagnostics_.size();
    size_t num_found = Diagnostics().size();
    for (size_t i = 0; i < std::max(num_expected, num_found); i++) {
      if (i < num_expected && i < num_found) {
        const std::string& expected = expected_diagnostics_[i];
        const std::string& found = Diagnostics()[i]->msg;
        const std::string found_at = Diagnostics()[i]->span.position_str();
        if (expected != found) {
          if (!ok) {
            fprintf(stderr, "\n");
          }
          fprintf(stderr, "Expected: %s\n   Found: %s\n      At: %s", expected.c_str(),
                  found.c_str(), found_at.c_str());
          ok = false;
        }
      } else if (i < num_found) {
        const std::string& found = Diagnostics()[i]->msg;
        const std::string found_at = Diagnostics()[i]->span.position_str();
        if (!ok) {
          fprintf(stderr, "\n");
        }
        fprintf(stderr, "Unexpected: %s\n        At: %s\n", found.c_str(), found_at.c_str());
        ok = false;
      } else if (i < num_expected) {
        const std::string& expected = expected_diagnostics_[i];
        if (!ok) {
          fprintf(stderr, "\n");
        }
        fprintf(stderr, "Expected: %s\n", expected.c_str());
        ok = false;
      }
    }
    expected_diagnostics_.clear();
    return ok;
  }

  // TODO(fxbug.dev/118282): remove (or rename this class to be more general), as this does not use
  // a library.
  bool Parse(std::unique_ptr<fidl::raw::File>* out_ast_ptr) {
    ZX_ASSERT_MSG(all_sources_.size() == 1, "parse can only be used with one source");
    used_ = true;
    auto source_file = all_sources_.at(0);
    fidl::Lexer lexer(*source_file, reporter());
    fidl::Parser parser(&lexer, reporter(), experimental_flags());
    out_ast_ptr->reset(parser.Parse().release());
    return parser.Success();
  }

  // Compiles the library. Must have compiled all dependencies first, using the
  // same SharedAmongstLibraries object for all of them.
  bool Compile() {
    used_ = true;
    fidl::flat::Compiler compiler(all_libraries(), version_selection(),
                                  internal::GetGeneratedOrdinal64ForTesting, experimental_flags());
    for (auto source_file : all_sources_) {
      fidl::Lexer lexer(*source_file, reporter());
      fidl::Parser parser(&lexer, reporter(), experimental_flags());
      auto ast = parser.Parse();
      if (!parser.Success())
        return false;
      if (!compiler.ConsumeFile(std::move(ast)))
        return false;
    }
    if (!compiler.Compile())
      return false;
    compilation_ = all_libraries()->Filter(version_selection());
    return true;
  }

  // Compiles the library and checks that the diagnostics asserted with
  bool CheckCompile() {
    bool compiled_ok = Compile();
    bool diagnostics_ok = CheckDiagnostics();
    // If the compile succeeded there should be no errors.
    ZX_ASSERT(compiled_ok == errors().empty());
    return diagnostics_ok;
  }

  bool Lint(LintArgs args = {}) {
    used_ = true;
    findings_ = fidl::Findings();

    bool passed = [&]() {
      ZX_ASSERT_MSG(all_sources_.size() == 1, "lint can only be used with one source");
      auto source_file = all_sources_.at(0);
      fidl::Lexer lexer(*source_file, reporter());
      fidl::Parser parser(&lexer, reporter(), experimental_flags());
      auto ast = parser.Parse();
      if (!parser.Success()) {
        std::string_view beginning(source_file->data().data(), 0);
        fidl::SourceSpan span(beginning, *source_file);
        const auto& error = errors().at(0);
        auto error_msg =
            fidl::Reporter::Format("error", error->span, error->Format(), /*color=*/false);
        findings_.emplace_back(span, "parser-error", error_msg + "\n");
        return false;
      }
      fidl::linter::Linter linter;
      if (!args.included_check_ids.empty()) {
        linter.set_included_checks(args.included_check_ids);
      }
      if (!args.excluded_check_ids.empty()) {
        linter.set_excluded_checks(args.excluded_check_ids);
      }
      linter.set_exclude_by_default(args.exclude_by_default);
      return linter.Lint(ast, &findings_, args.excluded_checks_not_found);
    }();

    lints_ = fidl::utils::FormatFindings(findings_, false);
    return passed;
  }

  std::string GenerateJSON() {
    auto json_generator = fidl::JSONGenerator(compilation_.get(), experimental_flags());
    auto out = json_generator.Produce();
    return out.str();
  }

  // Note: We don't provide a convenient library() method because inspecting a
  // Library is usually the wrong thing to do in tests. What usually matters is
  // the Compilation, for which we provide compilation() and helpers like
  // LookupStruct() etc. However, sometimes tests really need to get a Library*
  // (e.g. to construct Name::Key), hence this method.
  const fidl::flat::Library* LookupLibrary(std::string_view name) {
    std::vector<std::string_view> parts;
    size_t dot_idx = 0;
    for (size_t i = 0; dot_idx != std::string::npos; i = dot_idx + 1) {
      dot_idx = name.find('.', i);
      parts.push_back(name.substr(i, dot_idx));
    }
    auto library = all_libraries()->Lookup(parts);
    ZX_ASSERT_MSG(library, "library not found");
    return library;
  }

  const fidl::flat::Bits* LookupBits(std::string_view name) {
    for (const auto& bits_decl : compilation_->declarations.bits) {
      if (bits_decl->name.decl_name() == name) {
        return bits_decl;
      }
    }
    return nullptr;
  }

  const fidl::flat::Const* LookupConstant(std::string_view name) {
    for (const auto& const_decl : compilation_->declarations.consts) {
      if (const_decl->name.decl_name() == name) {
        return const_decl;
      }
    }
    return nullptr;
  }

  const fidl::flat::Enum* LookupEnum(std::string_view name) {
    for (const auto& enum_decl : compilation_->declarations.enums) {
      if (enum_decl->name.decl_name() == name) {
        return enum_decl;
      }
    }
    return nullptr;
  }

  const fidl::flat::Resource* LookupResource(std::string_view name) {
    for (const auto& resource_decl : compilation_->declarations.resources) {
      if (resource_decl->name.decl_name() == name) {
        return resource_decl;
      }
    }
    return nullptr;
  }

  const fidl::flat::Service* LookupService(std::string_view name) {
    for (const auto& service_decl : compilation_->declarations.services) {
      if (service_decl->name.decl_name() == name) {
        return service_decl;
      }
    }
    return nullptr;
  }

  const fidl::flat::Struct* LookupStruct(std::string_view name) {
    for (const auto& struct_decl : compilation_->declarations.structs) {
      if (struct_decl->name.decl_name() == name) {
        return struct_decl;
      }
    }
    return nullptr;
  }

  const fidl::flat::NewType* LookupNewType(std::string_view name) {
    for (const auto& new_type_decl : compilation_->declarations.new_types) {
      if (new_type_decl->name.decl_name() == name) {
        return new_type_decl;
      }
    }
    return nullptr;
  }

  const fidl::flat::Table* LookupTable(std::string_view name) {
    for (const auto& table_decl : compilation_->declarations.tables) {
      if (table_decl->name.decl_name() == name) {
        return table_decl;
      }
    }
    return nullptr;
  }

  const fidl::flat::Alias* LookupAlias(std::string_view name) {
    for (const auto& alias_decl : compilation_->declarations.aliases) {
      if (alias_decl->name.decl_name() == name) {
        return alias_decl;
      }
    }
    return nullptr;
  }

  const fidl::flat::Union* LookupUnion(std::string_view name) {
    for (const auto& union_decl : compilation_->declarations.unions) {
      if (union_decl->name.decl_name() == name) {
        return union_decl;
      }
    }
    return nullptr;
  }
  const fidl::flat::Overlay* LookupOverlay(std::string_view name) {
    for (const auto& overlay_decl : compilation_->declarations.overlays) {
      if (overlay_decl->name.decl_name() == name) {
        return overlay_decl;
      }
    }
    return nullptr;
  }

  const fidl::flat::Protocol* LookupProtocol(std::string_view name) {
    for (const auto& protocol_decl : compilation_->declarations.protocols) {
      if (protocol_decl->name.decl_name() == name) {
        return protocol_decl;
      }
    }
    return nullptr;
  }

  const fidl::SourceFile& source_file() const {
    ZX_ASSERT_MSG(all_sources_.size() == 1, "convenience method only possible with single source");
    return *all_sources_.at(0);
  }

  std::vector<const fidl::SourceFile*> source_files() const {
    std::vector<const fidl::SourceFile*> out;
    out.reserve(all_sources_.size());
    for (const auto& source : all_sources_) {
      out.push_back(source);
    }
    return out;
  }

  fidl::SourceSpan source_span(size_t start, size_t size) const {
    ZX_ASSERT_MSG(all_sources_.size() == 1, "convenience method only possible with single source");
    std::string_view data = all_sources_.at(0)->data();
    data.remove_prefix(start);
    data.remove_suffix(data.size() - size);
    return fidl::SourceSpan(data, *all_sources_.at(0));
  }

  fidl::SourceSpan find_source_span(std::string_view span_text) {
    ZX_ASSERT_MSG(all_sources_.size() == 1, "convenience method only possible with single source");
    std::string_view data = all_sources_.at(0)->data();
    size_t pos = data.find(span_text);
    ZX_ASSERT_MSG(pos != std::string_view::npos, "source span text not found");
    return source_span(pos, span_text.size());
  }

  const fidl::Findings& findings() const { return findings_; }

  const std::vector<std::string>& lints() const { return lints_; }

  const fidl::flat::Compilation* compilation() const {
    ZX_ASSERT_MSG(compilation_, "must compile successfully before accessing compilation");
    return compilation_.get();
  }

  const fidl::flat::AttributeList* attributes() { return compilation_->library_attributes; }

  const std::vector<const fidl::flat::Struct*>& external_structs() const {
    return compilation_->external_structs;
  }

  const std::vector<const fidl::flat::Decl*>& declaration_order() const {
    return compilation_->declaration_order;
  }

  const std::vector<fidl::flat::Compilation::Dependency>& direct_and_composed_dependencies() const {
    return compilation_->direct_and_composed_dependencies;
  }

 private:
  std::optional<SharedAmongstLibraries> owned_shared_;
  SharedAmongstLibraries* shared_;
  fidl::Findings findings_;
  std::vector<std::string> lints_;
  std::vector<fidl::SourceFile*> all_sources_;
  std::unique_ptr<fidl::flat::Compilation> compilation_;
  std::vector<std::string> expected_diagnostics_;
  bool used_ = false;
};

#endif  // TOOLS_FIDL_FIDLC_TESTS_TEST_LIBRARY_H_
