// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/src/consume_step.h"

#include <zircon/assert.h>

#include "tools/fidl/fidlc/src/attributes.h"
#include "tools/fidl/fidlc/src/compile_step.h"
#include "tools/fidl/fidlc/src/diagnostics.h"
#include "tools/fidl/fidlc/src/experimental_flags.h"
#include "tools/fidl/fidlc/src/flat_ast.h"
#include "tools/fidl/fidlc/src/raw_ast.h"

namespace fidlc {

ConsumeStep::ConsumeStep(Compiler* compiler, std::unique_ptr<File> file)
    : Step(compiler),
      file_(std::move(file)),
      default_underlying_type_(
          all_libraries()->root_library()->declarations.LookupBuiltin(Builtin::Identity::kUint32)),
      framework_err_type_(all_libraries()->root_library()->declarations.LookupBuiltin(
          Builtin::Identity::kFrameworkErr)) {}

void ConsumeStep::RunImpl() {
  // All fidl files in a library should agree on the library name.
  if (auto name = file_->library_decl->path->ToString(); library()->name.empty()) {
    library()->name = std::move(name);
  } else if (name != library()->name) {
    reporter()->Fail(ErrFilesDisagreeOnLibraryName, file_->library_decl->path->span());
    return;
  }
  library()->name_spans.emplace_back(file_->library_decl->path->span());

  ConsumeAttributeList(std::move(file_->library_decl->attributes), &library()->attributes);

  for (auto& using_directive : std::move(file_->using_list)) {
    ConsumeUsing(std::move(using_directive));
  }
  for (auto& alias_declaration : std::move(file_->alias_list)) {
    ConsumeAliasDeclaration(std::move(alias_declaration));
  }
  for (auto& const_declaration : std::move(file_->const_declaration_list)) {
    ConsumeConstDeclaration(std::move(const_declaration));
  }
  for (auto& protocol_declaration : std::move(file_->protocol_declaration_list)) {
    ConsumeProtocolDeclaration(std::move(protocol_declaration));
  }
  for (auto& resource_declaration : std::move(file_->resource_declaration_list)) {
    ConsumeResourceDeclaration(std::move(resource_declaration));
  }
  for (auto& service_declaration : std::move(file_->service_declaration_list)) {
    ConsumeServiceDeclaration(std::move(service_declaration));
  }
  for (auto& type_decl : std::move(file_->type_decls)) {
    ConsumeTypeDeclaration(std::move(type_decl));
  }
}

Decl* ConsumeStep::RegisterDecl(std::unique_ptr<Decl> decl) {
  auto decl_ptr = library()->declarations.Insert(std::move(decl));
  const Name& name = decl_ptr->name;
  if (name.span()) {
    if (library()->dependencies.Contains(name.span()->source_file().filename(),
                                         {name.span()->data()})) {
      reporter()->Fail(ErrDeclNameConflictsWithLibraryImport, name.span().value(), name);
    } else if (auto canonical_decl_name = Canonicalize(name.decl_name());
               library()->dependencies.Contains(name.span()->source_file().filename(),
                                                {canonical_decl_name})) {
      reporter()->Fail(ErrDeclNameConflictsWithLibraryImportCanonical, name.span().value(), name,
                       canonical_decl_name);
    }
  }
  return decl_ptr;
}

void ConsumeStep::ConsumeAttributeList(std::unique_ptr<RawAttributeList> raw_attribute_list,
                                       std::unique_ptr<AttributeList>* out_attribute_list) {
  ZX_ASSERT_MSG(out_attribute_list, "must provide out parameter");
  // Usually *out_attribute_list is null and we create the AttributeList here.
  // For library declarations it's not, since we consume attributes from each
  // file into the same library->attributes field.
  if (*out_attribute_list == nullptr) {
    *out_attribute_list = std::make_unique<AttributeList>();
  }
  if (!raw_attribute_list) {
    return;
  }
  auto& out_attributes = (*out_attribute_list)->attributes;
  for (auto& raw_attribute : raw_attribute_list->attributes) {
    std::unique_ptr<Attribute> attribute;
    ConsumeAttribute(std::move(raw_attribute), &attribute);
    out_attributes.push_back(std::move(attribute));
  }
}

void ConsumeStep::ConsumeAttribute(std::unique_ptr<RawAttribute> raw_attribute,
                                   std::unique_ptr<Attribute>* out_attribute) {
  bool all_named = true;
  std::vector<std::unique_ptr<AttributeArg>> args;
  for (auto& raw_arg : raw_attribute->args) {
    std::unique_ptr<Constant> constant;
    if (!ConsumeConstant(std::move(raw_arg->value), &constant)) {
      continue;
    }
    std::optional<SourceSpan> name;
    if (raw_arg->maybe_name) {
      name = raw_arg->maybe_name->span();
    }
    all_named = all_named && name.has_value();
    args.emplace_back(std::make_unique<AttributeArg>(name, std::move(constant), raw_arg->span()));
  }
  ZX_ASSERT_MSG(all_named || args.size() == 1,
                "parser should not allow an anonymous arg with other args");
  SourceSpan name;
  switch (raw_attribute->provenance) {
    case RawAttribute::Provenance::kDefault:
      name = raw_attribute->maybe_name->span();
      break;
    case RawAttribute::Provenance::kDocComment:
      name = generated_source_file()->AddLine(Attribute::kDocCommentName);
      break;
  }
  *out_attribute = std::make_unique<Attribute>(name, std::move(args), raw_attribute->span());
  all_libraries()->WarnOnAttributeTypo(out_attribute->get());
}

bool ConsumeStep::ConsumeConstant(std::unique_ptr<RawConstant> raw_constant,
                                  std::unique_ptr<Constant>* out_constant) {
  switch (raw_constant->kind) {
    case RawConstant::Kind::kIdentifier: {
      auto identifier = static_cast<RawIdentifierConstant*>(raw_constant.get());
      *out_constant =
          std::make_unique<IdentifierConstant>(*identifier->identifier, identifier->span());
      break;
    }
    case RawConstant::Kind::kLiteral: {
      auto literal = static_cast<RawLiteralConstant*>(raw_constant.get());
      std::unique_ptr<LiteralConstant> out;
      ConsumeLiteralConstant(literal, &out);
      *out_constant = std::unique_ptr<Constant>(out.release());
      break;
    }
    case RawConstant::Kind::kBinaryOperator: {
      auto binary_operator_constant = static_cast<RawBinaryOperatorConstant*>(raw_constant.get());
      BinaryOperatorConstant::Operator op;
      switch (binary_operator_constant->op) {
        case RawBinaryOperatorConstant::Operator::kOr:
          op = BinaryOperatorConstant::Operator::kOr;
          break;
      }
      std::unique_ptr<Constant> left_operand;
      if (!ConsumeConstant(std::move(binary_operator_constant->left_operand), &left_operand)) {
        return false;
      }
      std::unique_ptr<Constant> right_operand;
      if (!ConsumeConstant(std::move(binary_operator_constant->right_operand), &right_operand)) {
        return false;
      }
      *out_constant = std::make_unique<BinaryOperatorConstant>(
          std::move(left_operand), std::move(right_operand), op, binary_operator_constant->span());
      break;
    }
  }
  return true;
}

void ConsumeStep::ConsumeLiteralConstant(RawLiteralConstant* raw_constant,
                                         std::unique_ptr<LiteralConstant>* out_constant) {
  *out_constant =
      std::make_unique<LiteralConstant>(ConsumeLiteral(std::move(raw_constant->literal)));
}

void ConsumeStep::ConsumeUsing(std::unique_ptr<RawUsing> using_directive) {
  if (using_directive->attributes != nullptr) {
    reporter()->Fail(ErrAttributesNotAllowedOnLibraryImport, using_directive->span());
    return;
  }

  auto library_name = using_directive->using_path->ToString();
  Library* dep_library = all_libraries()->Lookup(library_name);
  if (!dep_library) {
    reporter()->Fail(ErrUnknownLibrary, using_directive->using_path->span(), library_name);
    return;
  }

  const auto filename = using_directive->span().source_file().filename();
  const auto result = library()->dependencies.Register(
      using_directive->using_path->span(), filename, dep_library, using_directive->maybe_alias);
  switch (result) {
    case Dependencies::RegisterResult::kSuccess:
      break;
    case Dependencies::RegisterResult::kDuplicate:
      reporter()->Fail(ErrDuplicateLibraryImport, using_directive->span(), library_name);
      return;
    case Dependencies::RegisterResult::kCollision:
      if (using_directive->maybe_alias) {
        reporter()->Fail(ErrConflictingLibraryImportAlias, using_directive->span(), library_name,
                         using_directive->maybe_alias->span().data());
        return;
      }
      reporter()->Fail(ErrConflictingLibraryImport, using_directive->span(), library_name);
      return;
  }
}

void ConsumeStep::ConsumeAliasDeclaration(std::unique_ptr<RawAliasDeclaration> alias_declaration) {
  ZX_ASSERT(alias_declaration->alias && alias_declaration->type_ctor != nullptr);

  std::unique_ptr<AttributeList> attributes;
  ConsumeAttributeList(std::move(alias_declaration->attributes), &attributes);

  auto alias_name = Name::CreateSourced(library(), alias_declaration->alias->span());
  std::unique_ptr<TypeConstructor> type_ctor_;

  if (!ConsumeTypeConstructor(std::move(alias_declaration->type_ctor),
                              NamingContext::Create(alias_name), &type_ctor_))
    return;

  RegisterDecl(
      std::make_unique<Alias>(std::move(attributes), std::move(alias_name), std::move(type_ctor_)));
}

void ConsumeStep::ConsumeConstDeclaration(std::unique_ptr<RawConstDeclaration> const_declaration) {
  auto span = const_declaration->identifier->span();
  auto name = Name::CreateSourced(library(), span);
  std::unique_ptr<AttributeList> attributes;
  ConsumeAttributeList(std::move(const_declaration->attributes), &attributes);

  std::unique_ptr<TypeConstructor> type_ctor;
  if (!ConsumeTypeConstructor(std::move(const_declaration->type_ctor), NamingContext::Create(name),
                              &type_ctor))
    return;

  std::unique_ptr<Constant> constant;
  if (!ConsumeConstant(std::move(const_declaration->constant), &constant))
    return;

  RegisterDecl(std::make_unique<Const>(std::move(attributes), std::move(name), std::move(type_ctor),
                                       std::move(constant)));
}

// Create a type constructor pointing to an anonymous layout.
static std::unique_ptr<TypeConstructor> IdentifierTypeForDecl(Decl* decl) {
  return std::make_unique<TypeConstructor>(SourceSpan(), Reference(Reference::Target(decl)),
                                           std::make_unique<LayoutParameterList>(),
                                           std::make_unique<TypeConstraints>());
}

bool ConsumeStep::CreateMethodResult(
    const std::shared_ptr<NamingContext>& success_variant_context,
    const std::shared_ptr<NamingContext>& err_variant_context,
    const std::shared_ptr<NamingContext>& framework_err_variant_context, bool has_err,
    bool has_framework_err, SourceSpan response_span, RawProtocolMethod* method,
    std::unique_ptr<TypeConstructor> success_variant,
    std::unique_ptr<TypeConstructor>* out_payload) {
  ZX_ASSERT_MSG(
      has_err || has_framework_err,
      "method should only use a result union if it has a result union and/or is flexible");
  ZX_ASSERT(err_variant_context != nullptr);
  ZX_ASSERT(framework_err_variant_context != nullptr);

  auto ordinal_source = SourceElement(Token(), Token());
  std::vector<Union::Member> result_members;

  using Ordinal = Protocol::Method::ResultUnionOrdinal;

  result_members.emplace_back(
      ConsumeOrdinal(std::make_unique<RawOrdinal64>(ordinal_source, Ordinal::kSuccess)),
      std::move(success_variant), success_variant_context->name(),
      std::make_unique<AttributeList>());

  if (has_err) {
    std::unique_ptr<TypeConstructor> error_type_ctor;
    // Compile the error type.
    if (!ConsumeTypeConstructor(std::move(method->maybe_error_ctor), err_variant_context,
                                &error_type_ctor))
      return false;

    ZX_ASSERT_MSG(error_type_ctor != nullptr, "missing err type ctor");

    result_members.emplace_back(
        ConsumeOrdinal(std::make_unique<RawOrdinal64>(ordinal_source, Ordinal::kDomainError)),
        std::move(error_type_ctor), err_variant_context->name(), std::make_unique<AttributeList>());
  }

  if (has_framework_err) {
    std::unique_ptr<TypeConstructor> error_type_ctor = IdentifierTypeForDecl(framework_err_type_);
    ZX_ASSERT_MSG(error_type_ctor != nullptr, "missing framework_err type ctor");
    result_members.emplace_back(
        ConsumeOrdinal(std::make_unique<RawOrdinal64>(ordinal_source, Ordinal::kFrameworkError)),
        std::move(error_type_ctor), framework_err_variant_context->name(),
        std::make_unique<AttributeList>());
  }
  // framework_err is not defined if the method is not flexible.

  auto result_context = err_variant_context->parent();
  auto result_name = Name::CreateAnonymous(library(), response_span, result_context,
                                           Name::Provenance::kGeneratedResultUnion);
  auto union_decl = std::make_unique<Union>(std::make_unique<AttributeList>(),
                                            std::move(result_name), std::move(result_members),
                                            Strictness::kStrict, std::nullopt /* resourceness */);
  auto result_decl = union_decl.get();
  if (!RegisterDecl(std::move(union_decl)))
    return false;

  *out_payload = IdentifierTypeForDecl(result_decl);
  return true;
}

void ConsumeStep::ConsumeProtocolDeclaration(
    std::unique_ptr<RawProtocolDeclaration> protocol_declaration) {
  auto protocol_name = Name::CreateSourced(library(), protocol_declaration->identifier->span());
  auto protocol_context = NamingContext::Create(protocol_name.span().value());

  std::vector<Protocol::ComposedProtocol> composed_protocols;
  for (auto& raw_composed : protocol_declaration->composed_protocols) {
    std::unique_ptr<AttributeList> attributes;
    ConsumeAttributeList(std::move(raw_composed->attributes), &attributes);
    composed_protocols.emplace_back(std::move(attributes), Reference(*raw_composed->protocol_name));
  }

  std::vector<Protocol::Method> methods;
  for (auto& method : protocol_declaration->methods) {
    std::unique_ptr<AttributeList> attributes;
    ConsumeAttributeList(std::move(method->attributes), &attributes);

    SourceSpan method_name = method->identifier->span();

    auto strictness = Strictness::kFlexible;
    if (method->modifiers != nullptr && method->modifiers->maybe_strictness.has_value()) {
      strictness = method->modifiers->maybe_strictness->value;
    }

    bool has_request = method->maybe_request != nullptr;
    std::unique_ptr<TypeConstructor> maybe_request;
    if (has_request) {
      bool result = ConsumeParameterList(method_name, protocol_context->EnterRequest(method_name),
                                         std::move(method->maybe_request),
                                         /*is_request_or_response=*/true, &maybe_request);
      if (!result)
        return;
    }

    std::unique_ptr<TypeConstructor> maybe_response;
    bool has_response = method->maybe_response != nullptr;
    bool has_error = false;
    if (has_response) {
      has_error = method->maybe_error_ctor != nullptr;
      // has_framework_error is true for flexible two-way methods. We already
      // checked has_response in the outer if block, so to see whether this is a
      // two-way method or an event, we check has_request here.
      bool has_framework_error = has_request && strictness == Strictness::kFlexible;

      const auto response_context = has_request ? protocol_context->EnterResponse(method_name)
                                                : protocol_context->EnterEvent(method_name);

      if (has_error || has_framework_error) {
        SourceSpan response_span = method->maybe_response->span();
        std::shared_ptr<NamingContext> success_variant_context, err_variant_context,
            framework_err_variant_context;

        // In protocol P, if method M is flexible or uses the error syntax, its
        // response is the following compiler-generated union:
        //
        //     type P_M_Result = union {
        //       // The "success variant".
        //       1: response @generated_name("P_M_Response") [user specified response type];
        //       // Only present for methods using error syntax.
        //       2: err @generated_name("P_M_Error") [user specified error type];
        //       // Only present for flexible methods.
        //       3: framework_err fidl.FrameworkErr;
        //     };
        //
        // This naming scheme is inconsistent with other compiler-generated
        // names (e.g. PMRequest) because the error syntax predates anonymous
        // layouts. We keep it like this for backwards compatibility.
        //
        // Note that although the success variant is named P_M_Response, in the
        // fidlc codebase we use the word "response" to refer to the outermost
        // type, which in this case is P_M_Result.
        auto prefix =
            std::string(protocol_name.decl_name()) + "_" + std::string(method_name.data());
        response_context->set_name_override(prefix + "_Result");
        success_variant_context =
            response_context->EnterMember(generated_source_file()->AddLine("response"));
        success_variant_context->set_name_override(prefix + "_Response");
        err_variant_context =
            response_context->EnterMember(generated_source_file()->AddLine("err"));
        err_variant_context->set_name_override(prefix + "_Error");
        framework_err_variant_context =
            response_context->EnterMember(generated_source_file()->AddLine("framework_err"));

        std::unique_ptr<TypeConstructor> result_payload;

        if (!ConsumeParameterList(method_name, success_variant_context,
                                  std::move(method->maybe_response),
                                  /*is_request_or_response=*/false, &result_payload)) {
          return;
        }

        ZX_ASSERT_MSG(err_variant_context != nullptr && framework_err_variant_context != nullptr,
                      "error type contexts should have been computed");
        if (!CreateMethodResult(success_variant_context, err_variant_context,
                                framework_err_variant_context, has_error, has_framework_error,
                                response_span, method.get(), std::move(result_payload),
                                &maybe_response))
          return;
      } else {
        std::unique_ptr<TypeConstructor> response_payload;
        if (!ConsumeParameterList(method_name, response_context, std::move(method->maybe_response),
                                  /*is_request_or_response=*/true, &response_payload)) {
          return;
        }

        maybe_response = std::move(response_payload);
      }
    }
    ZX_ASSERT(has_request || has_response);
    methods.emplace_back(std::move(attributes), strictness, method_name, has_request,
                         std::move(maybe_request), has_response, std::move(maybe_response),
                         has_error);
  }

  std::unique_ptr<AttributeList> attributes;
  ConsumeAttributeList(std::move(protocol_declaration->attributes), &attributes);

  auto openness = Openness::kOpen;
  if (protocol_declaration->modifiers != nullptr &&
      protocol_declaration->modifiers->maybe_openness.has_value()) {
    openness = protocol_declaration->modifiers->maybe_openness->value;
  }

  RegisterDecl(std::make_unique<Protocol>(std::move(attributes), openness, std::move(protocol_name),
                                          std::move(composed_protocols), std::move(methods)));
}

bool ConsumeStep::ConsumeParameterList(const SourceSpan method_name,
                                       const std::shared_ptr<NamingContext>& context,
                                       std::unique_ptr<RawParameterList> parameter_layout,
                                       bool is_request_or_response,
                                       std::unique_ptr<TypeConstructor>* out_payload) {
  if (!parameter_layout->type_ctor) {
    // Empty request or response, like `Foo()` or `Foo(...) -> ()`:
    if (is_request_or_response) {
      // Nothing to do.
      return true;
    }
    // We have an empty success variant, like `Foo(...) -> () error uint32`.
    // Synthesize an empty struct for the result union.
    auto empty_struct = std::make_unique<Struct>(
        std::make_unique<AttributeList>(),
        Name::CreateAnonymous(library(), parameter_layout->span(), context,
                              Name::Provenance::kGeneratedEmptySuccessStruct),
        std::vector<Struct::Member>(), Resourceness::kValue);
    auto empty_struct_decl = empty_struct.get();
    ZX_ASSERT(RegisterDecl(std::move(empty_struct)));
    *out_payload = IdentifierTypeForDecl(empty_struct_decl);
    return true;
  }

  std::unique_ptr<TypeConstructor> type_ctor;
  Decl* inline_decl = nullptr;
  if (!ConsumeTypeConstructor(std::move(parameter_layout->type_ctor), context,
                              /*raw_attribute_list=*/nullptr, &type_ctor, &inline_decl))
    return false;

  *out_payload = std::move(type_ctor);
  return true;
}

void ConsumeStep::ConsumeResourceDeclaration(
    std::unique_ptr<RawResourceDeclaration> resource_declaration) {
  auto name = Name::CreateSourced(library(), resource_declaration->identifier->span());
  auto context = NamingContext::Create(name);
  std::vector<Resource::Property> properties;
  for (auto& property : resource_declaration->properties) {
    std::unique_ptr<AttributeList> attributes;
    ConsumeAttributeList(std::move(property->attributes), &attributes);

    std::unique_ptr<TypeConstructor> type_ctor;
    if (!ConsumeTypeConstructor(std::move(property->type_ctor),
                                context->EnterMember(property->identifier->span()), &type_ctor))
      return;
    properties.emplace_back(std::move(type_ctor), property->identifier->span(),
                            std::move(attributes));
  }

  std::unique_ptr<AttributeList> attributes;
  ConsumeAttributeList(std::move(resource_declaration->attributes), &attributes);

  std::unique_ptr<TypeConstructor> type_ctor;
  if (resource_declaration->maybe_type_ctor != nullptr) {
    if (!ConsumeTypeConstructor(std::move(resource_declaration->maybe_type_ctor), context,
                                &type_ctor))
      return;
  } else {
    type_ctor = IdentifierTypeForDecl(default_underlying_type_);
  }

  RegisterDecl(std::make_unique<Resource>(std::move(attributes), std::move(name),
                                          std::move(type_ctor), std::move(properties)));
}

void ConsumeStep::ConsumeServiceDeclaration(std::unique_ptr<RawServiceDeclaration> service_decl) {
  auto name = Name::CreateSourced(library(), service_decl->identifier->span());
  auto context = NamingContext::Create(name);
  std::vector<Service::Member> members;
  for (auto& member : service_decl->members) {
    std::unique_ptr<AttributeList> attributes;
    ConsumeAttributeList(std::move(member->attributes), &attributes);

    std::unique_ptr<TypeConstructor> type_ctor;
    if (!ConsumeTypeConstructor(std::move(member->type_ctor), context->EnterMember(member->span()),
                                &type_ctor))
      return;
    members.emplace_back(std::move(type_ctor), member->identifier->span(), std::move(attributes));
  }

  std::unique_ptr<AttributeList> attributes;
  ConsumeAttributeList(std::move(service_decl->attributes), &attributes);

  RegisterDecl(
      std::make_unique<Service>(std::move(attributes), std::move(name), std::move(members)));
}

void ConsumeStep::MaybeOverrideName(AttributeList& attributes, NamingContext* context) {
  auto attr = attributes.Get("generated_name");
  if (attr == nullptr)
    return;

  CompileStep::CompileAttributeEarly(compiler(), attr);
  const auto* arg = attr->GetArg(AttributeArg::kDefaultAnonymousName);
  if (arg == nullptr || !arg->value->IsResolved()) {
    return;
  }
  auto str = arg->value->Value().AsString();
  if (IsValidIdentifierComponent(str)) {
    context->set_name_override(std::string(str));
  } else {
    reporter()->Fail(ErrInvalidGeneratedName, arg->span);
  }
}

template <typename T>
bool ConsumeStep::ConsumeValueLayout(std::unique_ptr<RawLayout> layout,
                                     const std::shared_ptr<NamingContext>& context,
                                     std::unique_ptr<RawAttributeList> raw_attribute_list,
                                     Decl** out_decl) {
  std::vector<typename T::Member> members;
  for (auto& mem : layout->members) {
    auto member = static_cast<RawValueLayoutMember*>(mem.get());
    auto span = member->identifier->span();

    std::unique_ptr<AttributeList> attributes;
    ConsumeAttributeList(std::move(member->attributes), &attributes);

    std::unique_ptr<Constant> value;
    if (!ConsumeConstant(std::move(member->value), &value))
      return false;

    members.emplace_back(span, std::move(value), std::move(attributes));
  }

  std::unique_ptr<TypeConstructor> subtype_ctor;
  if (layout->subtype_ctor != nullptr) {
    if (!ConsumeTypeConstructor(std::move(layout->subtype_ctor), context, &subtype_ctor))
      return false;
  } else {
    subtype_ctor = IdentifierTypeForDecl(default_underlying_type_);
  }

  std::unique_ptr<AttributeList> attributes;
  ConsumeAttributeList(std::move(raw_attribute_list), &attributes);
  MaybeOverrideName(*attributes, context.get());

  auto strictness = Strictness::kFlexible;
  if (layout->modifiers != nullptr && layout->modifiers->maybe_strictness.has_value())
    strictness = layout->modifiers->maybe_strictness->value;

  if (layout->members.empty()) {
    if (strictness != Strictness::kFlexible)
      return reporter()->Fail(ErrMustHaveOneMember, layout->span());
  }

  Decl* decl = RegisterDecl(
      std::make_unique<T>(std::move(attributes), context->ToName(library(), layout->span()),
                          std::move(subtype_ctor), std::move(members), strictness));
  if (out_decl) {
    *out_decl = decl;
  }
  return decl != nullptr;
}

template <typename T>
bool ConsumeStep::ConsumeOrdinaledLayout(std::unique_ptr<RawLayout> layout,
                                         const std::shared_ptr<NamingContext>& context,
                                         std::unique_ptr<RawAttributeList> raw_attribute_list,
                                         Decl** out_decl) {
  std::vector<typename T::Member> members;
  for (auto& mem : layout->members) {
    auto member = static_cast<RawOrdinaledLayoutMember*>(mem.get());
    std::unique_ptr<AttributeList> attributes;
    ConsumeAttributeList(std::move(member->attributes), &attributes);

    std::unique_ptr<TypeConstructor> type_ctor;
    if (!ConsumeTypeConstructor(std::move(member->type_ctor),
                                context->EnterMember(member->identifier->span()), &type_ctor))
      return false;

    members.emplace_back(ConsumeOrdinal(std::move(member->ordinal)), std::move(type_ctor),
                         member->identifier->span(), std::move(attributes));
  }

  std::unique_ptr<AttributeList> attributes;
  ConsumeAttributeList(std::move(raw_attribute_list), &attributes);
  MaybeOverrideName(*attributes, context.get());

  auto strictness = Strictness::kFlexible;
  if (layout->modifiers != nullptr && layout->modifiers->maybe_strictness.has_value())
    strictness = layout->modifiers->maybe_strictness->value;

  auto resourceness = Resourceness::kValue;
  if (layout->modifiers != nullptr && layout->modifiers->maybe_resourceness.has_value())
    resourceness = layout->modifiers->maybe_resourceness->value;

  Decl* decl = RegisterDecl(std::make_unique<T>(std::move(attributes),
                                                context->ToName(library(), layout->span()),
                                                std::move(members), strictness, resourceness));
  if (out_decl) {
    *out_decl = decl;
  }
  return decl != nullptr;
}

bool ConsumeStep::ConsumeStructLayout(std::unique_ptr<RawLayout> layout,
                                      const std::shared_ptr<NamingContext>& context,
                                      std::unique_ptr<RawAttributeList> raw_attribute_list,
                                      Decl** out_decl) {
  std::vector<Struct::Member> members;
  for (auto& mem : layout->members) {
    auto member = static_cast<RawStructLayoutMember*>(mem.get());

    std::unique_ptr<AttributeList> attributes;
    ConsumeAttributeList(std::move(member->attributes), &attributes);

    std::unique_ptr<TypeConstructor> type_ctor;
    if (!ConsumeTypeConstructor(std::move(member->type_ctor),
                                context->EnterMember(member->identifier->span()), &type_ctor))
      return false;

    std::unique_ptr<Constant> default_value;
    if (member->default_value != nullptr) {
      ConsumeConstant(std::move(member->default_value), &default_value);
    }

    Attribute* allow_struct_defaults = attributes->Get("allow_deprecated_struct_defaults");
    if (!allow_struct_defaults && default_value != nullptr) {
      reporter()->Fail(ErrDeprecatedStructDefaults, mem->span());
    }

    members.emplace_back(std::move(type_ctor), member->identifier->span(), std::move(default_value),
                         std::move(attributes));
  }

  std::unique_ptr<AttributeList> attributes;
  ConsumeAttributeList(std::move(raw_attribute_list), &attributes);
  MaybeOverrideName(*attributes, context.get());

  auto resourceness = Resourceness::kValue;
  if (layout->modifiers != nullptr && layout->modifiers->maybe_resourceness.has_value())
    resourceness = layout->modifiers->maybe_resourceness->value;

  Decl* decl = RegisterDecl(std::make_unique<Struct>(std::move(attributes),
                                                     context->ToName(library(), layout->span()),
                                                     std::move(members), resourceness));
  if (out_decl) {
    *out_decl = decl;
  }
  return decl != nullptr;
}

bool ConsumeStep::ConsumeLayout(std::unique_ptr<RawLayout> layout,
                                const std::shared_ptr<NamingContext>& context,
                                std::unique_ptr<RawAttributeList> raw_attribute_list,
                                Decl** out_decl) {
  switch (layout->kind) {
    case RawLayout::Kind::kBits: {
      return ConsumeValueLayout<Bits>(std::move(layout), context, std::move(raw_attribute_list),
                                      out_decl);
    }
    case RawLayout::Kind::kEnum: {
      return ConsumeValueLayout<Enum>(std::move(layout), context, std::move(raw_attribute_list),
                                      out_decl);
    }
    case RawLayout::Kind::kStruct: {
      return ConsumeStructLayout(std::move(layout), context, std::move(raw_attribute_list),
                                 out_decl);
    }
    case RawLayout::Kind::kTable: {
      return ConsumeOrdinaledLayout<Table>(std::move(layout), context,
                                           std::move(raw_attribute_list), out_decl);
    }
    case RawLayout::Kind::kUnion: {
      return ConsumeOrdinaledLayout<Union>(std::move(layout), context,
                                           std::move(raw_attribute_list), out_decl);
    }
    case RawLayout::Kind::kOverlay: {
      return ConsumeOrdinaledLayout<Overlay>(std::move(layout), context,
                                             std::move(raw_attribute_list), out_decl);
    }
  }
}

bool ConsumeStep::ConsumeTypeConstructor(std::unique_ptr<RawTypeConstructor> raw_type_ctor,
                                         const std::shared_ptr<NamingContext>& context,
                                         std::unique_ptr<RawAttributeList> raw_attribute_list,
                                         std::unique_ptr<TypeConstructor>* out_type_ctor,
                                         Decl** out_inline_decl) {
  std::vector<std::unique_ptr<LayoutParameter>> params;
  std::optional<SourceSpan> params_span;
  if (raw_type_ctor->parameters) {
    params_span = raw_type_ctor->parameters->span();
    for (auto& p : raw_type_ctor->parameters->items) {
      auto param = std::move(p);
      auto span = param->span();
      switch (param->kind) {
        case RawLayoutParameter::Kind::kLiteral: {
          auto literal_param = static_cast<RawLiteralLayoutParameter*>(param.get());
          std::unique_ptr<LiteralConstant> constant;
          ConsumeLiteralConstant(literal_param->literal.get(), &constant);

          std::unique_ptr<LayoutParameter> consumed =
              std::make_unique<LiteralLayoutParameter>(std::move(constant), span);
          params.push_back(std::move(consumed));
          break;
        }
        case RawLayoutParameter::Kind::kType: {
          auto type_param = static_cast<RawTypeLayoutParameter*>(param.get());
          std::unique_ptr<TypeConstructor> type_ctor;
          if (!ConsumeTypeConstructor(std::move(type_param->type_ctor), context,
                                      /*raw_attribute_list=*/nullptr, &type_ctor,
                                      /*out_inline_decl=*/nullptr))
            return false;

          std::unique_ptr<LayoutParameter> consumed =
              std::make_unique<TypeLayoutParameter>(std::move(type_ctor), span);
          params.push_back(std::move(consumed));
          break;
        }
        case RawLayoutParameter::Kind::kIdentifier: {
          auto id_param = static_cast<RawIdentifierLayoutParameter*>(param.get());
          std::unique_ptr<LayoutParameter> consumed =
              std::make_unique<IdentifierLayoutParameter>(Reference(*id_param->identifier), span);
          params.push_back(std::move(consumed));
          break;
        }
      }
    }
  }

  std::vector<std::unique_ptr<Constant>> constraints;
  std::optional<SourceSpan> constraints_span;
  if (raw_type_ctor->constraints) {
    constraints_span = raw_type_ctor->constraints->span();
    for (auto& c : raw_type_ctor->constraints->items) {
      std::unique_ptr<Constant> constraint;
      if (!ConsumeConstant(std::move(c), &constraint))
        return false;
      constraints.push_back(std::move(constraint));
    }
  }

  if (raw_type_ctor->layout_ref->kind == RawLayoutReference::Kind::kInline) {
    auto inline_ref = static_cast<RawInlineLayoutReference*>(raw_type_ctor->layout_ref.get());
    auto attributes = std::move(raw_attribute_list);
    if (inline_ref->attributes != nullptr)
      attributes = std::move(inline_ref->attributes);
    Decl* inline_decl;
    if (!ConsumeLayout(std::move(inline_ref->layout), context, std::move(attributes), &inline_decl))
      return false;

    if (out_inline_decl) {
      *out_inline_decl = inline_decl;
    }
    if (out_type_ctor) {
      *out_type_ctor = std::make_unique<TypeConstructor>(
          raw_type_ctor->span(), Reference(Reference::Target(inline_decl)),
          std::make_unique<LayoutParameterList>(std::move(params), params_span),
          std::make_unique<TypeConstraints>(std::move(constraints), constraints_span));
    }
    return true;
  }

  auto named_ref = static_cast<RawNamedLayoutReference*>(raw_type_ctor->layout_ref.get());
  ZX_ASSERT_MSG(out_type_ctor, "out type ctors should always be provided for a named type ctor");
  *out_type_ctor = std::make_unique<TypeConstructor>(
      raw_type_ctor->span(), Reference(*named_ref->identifier),
      std::make_unique<LayoutParameterList>(std::move(params), params_span),
      std::make_unique<TypeConstraints>(std::move(constraints), constraints_span));
  return true;
}

bool ConsumeStep::ConsumeTypeConstructor(std::unique_ptr<RawTypeConstructor> raw_type_ctor,
                                         const std::shared_ptr<NamingContext>& context,
                                         std::unique_ptr<TypeConstructor>* out_type) {
  return ConsumeTypeConstructor(std::move(raw_type_ctor), context, /*raw_attribute_list=*/nullptr,
                                out_type, /*out_inline_decl=*/nullptr);
}

void ConsumeStep::ConsumeTypeDeclaration(std::unique_ptr<RawTypeDeclaration> type_decl) {
  auto name = Name::CreateSourced(library(), type_decl->identifier->span());
  auto& layout_ref = type_decl->type_ctor->layout_ref;

  if (layout_ref->kind == RawLayoutReference::Kind::kNamed) {
    if (experimental_flags().IsEnabled(ExperimentalFlag::kAllowNewTypes)) {
      ConsumeNewType(std::move(type_decl));
      return;
    }
    auto named_ref = static_cast<RawNamedLayoutReference*>(layout_ref.get());
    reporter()->Fail(ErrNewTypesNotAllowed, type_decl->span(), name, named_ref->span().data());
    return;
  }

  ConsumeTypeConstructor(std::move(type_decl->type_ctor), NamingContext::Create(name),
                         std::move(type_decl->attributes),
                         /*out_type=*/nullptr, /*out_inline_decl=*/nullptr);
}

void ConsumeStep::ConsumeNewType(std::unique_ptr<RawTypeDeclaration> type_decl) {
  ZX_ASSERT(type_decl->type_ctor->layout_ref->kind == RawLayoutReference::Kind::kNamed);
  ZX_ASSERT(experimental_flags().IsEnabled(ExperimentalFlag::kAllowNewTypes));

  std::unique_ptr<AttributeList> attributes;
  ConsumeAttributeList(std::move(type_decl->attributes), &attributes);

  auto new_type_name = Name::CreateSourced(library(), type_decl->identifier->span());

  std::unique_ptr<TypeConstructor> new_type_ctor;
  if (!ConsumeTypeConstructor(std::move(type_decl->type_ctor), NamingContext::Create(new_type_name),
                              &new_type_ctor))
    return;

  RegisterDecl(std::make_unique<NewType>(std::move(attributes), std::move(new_type_name),
                                         std::move(new_type_ctor)));
}

const RawLiteral* ConsumeStep::ConsumeLiteral(std::unique_ptr<RawLiteral> raw_literal) {
  auto ptr = raw_literal.get();
  library()->raw_literals.push_back(std::move(raw_literal));
  return ptr;
}

const RawOrdinal64* ConsumeStep::ConsumeOrdinal(std::unique_ptr<RawOrdinal64> raw_ordinal) {
  auto ptr = raw_ordinal.get();
  library()->raw_ordinals.push_back(std::move(raw_ordinal));
  return ptr;
}

}  // namespace fidlc
