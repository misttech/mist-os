// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/src/names.h"

#include <zircon/assert.h>

#include <sstream>

namespace fidlc {

namespace {

const char* NameNullability(bool is_nullable) { return is_nullable ? "nullable" : "nonnullable"; }

const char* NameNullability(Nullability nullability) {
  return NameNullability(nullability == Nullability::kNullable);
}

std::string NameSize(uint64_t size) {
  if (size == std::numeric_limits<uint64_t>::max())
    return "unbounded";
  std::ostringstream name;
  name << size;
  return name.str();
}

std::string FormatName(const Name& name, std::string_view library_separator,
                       std::string_view name_separator) {
  std::string compiled_name;
  if (name.library() != nullptr && !name.is_intrinsic()) {
    compiled_name += LibraryName(name.library()->name, library_separator);
    compiled_name += name_separator;
  }
  compiled_name += name.full_name();
  return compiled_name;
}

std::string LengthPrefixedString(std::string_view str) {
  std::ostringstream out;
  out << str.length();
  out << str;
  return out.str();
}

}  // namespace

std::string NameHandleSubtype(HandleSubtype subtype) {
  switch (subtype) {
    case HandleSubtype::kHandle:
      return "handle";
    case HandleSubtype::kBti:
      return "bti";
    case HandleSubtype::kChannel:
      return "channel";
    case HandleSubtype::kClock:
      return "clock";
    case HandleSubtype::kEvent:
      return "event";
    case HandleSubtype::kEventpair:
      return "eventpair";
    case HandleSubtype::kException:
      return "exception";
    case HandleSubtype::kFifo:
      return "fifo";
    case HandleSubtype::kGuest:
      return "guest";
    case HandleSubtype::kInterrupt:
      return "interrupt";
    case HandleSubtype::kIob:
      return "iob";
    case HandleSubtype::kIommu:
      return "iommu";
    case HandleSubtype::kJob:
      return "job";
    case HandleSubtype::kDebugLog:
      return "debuglog";
    case HandleSubtype::kMsi:
      return "msi";
    case HandleSubtype::kPager:
      return "pager";
    case HandleSubtype::kPciDevice:
      return "pcidevice";
    case HandleSubtype::kPmt:
      return "pmt";
    case HandleSubtype::kPort:
      return "port";
    case HandleSubtype::kProcess:
      return "process";
    case HandleSubtype::kProfile:
      return "profile";
    case HandleSubtype::kResource:
      return "resource";
    case HandleSubtype::kSocket:
      return "socket";
    case HandleSubtype::kStream:
      return "stream";
    case HandleSubtype::kSuspendToken:
      return "suspendtoken";
    case HandleSubtype::kThread:
      return "thread";
    case HandleSubtype::kTimer:
      return "timer";
    case HandleSubtype::kVcpu:
      return "vcpu";
    case HandleSubtype::kVmar:
      return "vmar";
    case HandleSubtype::kVmo:
      return "vmo";
  }
}

std::string NameHandleRights(RightsWrappedType rights) { return std::to_string(rights); }

std::string NameRawLiteralKind(RawLiteral::Kind kind) {
  switch (kind) {
    case RawLiteral::Kind::kDocComment:
    case RawLiteral::Kind::kString:
      return "string";
    case RawLiteral::Kind::kNumeric:
      return "numeric";
    case RawLiteral::Kind::kBool:
      return "bool";
  }
}

std::string NameFlatName(const Name& name) { return FormatName(name, ".", "/"); }

std::string NameFlatTypeKind(const Type* type) {
  switch (type->kind) {
    case Type::Kind::kArray:
      if (static_cast<const ArrayType*>(type)->IsStringArray()) {
        return "string_array";
      }
      return "array";
    case Type::Kind::kVector:
      return "vector";
    case Type::Kind::kZxExperimentalPointer:
      return "experimental_pointer";
    case Type::Kind::kString:
      return "string";
    case Type::Kind::kHandle:
      return "handle";
    case Type::Kind::kTransportSide: {
      // TODO(https://fxbug.dev/42149402): transition the JSON and other backends to using
      // client/server end
      auto channel_end = static_cast<const TransportSideType*>(type);
      return (channel_end->end == TransportSide::kClient) ? "identifier" : "request";
    }
    case Type::Kind::kPrimitive:
      return "primitive";
    case Type::Kind::kInternal:
      return "internal";
    // TODO(https://fxbug.dev/42149402): transition the JSON and other backends to using box
    case Type::Kind::kBox:
    case Type::Kind::kIdentifier:
      return "identifier";
    case Type::Kind::kUntypedNumeric:
      ZX_PANIC("should not have untyped numeric here");
  }
}

std::string NameFlatConstantKind(Constant::Kind kind) {
  switch (kind) {
    case Constant::Kind::kIdentifier:
      return "identifier";
    case Constant::Kind::kLiteral:
      return "literal";
    case Constant::Kind::kBinaryOperator:
      return "binary_operator";
  }
}

std::string NameHandleZXObjType(HandleSubtype subtype) {
  switch (subtype) {
    case HandleSubtype::kHandle:
      return "ZX_OBJ_TYPE_NONE";
    case HandleSubtype::kBti:
      return "ZX_OBJ_TYPE_BTI";
    case HandleSubtype::kChannel:
      return "ZX_OBJ_TYPE_CHANNEL";
    case HandleSubtype::kClock:
      return "ZX_OBJ_TYPE_CLOCK";
    case HandleSubtype::kEvent:
      return "ZX_OBJ_TYPE_EVENT";
    case HandleSubtype::kEventpair:
      return "ZX_OBJ_TYPE_EVENTPAIR";
    case HandleSubtype::kException:
      return "ZX_OBJ_TYPE_EXCEPTION";
    case HandleSubtype::kFifo:
      return "ZX_OBJ_TYPE_FIFO";
    case HandleSubtype::kGuest:
      return "ZX_OBJ_TYPE_GUEST";
    case HandleSubtype::kInterrupt:
      return "ZX_OBJ_TYPE_INTERRUPT";
    case HandleSubtype::kIob:
      return "ZX_OBJ_TYPE_IOB";
    case HandleSubtype::kIommu:
      return "ZX_OBJ_TYPE_IOMMU";
    case HandleSubtype::kJob:
      return "ZX_OBJ_TYPE_JOB";
    case HandleSubtype::kDebugLog:
      return "ZX_OBJ_TYPE_LOG";
    case HandleSubtype::kMsi:
      return "ZX_OBJ_TYPE_MSI";
    case HandleSubtype::kPager:
      return "ZX_OBJ_TYPE_PAGER";
    case HandleSubtype::kPciDevice:
      return "ZX_OBJ_TYPE_PCI_DEVICE";
    case HandleSubtype::kPmt:
      return "ZX_OBJ_TYPE_PMT";
    case HandleSubtype::kPort:
      return "ZX_OBJ_TYPE_PORT";
    case HandleSubtype::kProcess:
      return "ZX_OBJ_TYPE_PROCESS";
    case HandleSubtype::kProfile:
      return "ZX_OBJ_TYPE_PROFILE";
    case HandleSubtype::kResource:
      return "ZX_OBJ_TYPE_RESOURCE";
    case HandleSubtype::kSocket:
      return "ZX_OBJ_TYPE_SOCKET";
    case HandleSubtype::kStream:
      return "ZX_OBJ_TYPE_STREAM";
    case HandleSubtype::kSuspendToken:
      return "ZX_OBJ_TYPE_SUSPEND_TOKEN";
    case HandleSubtype::kThread:
      return "ZX_OBJ_TYPE_THREAD";
    case HandleSubtype::kTimer:
      return "ZX_OBJ_TYPE_TIMER";
    case HandleSubtype::kVcpu:
      return "ZX_OBJ_TYPE_VCPU";
    case HandleSubtype::kVmar:
      return "ZX_OBJ_TYPE_VMAR";
    case HandleSubtype::kVmo:
      return "ZX_OBJ_TYPE_VMO";
  }
}

std::string NameUnionTag(std::string_view union_name, const Union::Member& member) {
  return std::string(union_name) + "Tag_" + NameIdentifier(member.name);
}

std::string NameFlatConstant(const Constant* constant) {
  switch (constant->kind) {
    case Constant::Kind::kLiteral: {
      auto literal_constant = static_cast<const LiteralConstant*>(constant);
      return std::string(literal_constant->literal->span().data());
    }
    case Constant::Kind::kIdentifier: {
      auto identifier_constant = static_cast<const IdentifierConstant*>(constant);
      return NameFlatName(identifier_constant->reference.resolved().name());
    }
    case Constant::Kind::kBinaryOperator: {
      return std::string("binary operator");
    }
  }  // switch
}

void NameFlatTypeHelper(std::ostringstream& buf, const Type* type) {
  buf << NameFlatName(type->name);
  switch (type->kind) {
    case Type::Kind::kArray: {
      const auto* array_type = static_cast<const ArrayType*>(type);
      buf << '<';
      NameFlatTypeHelper(buf, array_type->element_type);
      if (*array_type->element_count != SizeValue::Max()) {
        buf << ", ";
        buf << array_type->element_count->value;
      }
      buf << '>';
      break;
    }
    case Type::Kind::kVector: {
      const auto* vector_type = static_cast<const VectorType*>(type);
      buf << '<';
      NameFlatTypeHelper(buf, vector_type->element_type);
      buf << '>';
      if (vector_type->ElementCount() != SizeValue::Max().value) {
        buf << ':';
        buf << vector_type->ElementCount();
      }
      break;
    }
    case Type::Kind::kString: {
      const auto* string_type = static_cast<const StringType*>(type);
      if (string_type->MaxSize() != SizeValue::Max().value) {
        buf << ':';
        buf << string_type->MaxSize();
      }
      break;
    }
    case Type::Kind::kZxExperimentalPointer: {
      const auto* pointer_type = static_cast<const ZxExperimentalPointerType*>(type);
      buf << '<';
      NameFlatTypeHelper(buf, pointer_type->pointee_type);
      buf << '>';
      break;
    }
    case Type::Kind::kHandle: {
      const auto* handle_type = static_cast<const HandleType*>(type);
      if (handle_type->subtype != HandleSubtype::kHandle) {
        buf << ':';
        buf << NameHandleSubtype(handle_type->subtype);
      }
      break;
    }
    case Type::Kind::kTransportSide: {
      const auto* transport_side = static_cast<const TransportSideType*>(type);
      buf << (transport_side->end == TransportSide::kClient ? "client" : "server");
      buf << ':';
      buf << NameFlatName(transport_side->protocol_decl->name);
      break;
    }
    case Type::Kind::kBox: {
      const auto* box_type = static_cast<const BoxType*>(type);
      buf << '<';
      buf << NameFlatName(box_type->boxed_type->name);
      buf << '>';
      break;
    }
    case Type::Kind::kPrimitive:
    case Type::Kind::kInternal:
    case Type::Kind::kIdentifier:
    case Type::Kind::kUntypedNumeric:
      // Like Stars, they are known by name.
      break;
  }  // switch
  // TODO(https://fxbug.dev/42175844): Use the new syntax, `:optional`.
  if (type->IsNullable()) {
    buf << '?';
  }
}

std::string NameFlatType(const Type* type) {
  std::ostringstream buf;
  NameFlatTypeHelper(buf, type);
  return buf.str();
}

std::string NameIdentifier(SourceSpan name) { return std::string(name.data()); }

std::string NameLibrary(const std::vector<std::unique_ptr<RawIdentifier>>& components) {
  std::string id;
  for (const auto& component : components) {
    if (!id.empty()) {
      id.append(".");
    }
    id.append(component->span().data());
  }
  return id;
}

std::string NameLibrary(const std::vector<std::string_view>& library_name) {
  return StringJoin(library_name, ".");
}

std::string NameLibraryCHeader(const std::vector<std::string_view>& library_name) {
  return StringJoin(library_name, "/") + "/c/fidl.h";
}

std::string NameDiscoverable(const Protocol& protocol) {
  return FormatName(protocol.name, ".", ".");
}

std::string NameMethod(std::string_view protocol_name, const Protocol::Method& method) {
  return std::string(protocol_name) + NameIdentifier(method.name);
}

std::string NameOrdinal(std::string_view method_name) {
  std::string ordinal_name(method_name);
  ordinal_name += "Ordinal";
  return ordinal_name;
}

std::string NameMessage(std::string_view method_name, MessageKind kind) {
  std::string message_name(method_name);
  switch (kind) {
    case MessageKind::kRequest:
      message_name += "RequestMessage";
      break;
    case MessageKind::kResponse:
      message_name += "ResponseMessage";
      break;
    case MessageKind::kEvent:
      message_name += "EventMessage";
      break;
  }
  return message_name;
}

std::string NameTable(std::string_view table_name) { return std::string(table_name) + "Table"; }

std::string NamePointer(std::string_view name) {
  std::string pointer_name("Pointer");
  pointer_name += LengthPrefixedString(name);
  return pointer_name;
}

std::string NameMembers(std::string_view name) {
  std::string members_name("Members");
  members_name += LengthPrefixedString(name);
  return members_name;
}

std::string NameFields(std::string_view name) {
  std::string fields_name("Fields");
  fields_name += LengthPrefixedString(name);
  return fields_name;
}

std::string NameFieldsAltField(std::string_view name, uint32_t field_num) {
  std::ostringstream fields_alt_field_name;
  fields_alt_field_name << NameFields(name);
  fields_alt_field_name << "_field";
  fields_alt_field_name << field_num;
  fields_alt_field_name << "_alt_field";
  return fields_alt_field_name.str();
}

std::string NameCodedName(const Name& name) { return FormatName(name, "_", "_"); }

std::string NameCodedNullableName(const Name& name) {
  std::ostringstream nullable_name;
  nullable_name << NameCodedName(name);
  nullable_name << "NullableRef";
  return nullable_name.str();
}

std::string NameCodedHandle(HandleSubtype subtype, RightsWrappedType rights,
                            Nullability nullability) {
  std::string name("Handle");
  name += NameHandleSubtype(subtype);
  name += NameHandleRights(rights);
  name += NameNullability(nullability);
  return name;
}

std::string NameCodedProtocolHandle(std::string_view protocol_name, Nullability nullability) {
  std::string name("Protocol");
  name += LengthPrefixedString(protocol_name);
  name += NameNullability(nullability);
  return name;
}

std::string NameCodedRequestHandle(std::string_view protocol_name, Nullability nullability) {
  std::string name("Request");
  name += LengthPrefixedString(protocol_name);
  name += NameNullability(nullability);
  return name;
}

std::string NameCodedArray(std::string_view element_name, uint64_t size) {
  std::string name("Array");
  name += NameSize(size);
  name += "_";
  name += LengthPrefixedString(element_name);
  return name;
}

std::string NameCodedVector(std::string_view element_name, uint64_t max_size,
                            Nullability nullability) {
  std::string name("Vector");
  name += NameSize(max_size);
  name += NameNullability(nullability);
  name += LengthPrefixedString(element_name);
  return name;
}

std::string NameCodedString(uint64_t max_size, Nullability nullability) {
  std::string name("String");
  name += NameSize(max_size);
  name += NameNullability(nullability);
  return name;
}

std::string NameCodedZxExperimentalPointer(std::string_view pointee_name) {
  std::string name("ZxExperimentalPointer");
  name += LengthPrefixedString(pointee_name);
  return name;
}

}  // namespace fidlc
