// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <algorithm>
#include <iomanip>
#include <iostream>
#include <set>
#include <stack>

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/include/fidl/raw_ast.h"
#include "tools/fidl/fidlc/include/fidl/tree_visitor.h"
#include "tools/fidl/fidlc/tests/test_library.h"

using NodeKind = fidl::raw::SourceElement::NodeKind;

// This test provides a way to write comprehensive unit tests on the fidlc
// parser. Each test case provides a SourceElement type and a list of source
// strings, with expected source spans of that type marked with special
// characters (see kMarkerLeft and kMarkerRight). The markers can be nested and
// are expected to specify all occurrences of that type of SourceElement.

// Test cases are defined near the bottom of the file as a
// std::vector<TestCase>.

// For each test case:
// - extract_expected_span_views creates a multiset of source spans and source signatures from a
// marked
//   source string.
// - |SourceSpanChecker| inherits from |TreeVisitor|, and it collects all the actual
//   spans and signatured of a given |SourceElement::NodeKind| by walking the AST in each test case.
// - then the expected values are compared against the actual values via set
//   arithmetic.

namespace {

#define FOR_ENUM_VARIANTS(DO)   \
  DO(AliasDeclaration)          \
  DO(Attribute)                 \
  DO(AttributeArg)              \
  DO(AttributeList)             \
  DO(BinaryOperatorConstant)    \
  DO(BoolLiteral)               \
  DO(CompoundIdentifier)        \
  DO(ConstDeclaration)          \
  DO(DocCommentLiteral)         \
  DO(File)                      \
  DO(Identifier)                \
  DO(IdentifierConstant)        \
  DO(IdentifierLayoutParameter) \
  DO(InlineLayoutReference)     \
  DO(LayoutParameterList)       \
  DO(LibraryDeclaration)        \
  DO(LiteralConstant)           \
  DO(LiteralLayoutParameter)    \
  DO(Modifiers)                 \
  DO(NamedLayoutReference)      \
  DO(NumericLiteral)            \
  DO(Ordinal64)                 \
  DO(OrdinaledLayout)           \
  DO(OrdinaledLayoutMember)     \
  DO(ParameterList)             \
  DO(ProtocolCompose)           \
  DO(ProtocolDeclaration)       \
  DO(ProtocolMethod)            \
  DO(ResourceDeclaration)       \
  DO(ResourceProperty)          \
  DO(ServiceMember)             \
  DO(ServiceDeclaration)        \
  DO(StringLiteral)             \
  DO(StructLayout)              \
  DO(StructLayoutMember)        \
  DO(TypeConstraints)           \
  DO(TypeDeclaration)           \
  DO(TypeConstructor)           \
  DO(TypeLayoutParameter)       \
  DO(Using)                     \
  DO(ValueLayout)               \
  DO(ValueLayoutMember)

#define MAKE_ENUM_NAME(VAR) #VAR,
const std::string kNodeKindNames[] = {FOR_ENUM_VARIANTS(MAKE_ENUM_NAME)};

std::string element_node_kind_str(NodeKind type) {
  auto x = static_cast<std::underlying_type_t<NodeKind>>(type);
  return kNodeKindNames[x];
}

// Used to delineate spans in source code. E.g.,
// const uint32 «three» = 3;
constexpr std::string_view kMarkerLeft = "«";
constexpr std::string_view kMarkerRight = "»";

class SourceSpanVisitor : public fidl::raw::TreeVisitor {
 public:
  explicit SourceSpanVisitor(NodeKind test_case_node_kind)
      : test_case_node_kind_(test_case_node_kind) {}

  const std::multiset<std::string>& spans() { return spans_; }
  const std::set<fidl::raw::SourceElement::Signature>& source_signatures() {
    return source_signatures_;
  }

  void OnAliasDeclaration(const std::unique_ptr<fidl::raw::AliasDeclaration>& element) override {
    CheckSpanOfNodeKind(NodeKind::kAliasDeclaration, *element);
    TreeVisitor::OnAliasDeclaration(element);
  }
  void OnAttribute(const std::unique_ptr<fidl::raw::Attribute>& element) override {
    CheckSpanOfNodeKind(NodeKind::kAttribute, *element);
    TreeVisitor::OnAttribute(element);
  }
  void OnAttributeArg(const std::unique_ptr<fidl::raw::AttributeArg>& element) override {
    CheckSpanOfNodeKind(NodeKind::kAttributeArg, *element);
    TreeVisitor::OnAttributeArg(element);
  }
  void OnAttributeList(const std::unique_ptr<fidl::raw::AttributeList>& element) override {
    CheckSpanOfNodeKind(NodeKind::kAttributeList, *element);
    TreeVisitor::OnAttributeList(element);
  }
  void OnBinaryOperatorConstant(
      const std::unique_ptr<fidl::raw::BinaryOperatorConstant>& element) override {
    CheckSpanOfNodeKind(NodeKind::kBinaryOperatorConstant, *element);
    TreeVisitor::OnBinaryOperatorConstant(element);
  }
  void OnBoolLiteral(fidl::raw::BoolLiteral& element) override {
    CheckSpanOfNodeKind(NodeKind::kBoolLiteral, element);
    TreeVisitor::OnBoolLiteral(element);
  }
  void OnCompoundIdentifier(
      const std::unique_ptr<fidl::raw::CompoundIdentifier>& element) override {
    CheckSpanOfNodeKind(NodeKind::kCompoundIdentifier, *element);
    TreeVisitor::OnCompoundIdentifier(element);
  }
  void OnConstDeclaration(const std::unique_ptr<fidl::raw::ConstDeclaration>& element) override {
    CheckSpanOfNodeKind(NodeKind::kConstDeclaration, *element);
    TreeVisitor::OnConstDeclaration(element);
  }
  void OnDocCommentLiteral(fidl::raw::DocCommentLiteral& element) override {
    CheckSpanOfNodeKind(NodeKind::kDocCommentLiteral, element);
    TreeVisitor::OnDocCommentLiteral(element);
  }
  void OnFile(const std::unique_ptr<fidl::raw::File>& element) override {
    CheckSpanOfNodeKind(NodeKind::kFile, *element);
    TreeVisitor::OnFile(element);
  }
  void OnIdentifier(const std::unique_ptr<fidl::raw::Identifier>& element) override {
    CheckSpanOfNodeKind(NodeKind::kIdentifier, *element);
  }
  void OnIdentifierConstant(
      const std::unique_ptr<fidl::raw::IdentifierConstant>& element) override {
    CheckSpanOfNodeKind(NodeKind::kIdentifierConstant, *element);
    TreeVisitor::OnIdentifierConstant(element);
  }
  void OnIdentifierLayoutParameter(
      const std::unique_ptr<fidl::raw::IdentifierLayoutParameter>& element) override {
    CheckSpanOfNodeKind(NodeKind::kIdentifierLayoutParameter, *element);
    TreeVisitor::OnIdentifierLayoutParameter(element);
  }
  void OnInlineLayoutReference(
      const std::unique_ptr<fidl::raw::InlineLayoutReference>& element) override {
    CheckSpanOfNodeKind(NodeKind::kInlineLayoutReference, *element);
    TreeVisitor::OnInlineLayoutReference(element);
  }
  void OnLayout(const std::unique_ptr<fidl::raw::Layout>& element) override {
    switch (element->kind) {
      case fidl::raw::Layout::kBits:
      case fidl::raw::Layout::kEnum:
        CheckSpanOfNodeKind(NodeKind::kValueLayout, *element);
        break;
      case fidl::raw::Layout::kStruct:
        CheckSpanOfNodeKind(NodeKind::kStructLayout, *element);
        break;
      case fidl::raw::Layout::kTable:
      case fidl::raw::Layout::kOverlay:
      case fidl::raw::Layout::kUnion:
        CheckSpanOfNodeKind(NodeKind::kOrdinaledLayout, *element);
        break;
    }
    TreeVisitor::OnLayout(element);
  }
  void OnLayoutParameterList(
      const std::unique_ptr<fidl::raw::LayoutParameterList>& element) override {
    CheckSpanOfNodeKind(NodeKind::kLayoutParameterList, *element);
    TreeVisitor::OnLayoutParameterList(element);
  }
  void OnLibraryDeclaration(
      const std::unique_ptr<fidl::raw::LibraryDeclaration>& element) override {
    CheckSpanOfNodeKind(NodeKind::kLibraryDeclaration, *element);
    TreeVisitor::OnLibraryDeclaration(element);
  }
  void OnLiteralConstant(const std::unique_ptr<fidl::raw::LiteralConstant>& element) override {
    CheckSpanOfNodeKind(NodeKind::kLiteralConstant, *element);
    TreeVisitor::OnLiteralConstant(element);
  }
  void OnLiteralLayoutParameter(
      const std::unique_ptr<fidl::raw::LiteralLayoutParameter>& element) override {
    CheckSpanOfNodeKind(NodeKind::kLiteralLayoutParameter, *element);
    TreeVisitor::OnLiteralLayoutParameter(element);
  }
  void OnModifiers(const std::unique_ptr<fidl::raw::Modifiers>& element) override {
    CheckSpanOfNodeKind(NodeKind::kModifiers, *element);
    TreeVisitor::OnModifiers(element);
  }
  void OnNamedLayoutReference(
      const std::unique_ptr<fidl::raw::NamedLayoutReference>& element) override {
    CheckSpanOfNodeKind(NodeKind::kNamedLayoutReference, *element);
    TreeVisitor::OnNamedLayoutReference(element);
  }
  void OnNumericLiteral(fidl::raw::NumericLiteral& element) override {
    CheckSpanOfNodeKind(NodeKind::kNumericLiteral, element);
    TreeVisitor::OnNumericLiteral(element);
  }
  void OnOrdinal64(fidl::raw::Ordinal64& element) override {
    CheckSpanOfNodeKind(NodeKind::kOrdinal64, element);
    TreeVisitor::OnOrdinal64(element);
  }
  void OnOrdinaledLayoutMember(
      const std::unique_ptr<fidl::raw::OrdinaledLayoutMember>& element) override {
    CheckSpanOfNodeKind(NodeKind::kOrdinaledLayoutMember, *element);
    TreeVisitor::OnOrdinaledLayoutMember(element);
  }
  void OnParameterList(const std::unique_ptr<fidl::raw::ParameterList>& element) override {
    CheckSpanOfNodeKind(NodeKind::kParameterList, *element);
    TreeVisitor::OnParameterList(element);
  }
  void OnProtocolCompose(const std::unique_ptr<fidl::raw::ProtocolCompose>& element) override {
    CheckSpanOfNodeKind(NodeKind::kProtocolCompose, *element);
    TreeVisitor::OnProtocolCompose(element);
  }
  void OnProtocolDeclaration(
      const std::unique_ptr<fidl::raw::ProtocolDeclaration>& element) override {
    CheckSpanOfNodeKind(NodeKind::kProtocolDeclaration, *element);
    TreeVisitor::OnProtocolDeclaration(element);
  }
  void OnProtocolMethod(const std::unique_ptr<fidl::raw::ProtocolMethod>& element) override {
    CheckSpanOfNodeKind(NodeKind::kProtocolMethod, *element);
    TreeVisitor::OnProtocolMethod(element);
  }
  void OnResourceDeclaration(
      const std::unique_ptr<fidl::raw::ResourceDeclaration>& element) override {
    CheckSpanOfNodeKind(NodeKind::kResourceDeclaration, *element);
    TreeVisitor::OnResourceDeclaration(element);
  }
  void OnResourceProperty(const std::unique_ptr<fidl::raw::ResourceProperty>& element) override {
    CheckSpanOfNodeKind(NodeKind::kResourceProperty, *element);
    TreeVisitor::OnResourceProperty(element);
  }
  void OnServiceDeclaration(
      const std::unique_ptr<fidl::raw::ServiceDeclaration>& element) override {
    CheckSpanOfNodeKind(NodeKind::kServiceDeclaration, *element);
    TreeVisitor::OnServiceDeclaration(element);
  }
  void OnServiceMember(const std::unique_ptr<fidl::raw::ServiceMember>& element) override {
    CheckSpanOfNodeKind(NodeKind::kServiceMember, *element);
    TreeVisitor::OnServiceMember(element);
  }
  void OnStringLiteral(fidl::raw::StringLiteral& element) override {
    CheckSpanOfNodeKind(NodeKind::kStringLiteral, element);
    TreeVisitor::OnStringLiteral(element);
  }
  void OnStructLayoutMember(
      const std::unique_ptr<fidl::raw::StructLayoutMember>& element) override {
    CheckSpanOfNodeKind(NodeKind::kStructLayoutMember, *element);
    TreeVisitor::OnStructLayoutMember(element);
  }
  void OnTypeConstraints(const std::unique_ptr<fidl::raw::TypeConstraints>& element) override {
    CheckSpanOfNodeKind(NodeKind::kTypeConstraints, *element);
    TreeVisitor::OnTypeConstraints(element);
  }
  void OnTypeConstructor(const std::unique_ptr<fidl::raw::TypeConstructor>& element) override {
    CheckSpanOfNodeKind(NodeKind::kTypeConstructor, *element);
    TreeVisitor::OnTypeConstructor(element);
  }
  void OnTypeDeclaration(const std::unique_ptr<fidl::raw::TypeDeclaration>& element) override {
    CheckSpanOfNodeKind(NodeKind::kTypeDeclaration, *element);
    TreeVisitor::OnTypeDeclaration(element);
  }
  void OnTypeLayoutParameter(
      const std::unique_ptr<fidl::raw::TypeLayoutParameter>& element) override {
    CheckSpanOfNodeKind(NodeKind::kTypeLayoutParameter, *element);
    TreeVisitor::OnTypeLayoutParameter(element);
  }
  void OnUsing(const std::unique_ptr<fidl::raw::Using>& element) override {
    CheckSpanOfNodeKind(NodeKind::kUsing, *element);
    TreeVisitor::OnUsing(element);
  }
  void OnValueLayoutMember(const std::unique_ptr<fidl::raw::ValueLayoutMember>& element) override {
    CheckSpanOfNodeKind(NodeKind::kValueLayoutMember, *element);
    TreeVisitor::OnValueLayoutMember(element);
  }

 private:
  // Called on every node of the AST that we visit. We collect spans of the |NodeKind| we are
  // looking for as we traverse the tree, and store them in a multiset.
  void CheckSpanOfNodeKind(const NodeKind node_kind, const fidl::raw::SourceElement& element) {
    if (node_kind != test_case_node_kind_) {
      return;
    }
    spans_.insert(std::string(element.span().data()));
    source_signatures_.insert(element.source_signature());
  }

  NodeKind test_case_node_kind_;
  std::multiset<std::string> spans_;
  std::set<fidl::raw::SourceElement::Signature> source_signatures_;
};

std::string replace_markers(std::string_view source, std::string_view left_replace,
                            std::string_view right_replace) {
  std::string result(source);

  const auto replace_all = [&](std::string_view pattern, std::string_view replace_with) {
    std::string::size_type i = result.find(pattern);
    while (i != std::string::npos) {
      result.replace(i, pattern.length(), replace_with);
      i = result.find(pattern, i + replace_with.length());
    }
  };

  replace_all(kMarkerLeft, left_replace);
  replace_all(kMarkerRight, right_replace);
  return result;
}

std::string remove_markers(std::string_view source) { return replace_markers(source, "", ""); }

// Extracts marked source spans from a given source string as |string_view|s into the
// |cleaned_source|. If source spans are incorrectly marked (missing or extra markers), returns
// empty set; otherwise, returns a multiset of expected spans.
std::multiset<std::string_view> extract_expected_span_views(std::string_view marked_source,
                                                            std::string_view clean_source,
                                                            std::vector<std::string>* errors) {
  std::stack<size_t> stack;
  std::multiset<std::string_view> spans;
  size_t left_marker_size = kMarkerLeft.length();
  size_t right_marker_size = kMarkerRight.length();
  size_t combined_marker_size = left_marker_size + right_marker_size;

  const auto match = [&](size_t i, std::string_view marker) {
    return marker.compare(marked_source.substr(i, marker.length())) == 0;
  };

  auto offset = [&](size_t i) {
    return i - (stack.size() * left_marker_size) - (spans.size() * combined_marker_size);
  };

  for (size_t i = 0; i < marked_source.length();) {
    if (match(i, kMarkerLeft)) {
      stack.push(offset(i));
      i += left_marker_size;
    } else if (match(i, kMarkerRight)) {
      if (stack.empty()) {
        std::stringstream error_msg;
        error_msg << "unexpected closing marker '" << kMarkerRight << "' at position " << i
                  << " in source string";
        errors->push_back(error_msg.str());
        // Return an empty set if errors
        spans.clear();
        break;
      }

      // const std::string span = remove_markers(source.substr(stack.top(),  // index of left marker
      //                                                       i - stack.top())  // length of span
      // );
      size_t pos_in_clean_source = stack.top();
      size_t count_in_clean_source = offset(i) - pos_in_clean_source;
      stack.pop();
      auto x = clean_source.substr(pos_in_clean_source, count_in_clean_source);
      spans.insert(x);
      i += right_marker_size;
    } else {
      i += 1;
    }
  }

  if (!stack.empty()) {
    std::stringstream error_msg;
    error_msg << "expected closing marker '" << kMarkerRight << "'";
    errors->push_back(error_msg.str());
    // Return an empty set if errors
    spans.clear();
  }

  return spans;
}

struct TestCase {
  NodeKind node_kind;
  std::vector<std::string> marked_sources;
};

const std::vector<TestCase> test_cases = {
    {NodeKind::kAliasDeclaration,
     {
         R"FIDL(library x; «alias Foo = uint8»;)FIDL",
         R"FIDL(library x; «alias Foo = vector<uint8>»;)FIDL",
     }},
    {NodeKind::kAttribute,
     {
         R"FIDL(library x; «@foo("foo")» «@bar» const MY_BOOL bool = false;)FIDL",
         R"FIDL(library x;
          «@foo("foo")»
          «@bar»
          const MY_BOOL bool = false;
         )FIDL",
         R"FIDL(library x;
          protocol Foo {
            Bar(«@foo» struct {});
          };
         )FIDL",
     }},
    {NodeKind::kAttributeArg,
     {
         R"FIDL(library x; @attr(«"foo"») const MY_BOOL bool = false;)FIDL",
         R"FIDL(library x; @attr(«a="foo"»,«b="bar"») const MY_BOOL bool = false;)FIDL",
         R"FIDL(library x;
          const MY_BOOL bool = false;
          @attr(«a=true»,«b=MY_BOOL»,«c="foo"»)
          const MY_OTHER_BOOL bool = false;
         )FIDL",
     }},
    {NodeKind::kAttributeList,
     {
         R"FIDL(library x; «@foo("foo") @bar» const MY_BOOL bool = false;)FIDL",
         R"FIDL(library x;
          «@foo("foo")
          @bar»
          const MY_BOOL bool = false;
         )FIDL",
         R"FIDL(library x;
          protocol Foo {
            Bar(«@foo» struct {});
          };
         )FIDL",
     }},
    {NodeKind::kBinaryOperatorConstant,
     {
         R"FIDL(library x;
          const one uint8 = 0x0001;
          const two_fifty_six uint16 = 0x0100;
          const two_fifty_seven uint16 = «one | two_fifty_six»;
         )FIDL",
         R"FIDL(library x; const two_fifty_seven uint16 = «0x0001 | 0x0100»;)FIDL",
     }},
    {NodeKind::kBoolLiteral,
     {
         R"FIDL(library x; const x bool = «true»;)FIDL",
         R"FIDL(library x; @attr(«true») const x bool = «true»;)FIDL",
         R"FIDL(library x; const x bool = «false»;)FIDL",
         R"FIDL(library x; @attr(«false») const x bool = «false»;)FIDL",
     }},
    {NodeKind::kCompoundIdentifier,
     {
         R"FIDL(library «foo.bar.baz»;)FIDL",
     }},
    {NodeKind::kConstDeclaration,
     {
         R"FIDL(library example;
          «const C_SIMPLE uint32   = 11259375»;
          «const C_HEX_S uint32    = 0xABCDEF»;
          «const C_HEX_L uint32    = 0XABCDEF»;
          «const C_BINARY_S uint32 = 0b101010111100110111101111»;
          «const C_BINARY_L uint32 = 0B101010111100110111101111»;
      )FIDL"}},
    {NodeKind::kDocCommentLiteral,
     {
         R"FIDL(library x;
          «/// Foo»
          const MY_BOOL bool = false;)FIDL",
     }},
    {NodeKind::kIdentifier,
     {
         R"FIDL(library «x»;
          type «MyEnum» = strict enum {
            «A» = 1;
          };
         )FIDL",
         R"FIDL(library «x»;
          type «MyStruct» = resource struct {
            «boolval» «bool»;
            «boolval» «resource»;
            «boolval» «flexible»;
            «boolval» «struct»;
          };
         )FIDL",
         R"FIDL(library «x»;
          type «MyUnion» = flexible union {
            1: «intval» «int64»;
            2: reserved;
          };
         )FIDL",
     }},
    {NodeKind::kIdentifierConstant,
     {
         R"FIDL(library x; const x bool = true; const y bool = «x»;)FIDL",
     }},
    {NodeKind::kIdentifierLayoutParameter,
     {
         R"FIDL(library x; type a = bool; const b uint8 = 4; type y = array<«a»,«b»>;)FIDL",
     }},
    {NodeKind::kInlineLayoutReference,
     {
         R"FIDL(library x;
          type S = «struct {
            intval int64;
            boolval bool = false;
            stringval string:MAX_STRING_SIZE;
            inner «union {
              1: floatval float64;
            }»:optional;
          }»;
         )FIDL",
         R"FIDL(library x;
          protocol P {
            M(«struct {
              intval int64;
              boolval bool = false;
              stringval string:MAX_STRING_SIZE;
              inner «union {
                1: floatval float64;
              }»:optional;
            }»);
          };
         )FIDL",
         R"FIDL(library x;
          protocol Foo {
            Bar(«@foo struct {}»);
          };
         )FIDL",
     }},
    {NodeKind::kLayoutParameterList,
     {
         R"FIDL(library x; type y = array«<uint8,4>»;)FIDL",
         R"FIDL(library x; type y = vector«<array«<uint8,4>»>»;)FIDL",
     }},
    {NodeKind::kLibraryDeclaration,
     {
         R"FIDL(«library x»; using y;)FIDL",
         R"FIDL(«library x.y.z»; using y;)FIDL",
     }},
    {NodeKind::kLiteralConstant,
     {
         R"FIDL(library x; const x bool = «true»;)FIDL",
         R"FIDL(library x; const x uint8 = «42»;)FIDL",
         R"FIDL(library x; const x string = «"hi"»;)FIDL",
     }},
    {NodeKind::kLiteralLayoutParameter,
     {
         R"FIDL(library x; type y = array<uint8,«4»>;)FIDL",
         R"FIDL(library x; type y = vector<array<uint8,«4»>>;)FIDL",
     }},
    {NodeKind::kModifiers,
     {
         // Layouts
         R"FIDL(library x; type MyBits = «flexible» bits { MY_VALUE = 1; };)FIDL",
         R"FIDL(library x; type MyBits = «strict» bits : uint32 { MY_VALUE = 1; };)FIDL",
         R"FIDL(library x; type MyEnum = «flexible» enum : uint32 { MY_VALUE = 1; };)FIDL",
         R"FIDL(library x; type MyEnum = «strict» enum { MY_VALUE = 1; };)FIDL",
         R"FIDL(library x; type MyStruct = «resource» struct {};)FIDL",
         R"FIDL(library x; type MyTable = «resource» table { 1: my_member bool; };)FIDL",
         R"FIDL(library x; type MyUnion = «resource» union { 1: my_member bool; };)FIDL",
         R"FIDL(library x; type MyUnion = «flexible» union { 1: my_member bool; };)FIDL",
         R"FIDL(library x; type MyUnion = «strict» union { 1: my_member bool; };)FIDL",
         R"FIDL(library x; type MyUnion = «resource strict» union { 1: my_member bool; };)FIDL",
         R"FIDL(library x; type MyEnum = @attr «flexible» enum : uint32 { MY_VALUE = 1; };)FIDL",
         R"FIDL(library x; type MyStruct = @attr «resource» struct {};)FIDL",
         R"FIDL(library x; type MyUnion = @attr «resource strict» union { 1: my_member bool; };)FIDL",
         // Note that the following 3 tests have union members named like modifiers.
         R"FIDL(library x; type MyUnion = «resource flexible» union { 1: my_member resource; };)FIDL",
         R"FIDL(library x; type MyUnion = «strict resource» union { 1: my_member flexible; };)FIDL",
         R"FIDL(library x; type MyUnion = «flexible resource» union { 1: my_member strict; };)FIDL",
         // Protocols
         R"FIDL(library x; «ajar» protocol MyProtocol {};)FIDL",
         R"FIDL(library x; «closed» protocol MyProtocol {};)FIDL",
         R"FIDL(library x; «open» protocol MyProtocol {};)FIDL",
         R"FIDL(library x; @attr «open» protocol MyProtocol {};)FIDL",
         // Methods
         R"FIDL(library x; «open» protocol MyProtocol { «flexible» MyMethod(); };)FIDL",
         R"FIDL(library x; «open» protocol MyProtocol { «strict» MyMethod(); };)FIDL",
         R"FIDL(library x; «open» protocol MyProtocol { @attr «strict» MyMethod(); };)FIDL",
         // Note that the following 3 tests have protocol methods named like modifiers.
         R"FIDL(library x; «open» protocol MyProtocol { «flexible» flexible(); strict(); };)FIDL",
         R"FIDL(library x; «open» protocol MyProtocol { «strict» strict(); flexible(); };)FIDL",
         R"FIDL(library x; «open» protocol MyProtocol { @attr «flexible» flexible(); @attr strict(); };)FIDL",
     }},
    {NodeKind::kNamedLayoutReference,
     {
         R"FIDL(library x;
          type S = struct {
            intval «int64»;
            boolval «bool» = false;
            stringval «string»:MAX_STRING_SIZE;
            inner struct {
              floatval «float64»;
              uintval «uint8» = 7;
              vecval «vector»<«vector»<Foo>>;
              arrval «array»<uint8,4>;
            };
          };
         )FIDL",
     }},
    {NodeKind::kNumericLiteral,
     {
         R"FIDL(library x; const x uint8 = «42»;)FIDL",
         R"FIDL(library x; @attr(«42») const x uint8 = «42»;)FIDL",
     }},
    {NodeKind::kOrdinal64,
     {
         R"FIDL(library x; type U = union { «1:» one uint8; };)FIDL",
     }},
    {NodeKind::kOrdinaledLayout,
     {
         R"FIDL(library x;
          type T = «resource table {
            1: intval int64;
          }»;
          type U = «flexible resource union {
            1: intval int64;
          }»:optional;
         )FIDL",
     }},
    {NodeKind::kOrdinaledLayoutMember,
     {
         R"FIDL(library x;
          type T = table {
            «1: intval int64»;
            «2: reserved»;
            «@attr 3: floatval float64»;
            «4: stringval string:100»;
            «5: inner union {
              «1: boolval bool»;
              «2: reserved»;
            }:optional»;
          };
         )FIDL",
     }},
    {NodeKind::kParameterList,
     {
         R"FIDL(library x; protocol X { Method«()» -> «()»; };)FIDL",
         R"FIDL(library x; protocol X { Method«(struct {})» -> «(struct {})»; };)FIDL",
         R"FIDL(library x; protocol X { Method«(struct { a int32; b bool; })» -> «(struct { c
         uint8; d bool; })»; };)FIDL",
         R"FIDL(library x; protocol X { -> Event«()»; };)FIDL",
         R"FIDL(library x; protocol X { -> Event«(struct {})»; };)FIDL",
         R"FIDL(library x; protocol X { -> Event«(struct { a int32; b bool; })»; };)FIDL",
     }},
    {NodeKind::kProtocolCompose,
     {
         R"FIDL(library x; protocol X { «compose OtherProtocol»; };)FIDL",
         R"FIDL(library x; protocol X { «@attr compose OtherProtocol»; };)FIDL",
         R"FIDL(library x; protocol X {
            «/// Foo
            compose OtherProtocol»;
          };)FIDL",
     }},
    {NodeKind::kProtocolDeclaration,
     {
         R"FIDL(library x; «protocol X {}»;)FIDL",
         R"FIDL(library x; «@attr protocol X { compose OtherProtocol; }»;)FIDL",
     }},
    {NodeKind::kProtocolMethod,
     {
         // One-way
         R"FIDL(library x; protocol X { «Method()»; };)FIDL",
         R"FIDL(library x; protocol X { «@attr Method(struct { a int32; b bool; })»; };)FIDL",
         // Two-way
         R"FIDL(library x; protocol X { «Method(struct { a int32; }) -> ()»; };)FIDL",
         R"FIDL(library x; protocol X { «@attr Method(struct { a int32; }) -> ()»; };)FIDL",
         R"FIDL(library x; protocol X { «Method(struct { a int32; }) -> (struct { res bool; })»;
         };)FIDL",
         R"FIDL(library x; protocol X { «Method(struct { a int32; }) -> (struct { res
         bool; res2 int32; })»; };)FIDL",
         // Two-way + error
         R"FIDL(library x; protocol X { «Method(struct { a int32; }) -> () error uint32»;
         };)FIDL",
         R"FIDL(library x; protocol X { «@attr Method(struct { a int32; }) -> () error
         uint32»; };)FIDL",
         R"FIDL(library x; protocol X { «Method(struct { a int32; }) ->
         (struct { res bool; }) error uint32»; };)FIDL",
         R"FIDL(library x; protocol X {
         «Method(struct { a int32; }) -> (struct { res bool; res2 int32; }) error uint32»;
         };)FIDL",
         // Event
         R"FIDL(library x; protocol X { «-> Event()»; };)FIDL",
         R"FIDL(library x; protocol X { «-> Event(struct { res bool; })»; };)FIDL",
         R"FIDL(library x; protocol X { «@attr -> Event(struct { res bool; res2 int32; })»;
         };)FIDL",
     }},
    {NodeKind::kResourceDeclaration, {R"FIDL(
     library example; «resource_definition Res : uint32 { properties { subtype Enum; };
     }»;)FIDL"}},
    {NodeKind::kResourceProperty, {R"FIDL(
     library example; resource_definition Res : uint32 { properties { «subtype Enum»; };
     };)FIDL"}},
    {NodeKind::kServiceDeclaration,
     {
         R"FIDL(library x; «service X {}»;)FIDL",
         R"FIDL(library x; protocol P {}; «service X { Z client_end:P; }»;)FIDL",
     }},
    {NodeKind::kServiceMember,
     {
         R"FIDL(library x; protocol P {}; service X { «Z client_end:P»; };)FIDL",
         R"FIDL(library x; protocol P {}; service X { «@attr Z client_end:P»; };)FIDL",
     }},
    {NodeKind::kStringLiteral,
     {
         R"FIDL(library x; const x string = «"hello"»;)FIDL",
         R"FIDL(library x; @attr(«"foo"») const x string = «"goodbye"»;)FIDL",
         R"FIDL(library x; @attr(a=«"foo"»,b=«"bar"») const MY_BOOL bool = false;)FIDL",
     }},
    {NodeKind::kStructLayout,
     {
         R"FIDL(library x;
          type S = «resource struct {
            intval int64;
          }»;
         )FIDL",
     }},
    {NodeKind::kStructLayoutMember,
     {
         R"FIDL(library x;
          type S = struct {
            «intval int64»;
            «boolval bool = false»;
            «@attr stringval string:100»;
            «inner struct {
              «floatval float64»;
              «uintval uint8 = 7»;
            }»;
          };
         )FIDL",
     }},
    {NodeKind::kTypeConstraints,
     {
         R"FIDL(library x; type y = array<uint8,4>;)FIDL",
         R"FIDL(library x; type y = vector<vector<uint8>:«16»>:«<16,optional>»;)FIDL",
         R"FIDL(library x; type y = union { 1: foo bool; }:«optional»;)FIDL",
         R"FIDL(library x; using zx; type y = zx.handle:«optional»;)FIDL",
         R"FIDL(library x; using zx; type y = zx.handle:«<VMO,zx.READ,optional>»;)FIDL",
     }},
    {NodeKind::kTypeConstructor,
     {
         R"FIDL(library x; const x «int32» = 1;)FIDL",
         R"FIDL(library x; const x «zx.handle:<VMO, zx.rights.READ, optional>» = 1;)FIDL",
         R"FIDL(library x; const x «Foo<«Bar<«zx.handle:VMO»>:20»>:optional» = 1;)FIDL",
         R"FIDL(library x; const x «zx.handle:VMO» = 1;)FIDL",
         R"FIDL(library x; type y = «array<uint8,4>»;)FIDL",
         R"FIDL(library x; type y = «vector<«array<Foo,4>»>»;)FIDL",
         R"FIDL(library x; type y = «string:100»;)FIDL",
         R"FIDL(library x; type y = «string:<100,optional>»;)FIDL",
         R"FIDL(library x;
          type e = «flexible enum : «uint32» {
            A = 1;
          }»;
         )FIDL",
         R"FIDL(library x;
          type S = «struct {
            intval «int64»;
            boolval «bool» = false;
            stringval «string:MAX_STRING_SIZE»;
            inner «struct {
              floatval «float64»;
              uintval «uint8» = 7;
              vecval «vector<«vector<Foo>»>»;
              arrval «array<uint8,4>»;
            }»;
          }»;
         )FIDL",
         R"FIDL(library x; protocol X { Method(«struct { a «int32»; b «bool»; }») -> («struct
         {}») error «uint32»; };)FIDL",
         R"FIDL(library x;
          resource_definition foo : «uint8» {
              properties {
                  rights «rights»;
              };
          };
         )FIDL",
         R"FIDL(library x;
          protocol Foo {
            Bar(«@foo struct {}»);
          };
         )FIDL",
     }},
    {NodeKind::kTypeDeclaration,
     {
         R"FIDL(library x;
          «type E = enum : int8 {
            A = 1;
          }»;
          «type S = struct {
            intval int64;
          }»;
          «type U = union {
            1: intval int64;
          }:optional»;
         )FIDL",
     }},
    {NodeKind::kTypeLayoutParameter,
     {
         R"FIDL(library x; type y = array<uint8,4>;)FIDL",
         R"FIDL(library x; type y = vector<«array<uint8,4>»>;)FIDL",
     }},
    {NodeKind::kUsing,
     {
         R"FIDL(library x; «using y»;)FIDL",
         R"FIDL(library x; «using y as z»;)FIDL",
     }},
    {NodeKind::kValueLayout,
     {
         R"FIDL(library x;
          type B = «bits {
            A = 1;
          }»;
          type E = «strict enum {
            A = 1;
          }»;
         )FIDL",
     }},
    {NodeKind::kValueLayoutMember,
     {
         R"FIDL(library x;
          type E = enum {
            «A = 1»;
            «@attr B = 2»;
          };
         )FIDL",
         R"FIDL(library x;
          type B = bits {
            «A = 0x1»;
            «@attr B = 0x2»;
          };
         )FIDL",
     }},
};

constexpr std::string_view kPassedMsg = "\x1B[32mPassed\033[0m";
constexpr std::string_view kFailedMsg = "\x1B[31mFailed\033[0m";
constexpr std::string_view kErrorMsg = "\x1B[31mERROR:\033[0m";

void RunParseTests(const std::vector<TestCase>& cases, const std::string& insert_left_padding,
                   const std::string& insert_right_padding, std::set<NodeKind> exclude) {
  std::cerr << '\n'
            << std::left << '\t' << "\x1B[34mWhere left padding = \"" << insert_left_padding
            << "\" and right padding = \"" << insert_right_padding << "\":\033[0m\n";

  bool all_passed = true;
  for (const auto& test_case : cases) {
    if (exclude.find(test_case.node_kind) != exclude.end()) {
      continue;
    }
    std::cerr << std::left << '\t' << std::setw(48) << element_node_kind_str(test_case.node_kind);
    std::vector<std::string> errors;

    for (const auto& unpadded_source : test_case.marked_sources) {
      // Insert the specified left/right padding.
      std::string marked_source =
          replace_markers(unpadded_source, insert_left_padding + kMarkerLeft.data(),
                          kMarkerRight.data() + insert_right_padding);
      std::string clean_source = remove_markers(marked_source);

      // Parse the source with markers removed
      TestLibrary library(clean_source);
      library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
      std::unique_ptr<fidl::raw::File> ast;
      if (!library.Parse(&ast)) {
        errors.push_back("failed to parse");
        break;
      }

      // Get the expected spans from the marked source
      std::multiset<std::string_view> span_views =
          extract_expected_span_views(marked_source, library.source_file().data(), &errors);
      // Returns an empty set when there are errors
      if (span_views.empty()) {
        break;
      }

      std::multiset<std::string> expected_spans;
      for (const auto& span_view : span_views) {
        expected_spans.insert(std::string(span_view));
      }

      // Convert each expected span into a |SourceElement::Signature| by using the |NodeKind|
      // supplied for the |test_case|.
      std::set<fidl::raw::SourceElement::Signature> expected_source_signatures;
      for (auto& span_view : span_views) {
        expected_source_signatures.insert(
            fidl::raw::SourceElement::Signature(test_case.node_kind, span_view));
      }

      // Get the actual spans and source signatures by walking the AST, then compare them against
      // our expectation.
      SourceSpanVisitor visitor(test_case.node_kind);
      visitor.OnFile(ast);
      std::multiset<std::string> actual_spans = visitor.spans();
      std::set<fidl::raw::SourceElement::Signature> actual_source_signatures =
          visitor.source_signatures();

      // Report errors where the checker found unexpected spans (spans in actual but not
      // expected).
      std::multiset<std::string> actual_spans_minus_expected;
      std::set_difference(
          actual_spans.begin(), actual_spans.end(), expected_spans.begin(), expected_spans.end(),
          std::inserter(actual_spans_minus_expected, actual_spans_minus_expected.begin()));
      for (const auto& span : actual_spans_minus_expected) {
        std::stringstream error_msg;
        error_msg << "unexpected occurrence of spans of type "
                  << element_node_kind_str(test_case.node_kind) << ": " << kMarkerLeft << span
                  << kMarkerRight;
        errors.push_back(error_msg.str());
      }

      // Report errors where the checker failed to find expected spans (spans in expected but not
      // actual).
      std::multiset<std::string> expected_spans_minus_actual;
      std::set_difference(
          expected_spans.begin(), expected_spans.end(), actual_spans.begin(), actual_spans.end(),
          std::inserter(expected_spans_minus_actual, expected_spans_minus_actual.begin()));
      for (const auto& span : expected_spans_minus_actual) {
        std::stringstream error_msg;
        error_msg << "expected (but didn't find) spans of type "
                  << element_node_kind_str(test_case.node_kind) << ": " << kMarkerLeft << span
                  << kMarkerRight;
        errors.push_back(error_msg.str());
      }

      // Report errors where the checker found unexpected source signatures (source signatures in
      // actual but not expected).
      std::set<fidl::raw::SourceElement::Signature> actual_source_signatures_minus_expected;
      std::set_difference(actual_source_signatures.begin(), actual_source_signatures.end(),
                          expected_source_signatures.begin(), expected_source_signatures.end(),
                          std::inserter(actual_source_signatures_minus_expected,
                                        actual_source_signatures_minus_expected.begin()));
      for (size_t i = 0; i < actual_source_signatures_minus_expected.size(); i++) {
        std::stringstream error_msg;
        error_msg << "unexpected occurrence of source signatures of type "
                  << element_node_kind_str(test_case.node_kind);
        errors.push_back(error_msg.str());
      }

      // Report errors where the checker failed to find expected source signatures (source
      // signatures in expected but not actual).
      std::set<fidl::raw::SourceElement::Signature> expected_source_signatures_minus_actual;
      std::set_difference(expected_source_signatures.begin(), expected_source_signatures.end(),
                          actual_source_signatures.begin(), actual_source_signatures.end(),
                          std::inserter(expected_source_signatures_minus_actual,
                                        expected_source_signatures_minus_actual.begin()));
      for (size_t i = 0; i < expected_source_signatures_minus_actual.size(); i++) {
        std::stringstream error_msg;
        error_msg << "expected (but didn't find) source signatures of type "
                  << element_node_kind_str(test_case.node_kind);
        errors.push_back(error_msg.str());
      }
    }

    if (errors.empty()) {
      std::cerr << kPassedMsg << '\n';
    } else {
      std::cerr << kFailedMsg << '\n';
      all_passed = false;
      for (const auto& error : errors) {
        std::cerr << "\t  " << kErrorMsg << ' ' << error << '\n';
      }
    }
  }

  // Assert after all tests are over so that we can get output for each test
  // case even if one of them fails.
  ASSERT_TRUE(all_passed, "At least one test case failed");
}

TEST(SpanTests, GoodParseTest) {
  RunParseTests(test_cases, "", "", {});
  RunParseTests(test_cases, " ", "", {});
  RunParseTests(test_cases, "", " ", {NodeKind::kDocCommentLiteral});
  RunParseTests(test_cases, " ", " ", {NodeKind::kDocCommentLiteral});
}

}  // namespace
