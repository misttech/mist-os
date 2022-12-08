// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/include/fidl/raw_ast.h"

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TREE_VISITOR_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TREE_VISITOR_H_

namespace fidl::raw {

// A TreeVisitor is an API that walks a FIDL AST.  The default implementation
// does nothing but walk the AST.  To make it interesting, subclass TreeVisitor
// and override behaviors with the ones you want.
class TreeVisitor {
 public:
  virtual ~TreeVisitor() = default;

  virtual void OnSourceElementStart(const SourceElement& element) {}
  virtual void OnSourceElementEnd(const SourceElement& element) {}
  virtual void OnIdentifier(const std::unique_ptr<Identifier>& element) { element->Accept(this); }
  virtual void OnCompoundIdentifier(const std::unique_ptr<CompoundIdentifier>& element) {
    element->Accept(this);
  }

  virtual void OnLiteral(const std::unique_ptr<fidl::raw::Literal>& element) {
    switch (element->kind) {
      case Literal::Kind::kDocComment:
        OnDocCommentLiteral(*static_cast<DocCommentLiteral*>(element.get()));
        break;
      case Literal::Kind::kString:
        OnStringLiteral(*static_cast<StringLiteral*>(element.get()));
        break;
      case Literal::Kind::kNumeric:
        OnNumericLiteral(*static_cast<NumericLiteral*>(element.get()));
        break;
      case Literal::Kind::kBool:
        OnBoolLiteral(*static_cast<BoolLiteral*>(element.get()));
        break;
    }
  }
  virtual void OnDocCommentLiteral(DocCommentLiteral& element) { element.Accept(this); }

  virtual void OnStringLiteral(StringLiteral& element) { element.Accept(this); }

  virtual void OnNumericLiteral(NumericLiteral& element) { element.Accept(this); }

  virtual void OnBoolLiteral(BoolLiteral& element) { element.Accept(this); }

  virtual void OnOrdinal64(Ordinal64& element) { element.Accept(this); }

#ifdef DISPATCH_TO
#error "Cannot define macro DISPATCH_TO: already defined"
#endif
#define DISPATCH_TO(TYPE, SUPERTYPE, ELEMENT)              \
  do {                                                     \
    std::unique_ptr<SUPERTYPE>& unconst_element =          \
        const_cast<std::unique_ptr<SUPERTYPE>&>(element);  \
    TYPE* ptr = static_cast<TYPE*>(unconst_element.get()); \
    std::unique_ptr<TYPE> uptr(ptr);                       \
    On##TYPE(uptr);                                        \
    static_cast<void>(uptr.release());                     \
  } while (0)

  virtual void OnConstant(const std::unique_ptr<Constant>& element) {
    Constant::Kind kind = element->kind;
    switch (kind) {
      case Constant::Kind::kIdentifier: {
        DISPATCH_TO(IdentifierConstant, Constant, element);
        break;
      }
      case Constant::Kind::kLiteral: {
        DISPATCH_TO(LiteralConstant, Constant, element);
        break;
      }
      case Constant::Kind::kBinaryOperator: {
        DISPATCH_TO(BinaryOperatorConstant, Constant, element);
        break;
      }
    }
  }

  virtual void OnIdentifierConstant(const std::unique_ptr<IdentifierConstant>& element) {
    element->Accept(this);
  }
  virtual void OnLiteralConstant(const std::unique_ptr<LiteralConstant>& element) {
    element->Accept(this);
  }
  virtual void OnBinaryOperatorConstant(const std::unique_ptr<BinaryOperatorConstant>& element) {
    element->Accept(this);
  }

  virtual void OnAttributeArg(const std::unique_ptr<AttributeArg>& element) {
    element->Accept(this);
  }

  virtual void OnAttribute(const std::unique_ptr<Attribute>& element) { element->Accept(this); }

  virtual void OnAttributeList(const std::unique_ptr<AttributeList>& element) {
    element->Accept(this);
  }

  virtual void OnAliasDeclaration(const std::unique_ptr<AliasDeclaration>& element) {
    element->Accept(this);
  }

  virtual void OnLibraryDeclaration(const std::unique_ptr<LibraryDeclaration>& element) {
    element->Accept(this);
  }

  virtual void OnUsing(const std::unique_ptr<Using>& element) { element->Accept(this); }

  virtual void OnConstDeclaration(const std::unique_ptr<ConstDeclaration>& element) {
    element->Accept(this);
  }

  virtual void OnParameterList(const std::unique_ptr<ParameterList>& element) {
    element->Accept(this);
  }
  virtual void OnProtocolMethod(const std::unique_ptr<ProtocolMethod>& element) {
    element->Accept(this);
  }
  virtual void OnProtocolCompose(const std::unique_ptr<ProtocolCompose>& element) {
    element->Accept(this);
  }
  virtual void OnProtocolDeclaration(const std::unique_ptr<ProtocolDeclaration>& element) {
    element->Accept(this);
  }
  virtual void OnResourceProperty(const std::unique_ptr<ResourceProperty>& element) {
    element->Accept(this);
  }
  virtual void OnResourceDeclaration(const std::unique_ptr<ResourceDeclaration>& element) {
    element->Accept(this);
  }
  virtual void OnServiceMember(const std::unique_ptr<ServiceMember>& element) {
    element->Accept(this);
  }
  virtual void OnServiceDeclaration(const std::unique_ptr<ServiceDeclaration>& element) {
    element->Accept(this);
  }
  virtual void OnModifiers(const std::unique_ptr<Modifiers>& element) { element->Accept(this); }

  virtual void OnLayoutParameter(const std::unique_ptr<LayoutParameter>& element) {
    LayoutParameter::Kind kind = element->kind;
    switch (kind) {
      case LayoutParameter::Kind::kIdentifier: {
        DISPATCH_TO(IdentifierLayoutParameter, LayoutParameter, element);
        break;
      }
      case LayoutParameter::Kind::kLiteral: {
        DISPATCH_TO(LiteralLayoutParameter, LayoutParameter, element);
        break;
      }
      case LayoutParameter::Kind::kType: {
        DISPATCH_TO(TypeLayoutParameter, LayoutParameter, element);
        break;
      }
    }
  }

  virtual void OnLayoutParameterList(const std::unique_ptr<LayoutParameterList>& element) {
    element->Accept(this);
  }

  virtual void OnIdentifierLayoutParameter(
      const std::unique_ptr<IdentifierLayoutParameter>& element) {
    element->Accept(this);
  }
  virtual void OnLiteralLayoutParameter(const std::unique_ptr<LiteralLayoutParameter>& element) {
    element->Accept(this);
  }
  virtual void OnTypeLayoutParameter(const std::unique_ptr<TypeLayoutParameter>& element) {
    element->Accept(this);
  }

  virtual void OnLayoutMember(const std::unique_ptr<LayoutMember>& element) {
    LayoutMember::Kind kind = element->kind;
    switch (kind) {
      case LayoutMember::Kind::kOrdinaled: {
        DISPATCH_TO(OrdinaledLayoutMember, LayoutMember, element);
        break;
      }
      case LayoutMember::Kind::kStruct: {
        DISPATCH_TO(StructLayoutMember, LayoutMember, element);
        break;
      }
      case LayoutMember::Kind::kValue: {
        DISPATCH_TO(ValueLayoutMember, LayoutMember, element);
        break;
      }
    }
  }

  virtual void OnOrdinaledLayoutMember(const std::unique_ptr<OrdinaledLayoutMember>& element) {
    element->Accept(this);
  }
  virtual void OnStructLayoutMember(const std::unique_ptr<StructLayoutMember>& element) {
    element->Accept(this);
  }
  virtual void OnValueLayoutMember(const std::unique_ptr<ValueLayoutMember>& element) {
    element->Accept(this);
  }

  virtual void OnLayout(const std::unique_ptr<Layout>& element) { element->Accept(this); }

  virtual void OnLayoutReference(const std::unique_ptr<LayoutReference>& element) {
    LayoutReference::Kind kind = element->kind;
    switch (kind) {
      case LayoutReference::Kind::kInline: {
        DISPATCH_TO(InlineLayoutReference, LayoutReference, element);
        break;
      }
      case LayoutReference::Kind::kNamed: {
        DISPATCH_TO(NamedLayoutReference, LayoutReference, element);
        break;
      }
    }
  }

  virtual void OnInlineLayoutReference(const std::unique_ptr<InlineLayoutReference>& element) {
    element->Accept(this);
  }
  virtual void OnNamedLayoutReference(const std::unique_ptr<NamedLayoutReference>& element) {
    element->Accept(this);
  }

  virtual void OnTypeConstraints(const std::unique_ptr<TypeConstraints>& element) {
    element->Accept(this);
  }

  virtual void OnTypeConstructor(const std::unique_ptr<TypeConstructor>& element) {
    element->Accept(this);
  }

  virtual void OnTypeDeclaration(const std::unique_ptr<TypeDeclaration>& element) {
    element->Accept(this);
  }

  virtual void OnFile(const std::unique_ptr<File>& element) { element->Accept(this); }
};

#undef DISPATCH_TO

// AST node contents are not stored in declaration order in the tree, so we
// have a special visitor for code that needs to visit in declaration order.
class DeclarationOrderTreeVisitor : public TreeVisitor {
 public:
  void OnFile(const std::unique_ptr<File>& element) override;
  void OnProtocolDeclaration(const std::unique_ptr<ProtocolDeclaration>& element) override;
};

}  // namespace fidl::raw

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_TREE_VISITOR_H_
