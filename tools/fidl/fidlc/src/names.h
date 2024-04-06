// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_SRC_NAMES_H_
#define TOOLS_FIDL_FIDLC_SRC_NAMES_H_

#include <string>
#include <string_view>

#include "tools/fidl/fidlc/src/flat_ast.h"
#include "tools/fidl/fidlc/src/properties.h"
#include "tools/fidl/fidlc/src/raw_ast.h"

namespace fidlc {

std::string NameIdentifier(SourceSpan name);

std::string NameLibrary(const std::vector<std::unique_ptr<RawIdentifier>>& components);
std::string NameLibrary(const std::vector<std::string_view>& library_name);
std::string NameLibraryCHeader(const std::vector<std::string_view>& library_name);

std::string NameHandleSubtype(HandleSubtype subtype);
std::string NameHandleRights(RightsWrappedType rights);
std::string NameHandleZXObjType(HandleSubtype subtype);

std::string NameRawLiteralKind(RawLiteral::Kind kind);

std::string NameFlatName(const Name& name);
std::string NameFlatConstantKind(Constant::Kind kind);
std::string NameFlatTypeKind(const Type* type);
std::string NameUnionTag(std::string_view union_name, const Union::Member& member);
std::string NameFlatConstant(const Constant* constant);
std::string NameFlatBinaryOperator(BinaryOperatorConstant::Operator op);
std::string NameFlatType(const Type* type);
std::string NameDiscoverable(const Protocol& protocol);
std::string NameMethod(std::string_view protocol_name, const Protocol::Method& method);
std::string NameOrdinal(std::string_view method_name);
std::string NameMessage(std::string_view method_name, MessageKind kind);

std::string NameTable(std::string_view table_name);
std::string NamePointer(std::string_view name);
std::string NameMembers(std::string_view name);
std::string NameFields(std::string_view name);
std::string NameFieldsAltField(std::string_view name, uint32_t field_num);

std::string NameCodedName(const Name& name);
std::string NameCodedNullableName(const Name& name);
std::string NameCodedHandle(HandleSubtype subtype, RightsWrappedType rights,
                            Nullability nullability);
std::string NameCodedProtocolHandle(std::string_view protocol_name, Nullability nullability);
std::string NameCodedRequestHandle(std::string_view protocol_name, Nullability nullability);
std::string NameCodedArray(std::string_view element_name, uint64_t size);
std::string NameCodedVector(std::string_view element_name, uint64_t max_size,
                            Nullability nullability);
std::string NameCodedString(uint64_t max_size, Nullability nullability);
std::string NameCodedZxExperimentalPointer(std::string_view pointee_name);

}  // namespace fidlc

#endif  // TOOLS_FIDL_FIDLC_SRC_NAMES_H_
