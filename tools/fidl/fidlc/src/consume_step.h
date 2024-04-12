// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_SRC_CONSUME_STEP_H_
#define TOOLS_FIDL_FIDLC_SRC_CONSUME_STEP_H_

#include "tools/fidl/fidlc/src/compiler.h"

namespace fidlc {

// We run a separate ConsumeStep for each file in the library.
class ConsumeStep : public Compiler::Step {
 public:
  ConsumeStep(Compiler* compiler, std::unique_ptr<File> file);

 private:
  void RunImpl() override;

  // Returns a pointer to the registered decl, or null on failure.
  Decl* RegisterDecl(std::unique_ptr<Decl> decl);

  // Top level declarations
  void ConsumeAliasDeclaration(std::unique_ptr<RawAliasDeclaration> alias_declaration);
  void ConsumeConstDeclaration(std::unique_ptr<RawConstDeclaration> const_declaration);
  void ConsumeProtocolDeclaration(std::unique_ptr<RawProtocolDeclaration> protocol_declaration);
  void ConsumeResourceDeclaration(std::unique_ptr<RawResourceDeclaration> resource_declaration);
  void ConsumeServiceDeclaration(std::unique_ptr<RawServiceDeclaration> service_decl);
  void ConsumeTypeDeclaration(std::unique_ptr<RawTypeDeclaration> type_decl);
  void ConsumeNewType(std::unique_ptr<RawTypeDeclaration> type_decl);
  void ConsumeUsing(std::unique_ptr<RawUsing> using_directive);

  // Layouts
  template <typename T>  // T should be Table, Union or Overlay
  bool ConsumeOrdinaledLayout(std::unique_ptr<RawLayout> layout,
                              const std::shared_ptr<NamingContext>& context,
                              std::unique_ptr<RawAttributeList> raw_attribute_list,
                              Decl** out_decl);
  bool ConsumeStructLayout(std::unique_ptr<RawLayout> layout,
                           const std::shared_ptr<NamingContext>& context,
                           std::unique_ptr<RawAttributeList> raw_attribute_list, Decl** out_decl);
  template <typename T>  // T should be Bits or Enum
  bool ConsumeValueLayout(std::unique_ptr<RawLayout> layout,
                          const std::shared_ptr<NamingContext>& context,
                          std::unique_ptr<RawAttributeList> raw_attribute_list, Decl** out_decl);
  bool ConsumeLayout(std::unique_ptr<RawLayout> layout,
                     const std::shared_ptr<NamingContext>& context,
                     std::unique_ptr<RawAttributeList> raw_attribute_list, Decl** out_decl);

  // Other elements
  void ConsumeAttribute(std::unique_ptr<RawAttribute> raw_attribute,
                        std::unique_ptr<Attribute>* out_attribute);
  void ConsumeAttributeList(std::unique_ptr<RawAttributeList> raw_attribute_list,
                            std::unique_ptr<AttributeList>* out_attribute_list);
  bool ConsumeConstant(std::unique_ptr<RawConstant> raw_constant,
                       std::unique_ptr<Constant>* out_constant);
  void ConsumeLiteralConstant(RawLiteralConstant* raw_constant,
                              std::unique_ptr<LiteralConstant>* out_constant);
  bool ConsumeParameterList(SourceSpan method_name, const std::shared_ptr<NamingContext>& context,
                            std::unique_ptr<RawParameterList> parameter_layout,
                            bool is_request_or_response,
                            std::unique_ptr<TypeConstructor>* out_payload);
  bool ConsumeTypeConstructor(std::unique_ptr<RawTypeConstructor> raw_type_ctor,
                              const std::shared_ptr<NamingContext>& context,
                              std::unique_ptr<RawAttributeList> raw_attribute_list,

                              std::unique_ptr<TypeConstructor>* out_type, Decl** out_inline_decl);
  bool ConsumeTypeConstructor(std::unique_ptr<RawTypeConstructor> raw_type_ctor,
                              const std::shared_ptr<NamingContext>& context,
                              std::unique_ptr<TypeConstructor>* out_type);

  // Elements stored in the library
  const RawLiteral* ConsumeLiteral(std::unique_ptr<RawLiteral> raw_literal);
  const RawOrdinal64* ConsumeOrdinal(std::unique_ptr<RawOrdinal64> raw_ordinal);

  // Sets the naming context's generated name override to the @generated_name
  // attribute's value if present, otherwise does nothing.
  void MaybeOverrideName(AttributeList& attributes, NamingContext* context);
  // Generates the synthetic result type used for encoding the method's response, if the method has
  // an error type or is marked as flexible (or both). Adds the generated type to the library and
  // provides a `TypeConstructor` that refers to it.
  //
  // The generated type includes both the outer wrapping struct and the result union.
  bool CreateMethodResult(const std::shared_ptr<NamingContext>& success_variant_context,
                          const std::shared_ptr<NamingContext>& err_variant_context,
                          const std::shared_ptr<NamingContext>& framework_err_variant_context,
                          bool has_err, bool has_framework_err, SourceSpan response_span,
                          RawProtocolMethod* method,
                          std::unique_ptr<TypeConstructor> success_variant,
                          std::unique_ptr<TypeConstructor>* out_payload);

  std::unique_ptr<File> file_;

  // Decl for default underlying type to use for bits and enums.
  Decl* default_underlying_type_;

  // Decl for the type to use for framework_err.
  Decl* framework_err_type_;
};

}  // namespace fidlc

#endif  // TOOLS_FIDL_FIDLC_SRC_CONSUME_STEP_H_
