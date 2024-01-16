// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/include/fidl/type_shape.h"

#include <zircon/assert.h>

#include <algorithm>

#include <safemath/clamped_math.h>

#include "tools/fidl/fidlc/include/fidl/flat/visitor.h"
#include "tools/fidl/fidlc/include/fidl/flat_ast.h"
#include "tools/fidl/fidlc/include/fidl/recursion_detector.h"

// TODO(https://fxbug.dev/7680): We may want to fail instead of saturating.
using DataSize = safemath::ClampedNumeric<uint32_t>;

namespace std {

// Add a partial specialization for std::numeric_limits<DataSize>::max(), which would
// otherwise return 0 (see
// <https://stackoverflow.com/questions/35575276/why-does-stdnumeric-limitssecondsmax-return-0> if
// you're curious about why.)
template <>
struct numeric_limits<DataSize> {
  static constexpr DataSize max() noexcept { return DataSize(numeric_limits<uint32_t>::max()); }
};

static_assert(numeric_limits<DataSize>::max() == numeric_limits<uint32_t>::max());

}  // namespace std

namespace fidlc {

namespace {

// Given |offset| in bytes, returns how many padding bytes need to be added to |offset| to be
// aligned to |alignment|.
DataSize Padding(const DataSize offset, const DataSize alignment) {
  // See <https://en.wikipedia.org/wiki/Data_structure_alignment#Computing_padding> for a context on
  // computing the amount of padding required.

  // The following expression is from <https://stackoverflow.com/a/32104582> and is equivalent to
  // "(alignment - (offset % alignment)) % alignment".
  return (~offset.RawValue() + 1) & (alignment.RawValue() - 1);
}

// Given |size| and |alignment| in bytes, returns |size| "rounded up" to the next |alignment|
// interval.
DataSize AlignTo(uint32_t size, uint64_t alignment) {
  // From <https://en.wikipedia.org/wiki/Data_structure_alignment#Computing_padding>.
  return (size + (alignment - 1)) & -alignment;
}

// Given |size|, returns |size| "rounded up" to the next alignment interval required by an
// out-of-line FIDL object.
DataSize ObjectAlign(uint32_t size) { return AlignTo(size, 8); }

constexpr uint32_t kHandleSize = 4;

DataSize UnalignedSize(const Object& object, WireFormat wire_format);
DataSize UnalignedSize(const Object* object, WireFormat wire_format);
[[maybe_unused]] DataSize Alignment(const Object& object, WireFormat wire_format);
[[maybe_unused]] DataSize Alignment(const Object* object, WireFormat wire_format);
DataSize Depth(const Object& object, WireFormat wire_format);
[[maybe_unused]] DataSize Depth(const Object* object, WireFormat wire_format);
DataSize MaxHandles(const Object& object);
[[maybe_unused]] DataSize MaxHandles(const Object* object);
DataSize MaxOutOfLine(const Object& object, WireFormat wire_format);
[[maybe_unused]] DataSize MaxOutOfLine(const Object* object, WireFormat wire_format);
bool HasPadding(const Object& object, WireFormat wire_format);
[[maybe_unused]] bool HasPadding(const Object* object, WireFormat wire_format);
bool HasFlexibleEnvelope(const Object& object, WireFormat wire_format);
[[maybe_unused]] bool HasFlexibleEnvelope(const Object* object, WireFormat wire_format);

DataSize AlignedSize(const Object& object, WireFormat wire_format) {
  return AlignTo(UnalignedSize(object, wire_format), Alignment(object, wire_format));
}

[[maybe_unused]] DataSize AlignedSize(const Object* object, WireFormat wire_format) {
  return AlignedSize(*object, wire_format);
}

template <typename T>
class TypeShapeVisitor : public Object::Visitor<T> {
 public:
  TypeShapeVisitor() = delete;
  explicit TypeShapeVisitor(WireFormat wire_format) : wire_format_(wire_format) {}

 protected:
  WireFormat wire_format() const { return wire_format_; }

 private:
  const WireFormat wire_format_;
};

class UnalignedSizeVisitor final : public TypeShapeVisitor<DataSize> {
 public:
  using TypeShapeVisitor<DataSize>::TypeShapeVisitor;

  std::any Visit(const ArrayType& object) override {
    return UnalignedSize(object.element_type) * object.element_count->value;
  }

  std::any Visit(const VectorType& object) override { return DataSize(16); }

  std::any Visit(const StringType& object) override { return DataSize(16); }

  std::any Visit(const HandleType& object) override { return DataSize(kHandleSize); }

  std::any Visit(const PrimitiveType& object) override {
    switch (object.subtype) {
      case PrimitiveSubtype::kBool:
      case PrimitiveSubtype::kInt8:
      case PrimitiveSubtype::kUint8:
      case PrimitiveSubtype::kZxUchar:
        return DataSize(1);
      case PrimitiveSubtype::kInt16:
      case PrimitiveSubtype::kUint16:
        return DataSize(2);
      case PrimitiveSubtype::kInt32:
      case PrimitiveSubtype::kUint32:
      case PrimitiveSubtype::kFloat32:
        return DataSize(4);
      case PrimitiveSubtype::kInt64:
      case PrimitiveSubtype::kUint64:
      case PrimitiveSubtype::kZxUsize64:
      case PrimitiveSubtype::kZxUintptr64:
      case PrimitiveSubtype::kFloat64:
        return DataSize(8);
    }
  }

  std::any Visit(const InternalType& object) override {
    switch (object.subtype) {
      case InternalSubtype::kFrameworkErr:
        return DataSize(4);
    }
  }

  std::any Visit(const IdentifierType& object) override {
    switch (object.nullability) {
      case Nullability::kNullable:
        switch (object.type_decl->kind) {
          case Decl::Kind::kProtocol:
          case Decl::Kind::kService:
            return DataSize(kHandleSize);
          // TODO(https://fxbug.dev/70186): this should be handled as a box and nullable structs
          // should never be visited
          case Decl::Kind::kStruct:
            return DataSize(8);
          case Decl::Kind::kUnion:
            switch (wire_format()) {
              case WireFormat::kV2:
                return DataSize(16);
            }
          case Decl::Kind::kBits:
          case Decl::Kind::kBuiltin:
          case Decl::Kind::kConst:
          case Decl::Kind::kEnum:
          case Decl::Kind::kNewType:
          case Decl::Kind::kResource:
          case Decl::Kind::kTable:
          case Decl::Kind::kAlias:
          case Decl::Kind::kOverlay:
            ZX_PANIC("UnalignedSize(IdentifierType&) called on invalid nullable kind");
        }
      case Nullability::kNonnullable: {
        return UnalignedSize(object.type_decl);
      }
    }
  }

  std::any Visit(const BoxType& object) override { return DataSize(8); }

  std::any Visit(const TransportSideType& object) override { return DataSize(kHandleSize); }

  std::any Visit(const Enum& object) override { return UnalignedSize(object.subtype_ctor->type); }

  std::any Visit(const Bits& object) override { return UnalignedSize(object.subtype_ctor->type); }

  std::any Visit(const Service& object) override { return DataSize(kHandleSize); }

  std::any Visit(const NewType& object) override { return UnalignedSize(object.type_ctor->type); }

  std::any Visit(const Struct& object) override {
    DataSize size = 0;
    if (object.members.empty()) {
      return DataSize(1 + size);
    }

    for (const auto& member : object.members) {
      const DataSize member_size = UnalignedSize(member) + member.fieldshape(wire_format()).padding;
      size += member_size;
    }

    return size;
  }

  std::any Visit(const Struct::Member& object) override {
    return UnalignedSize(object.type_ctor->type);
  }

  std::any Visit(const Table& object) override { return DataSize(16); }

  std::any Visit(const Table::Member& object) override {
    return object.maybe_used ? UnalignedSize(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Table::Member::Used& object) override {
    return UnalignedSize(object.type_ctor->type);
  }

  std::any Visit(const Union& object) override {
    switch (wire_format()) {
      case WireFormat::kV2:
        return DataSize(16);
    }
  }

  std::any Visit(const Union::Member& object) override {
    return object.maybe_used ? UnalignedSize(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Union::Member::Used& object) override {
    return UnalignedSize(object.type_ctor->type);
  }

  std::any Visit(const Overlay& object) override {
    DataSize max_member_size = 0;
    for (const auto& member : object.members) {
      if (member.maybe_used) {
        max_member_size =
            std::max(max_member_size, UnalignedSize(member.maybe_used->type_ctor->type));
      }
    }
    return max_member_size + DataSize(8);
  }

  std::any Visit(const Overlay::Member& object) override {
    return object.maybe_used ? UnalignedSize(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Overlay::Member::Used& object) override {
    return UnalignedSize(object.type_ctor->type);
  }

  std::any Visit(const Protocol& object) override { return DataSize(kHandleSize); }

  std::any Visit(const ZxExperimentalPointerType& object) override { return DataSize(8); }

 private:
  DataSize UnalignedSize(const Object& object) { return object.Accept(this); }

  DataSize UnalignedSize(const Object* object) { return UnalignedSize(*object); }
};

class AlignmentVisitor final : public TypeShapeVisitor<DataSize> {
 public:
  using TypeShapeVisitor<DataSize>::TypeShapeVisitor;

  std::any Visit(const ArrayType& object) override { return Alignment(object.element_type); }

  std::any Visit(const VectorType& object) override { return DataSize(8); }

  std::any Visit(const StringType& object) override { return DataSize(8); }

  std::any Visit(const HandleType& object) override { return DataSize(kHandleSize); }

  std::any Visit(const PrimitiveType& object) override {
    return UnalignedSize(object, wire_format());
  }

  std::any Visit(const InternalType& object) override {
    switch (object.subtype) {
      case InternalSubtype::kFrameworkErr:
        return DataSize(4);
    }
  }

  std::any Visit(const IdentifierType& object) override {
    switch (object.nullability) {
      case Nullability::kNullable:
        switch (object.type_decl->kind) {
          case Decl::Kind::kProtocol:
          case Decl::Kind::kService:
            return DataSize(kHandleSize);
          // TODO(https://fxbug.dev/70186): this should be handled as a box and nullable structs
          // should never be visited
          case Decl::Kind::kStruct:
          case Decl::Kind::kUnion:
            return DataSize(8);
          case Decl::Kind::kBits:
          case Decl::Kind::kBuiltin:
          case Decl::Kind::kConst:
          case Decl::Kind::kEnum:
          case Decl::Kind::kNewType:
          case Decl::Kind::kResource:
          case Decl::Kind::kTable:
          case Decl::Kind::kAlias:
          case Decl::Kind::kOverlay:
            ZX_PANIC("Alignment(IdentifierType&) called on invalid nullable kind");
        }
      case Nullability::kNonnullable:
        return Alignment(object.type_decl);
    }
  }

  std::any Visit(const BoxType& object) override { return DataSize(8); }

  std::any Visit(const TransportSideType& object) override { return DataSize(kHandleSize); }

  std::any Visit(const Enum& object) override { return Alignment(object.subtype_ctor->type); }

  std::any Visit(const Bits& object) override { return Alignment(object.subtype_ctor->type); }

  std::any Visit(const Service& object) override { return DataSize(kHandleSize); }

  std::any Visit(const NewType& object) override { return Alignment(object.type_ctor->type); }

  std::any Visit(const Struct& object) override {
    if (object.recursive) {
      // |object| is recursive, therefore there must be a pointer to this struct in the recursion
      // chain, with pointer-sized alignment.
      return DataSize(8);
    }

    if (object.members.empty()) {
      // Empty struct.

      return DataSize(1);
    }

    DataSize alignment = 0;

    for (const auto& member : object.members) {
      alignment = std::max(alignment, Alignment(member));
    }

    return alignment;
  }

  std::any Visit(const Struct::Member& object) override {
    return Alignment(object.type_ctor->type);
  }

  std::any Visit(const Overlay& object) override {
    // Ordinal always has alignment 8, so the overlay does too.
    return DataSize(8);
  }

  std::any Visit(const Overlay::Member& object) override {
    return object.maybe_used ? Alignment(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Overlay::Member::Used& object) override {
    return Alignment(object.type_ctor->type);
  }

  std::any Visit(const Table& object) override { return DataSize(8); }

  std::any Visit(const Table::Member& object) override {
    return object.maybe_used ? Alignment(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Table::Member::Used& object) override {
    return Alignment(object.type_ctor->type);
  }

  std::any Visit(const Union& object) override { return DataSize(8); }

  std::any Visit(const Union::Member& object) override {
    return object.maybe_used ? Alignment(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const UnionMember::Used& object) override {
    return Alignment(object.type_ctor->type);
  }

  std::any Visit(const Protocol& object) override { return DataSize(kHandleSize); }

  std::any Visit(const ZxExperimentalPointerType& object) override { return DataSize(8); }

 private:
  DataSize Alignment(const Object& object) { return object.Accept(this); }

  DataSize Alignment(const Object* object) { return Alignment(*object); }
};

class DepthVisitor : public TypeShapeVisitor<DataSize> {
 public:
  using TypeShapeVisitor<DataSize>::TypeShapeVisitor;

  std::any Visit(const ArrayType& object) override { return Depth(object.element_type); }

  std::any Visit(const VectorType& object) override {
    return DataSize(1) + Depth(object.element_type);
  }

  std::any Visit(const StringType& object) override { return DataSize(1); }

  std::any Visit(const HandleType& object) override { return DataSize(0); }

  std::any Visit(const PrimitiveType& object) override { return DataSize(0); }

  std::any Visit(const InternalType& object) override {
    switch (object.subtype) {
      case InternalSubtype::kFrameworkErr:
        return DataSize(0);
    }
  }

  std::any Visit(const IdentifierType& object) override {
    thread_local RecursionDetector recursion_detector;

    auto guard = recursion_detector.Enter(&object);
    if (!guard) {
      return DataSize(0);
    }

    switch (object.nullability) {
      case Nullability::kNullable:
        switch (object.type_decl->kind) {
          case Decl::Kind::kProtocol:
          case Decl::Kind::kService:
            return DataSize(0);
          case Decl::Kind::kStruct:
            return DataSize(1) + Depth(object.type_decl);
          case Decl::Kind::kUnion:
            return Depth(object.type_decl);
          case Decl::Kind::kBits:
          case Decl::Kind::kBuiltin:
          case Decl::Kind::kConst:
          case Decl::Kind::kEnum:
          case Decl::Kind::kNewType:
          case Decl::Kind::kResource:
          case Decl::Kind::kTable:
          case Decl::Kind::kAlias:
          case Decl::Kind::kOverlay:
            ZX_PANIC("Depth(IdentifierType&) called on invalid nullable kind");
        }
      case Nullability::kNonnullable:
        switch (object.type_decl->kind) {
          case Decl::Kind::kBits:
          case Decl::Kind::kConst:
          case Decl::Kind::kEnum:
          case Decl::Kind::kProtocol:
          case Decl::Kind::kResource:
          case Decl::Kind::kService:
            return DataSize(0);
          case Decl::Kind::kNewType:
          case Decl::Kind::kStruct:
          case Decl::Kind::kTable:
          case Decl::Kind::kAlias:
          case Decl::Kind::kUnion:
          case Decl::Kind::kOverlay:
            return Depth(object.type_decl);
          case Decl::Kind::kBuiltin:
            ZX_PANIC("unexpected builtin");
        }
      default:
        ZX_PANIC("unexpected nullability type");
    }
  }

  std::any Visit(const NewType& object) override { return Depth(object.type_ctor->type); }

  std::any Visit(const BoxType& object) override {
    // The nullable struct case will add one, no need to do it here.
    return Depth(object.boxed_type);
  }

  std::any Visit(const TransportSideType& object) override { return DataSize(0); }

  std::any Visit(const Enum& object) override { return Depth(object.subtype_ctor->type); }

  std::any Visit(const Bits& object) override { return Depth(object.subtype_ctor->type); }

  std::any Visit(const Service& object) override { return DataSize(0); }

  std::any Visit(const Struct& object) override {
    if (object.recursive) {
      return std::numeric_limits<DataSize>::max();
    }

    DataSize max_depth = 0;

    for (const auto& member : object.members) {
      max_depth = std::max(max_depth, Depth(member));
    }

    return max_depth;
  }

  std::any Visit(const Struct::Member& object) override { return Depth(object.type_ctor->type); }

  std::any Visit(const Overlay& object) override {
    DataSize max_depth = 0;

    for (const auto& member : object.members) {
      max_depth = std::max(max_depth, Depth(member));
    }

    return max_depth;
  }

  std::any Visit(const Overlay::Member& object) override {
    return object.maybe_used ? Depth(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Overlay::Member::Used& object) override {
    return Depth(object.type_ctor->type);
  }

  std::any Visit(const Table& object) override {
    DataSize max_depth = 0;

    for (const auto& member : object.members) {
      max_depth = std::max(max_depth, Depth(member));
    }

    return DataSize(1) + max_depth;
  }

  std::any Visit(const Table::Member& object) override {
    return object.maybe_used ? Depth(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Table::Member::Used& object) override {
    return DataSize(1) + Depth(object.type_ctor->type);
  }

  std::any Visit(const Union& object) override {
    DataSize max_depth;

    for (const auto& member : object.members) {
      max_depth = std::max(max_depth, Depth(member));
    }

    return DataSize(1) + max_depth;
  }

  std::any Visit(const Union::Member& object) override {
    return object.maybe_used ? Depth(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Union::Member::Used& object) override {
    return Depth(object.type_ctor->type);
  }

  std::any Visit(const Protocol& object) override { return DataSize(0); }

  std::any Visit(const ZxExperimentalPointerType& object) override {
    return DataSize(1) + Depth(object.pointee_type);
  }

 protected:
  DataSize Depth(const Object& object) { return object.Accept(this); }

  DataSize Depth(const Object* object) { return Depth(*object); }
};

class MaxHandlesVisitor final : public Object::Visitor<DataSize> {
 public:
  std::any Visit(const ArrayType& object) override {
    return MaxHandles(object.element_type) * object.element_count->value;
  }

  std::any Visit(const VectorType& object) override {
    return MaxHandles(object.element_type) * object.ElementCount();
  }

  std::any Visit(const StringType& object) override { return DataSize(0); }

  std::any Visit(const HandleType& object) override { return DataSize(1); }

  std::any Visit(const PrimitiveType& object) override { return DataSize(0); }

  std::any Visit(const InternalType& object) override {
    switch (object.subtype) {
      case InternalSubtype::kFrameworkErr:
        return DataSize(0);
    }
  }

  std::any Visit(const IdentifierType& object) override {
    thread_local RecursionDetector recursion_detector;

    // TODO(https://fxbug.dev/36327): This code is technically incorrect; see the visit(Struct&)
    // overload for more details.
    auto guard = recursion_detector.Enter(&object);
    if (!guard) {
      return DataSize(0);
    }

    return MaxHandles(object.type_decl);
  }

  std::any Visit(const BoxType& object) override { return MaxHandles(object.boxed_type); }

  std::any Visit(const TransportSideType& object) override { return DataSize(1); }

  std::any Visit(const Enum& object) override { return MaxHandles(object.subtype_ctor->type); }

  std::any Visit(const Bits& object) override { return MaxHandles(object.subtype_ctor->type); }

  std::any Visit(const Service& object) override { return DataSize(1); }

  std::any Visit(const Struct& object) override {
    // TODO(https://fxbug.dev/36327): This is technically incorrect: if a struct is recursive, it
    // may not directly contain a handle, but could contain e.g. a struct that contains a handle. In
    // that case, this code will return 0 instead of std::numeric_limits<DataSize>::max(). This does
    // pass all current tests and Fuchsia compilation, so fixing it isn't super-urgent.
    if (object.recursive) {
      for (const auto& member : object.members) {
        switch (member.type_ctor->type->kind) {
          case Type::Kind::kHandle:
          case Type::Kind::kTransportSide:
            return std::numeric_limits<DataSize>::max();
          case Type::Kind::kArray:
          case Type::Kind::kVector:
          case Type::Kind::kZxExperimentalPointer:
          case Type::Kind::kString:
          case Type::Kind::kPrimitive:
          case Type::Kind::kInternal:
          case Type::Kind::kIdentifier:
          case Type::Kind::kBox:
            continue;
          case Type::Kind::kUntypedNumeric:
            ZX_PANIC("should not have untyped numeric here");
        }
      }

      return DataSize(0);
    }

    DataSize max_handles = 0;

    for (const auto& member : object.members) {
      max_handles += MaxHandles(member);
    }

    return max_handles;
  }

  std::any Visit(const NewType& object) override { return MaxHandles(object.type_ctor->type); }

  std::any Visit(const Struct::Member& object) override {
    return MaxHandles(object.type_ctor->type);
  }

  std::any Visit(const Overlay::Member& object) override {
    return object.maybe_used ? MaxHandles(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Overlay::Member::Used& object) override {
    return MaxHandles(object.type_ctor->type);
  }

  std::any Visit(const Table& object) override {
    DataSize max_handles = 0;

    for (const auto& member : object.members) {
      max_handles += MaxHandles(member);
    }

    return max_handles;
  }

  std::any Visit(const Table::Member& object) override {
    return object.maybe_used ? MaxHandles(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Table::Member::Used& object) override {
    return MaxHandles(object.type_ctor->type);
  }

  std::any Visit(const Union& object) override {
    DataSize max_handles;

    for (const auto& member : object.members) {
      max_handles = std::max(max_handles, MaxHandles(member));
    }

    return max_handles;
  }

  std::any Visit(const Overlay& object) override {
    DataSize max_handles;

    for (const auto& member : object.members) {
      max_handles = std::max(max_handles, MaxHandles(member));
    }

    return max_handles;
  }

  std::any Visit(const Union::Member& object) override {
    return object.maybe_used ? MaxHandles(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Union::Member::Used& object) override {
    return MaxHandles(object.type_ctor->type);
  }

  std::any Visit(const Protocol& object) override { return DataSize(1); }

  std::any Visit(const ZxExperimentalPointerType& object) override {
    return MaxHandles(object.pointee_type);
  }
};

class MaxOutOfLineVisitor final : public TypeShapeVisitor<DataSize> {
 public:
  using TypeShapeVisitor<DataSize>::TypeShapeVisitor;

  std::any Visit(const ArrayType& object) override {
    return MaxOutOfLine(object.element_type) * DataSize(object.element_count->value);
  }

  std::any Visit(const VectorType& object) override {
    return ObjectAlign(UnalignedSize(object.element_type, wire_format()) * object.ElementCount()) +
           ObjectAlign(MaxOutOfLine(object.element_type)) * object.ElementCount();
  }

  std::any Visit(const StringType& object) override {
    return object.MaxSize() != SizeValue::Max().value ? ObjectAlign(object.MaxSize())
                                                      : std::numeric_limits<DataSize>::max();
  }

  std::any Visit(const HandleType& object) override { return DataSize(0); }

  std::any Visit(const PrimitiveType& object) override { return DataSize(0); }

  std::any Visit(const InternalType& object) override {
    switch (object.subtype) {
      case InternalSubtype::kFrameworkErr:
        return DataSize(0);
    }
  }

  std::any Visit(const IdentifierType& object) override {
    if (object.type_decl->recursive) {
      return std::numeric_limits<DataSize>::max();
    }

    switch (object.nullability) {
      case Nullability::kNullable: {
        switch (object.type_decl->kind) {
          case Decl::Kind::kProtocol:
          case Decl::Kind::kService:
            return DataSize(0);
          case Decl::Kind::kStruct:
            return ObjectAlign(UnalignedSize(object.type_decl, wire_format())) +
                   MaxOutOfLine(object.type_decl);
          case Decl::Kind::kUnion:
            return MaxOutOfLine(object.type_decl);
          case Decl::Kind::kBits:
          case Decl::Kind::kBuiltin:
          case Decl::Kind::kConst:
          case Decl::Kind::kEnum:
          case Decl::Kind::kNewType:
          case Decl::Kind::kResource:
          case Decl::Kind::kTable:
          case Decl::Kind::kAlias:
          case Decl::Kind::kOverlay:
            ZX_PANIC("MaxOutOfLine(IdentifierType&) called on invalid nullable kind");
        }
      }
      case Nullability::kNonnullable:
        return MaxOutOfLine(object.type_decl);
    }
  }

  std::any Visit(const NewType& object) override { return MaxOutOfLine(object.type_ctor->type); }

  std::any Visit(const BoxType& object) override { return MaxOutOfLine(object.boxed_type); }

  std::any Visit(const TransportSideType& object) override { return DataSize(0); }

  std::any Visit(const Enum& object) override { return MaxOutOfLine(object.subtype_ctor->type); }

  std::any Visit(const Bits& object) override { return MaxOutOfLine(object.subtype_ctor->type); }

  std::any Visit(const Service& object) override { return DataSize(0); }

  std::any Visit(const Struct& object) override {
    DataSize max_out_of_line = 0;

    for (const auto& member : object.members) {
      max_out_of_line += MaxOutOfLine(member);
    }

    return max_out_of_line;
  }

  std::any Visit(const Struct::Member& object) override {
    return MaxOutOfLine(object.type_ctor->type);
  }

  std::any Visit(const Overlay& object) override {
    DataSize max_out_of_line = 0;

    for (const auto& member : object.members) {
      max_out_of_line = std::max(max_out_of_line, MaxOutOfLine(member));
    }

    return max_out_of_line;
  }

  std::any Visit(const Overlay::Member& object) override {
    return object.maybe_used ? MaxOutOfLine(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Overlay::Member::Used& object) override {
    return MaxOutOfLine(object.type_ctor->type);
  }

  std::any Visit(const Table& object) override {
    DataSize max_out_of_line = 0;

    for (const auto& member : object.members) {
      if (wire_format() == WireFormat::kV2) {
        if (UnalignedSize(member, wire_format()) <= 4) {
          continue;
        }
      }
      max_out_of_line += ObjectAlign(UnalignedSize(member, wire_format())) + MaxOutOfLine(member);
    }

    // The maximum number of envelopes is determined by the maximum _unreserved_ ordinal.
    // Any trailing reserved ordinals MUST NOT be present in the array of envelopes.
    // For example, a table that looks like
    // "table T { 1: int32 i; 2: reserved; 3: uint32 u; 4: reserved; }"
    // has an envelope array size of 3, not 4.
    ZX_ASSERT(object.members.size() <= INT32_MAX);
    int max_unreserved_index = -1;
    for (int i = static_cast<int>(object.members.size()) - 1; i >= 0; i--) {
      if (object.members.at(i).maybe_used) {
        max_unreserved_index = i;
        break;
      }
    }

    const size_t envelope_array_size = max_unreserved_index == -1 ? 0 : max_unreserved_index + 1;

    DataSize envelope_size = 0;
    switch (wire_format()) {
      case WireFormat::kV2:
        envelope_size = 8;
        break;
    }
    return DataSize(envelope_array_size) * envelope_size + max_out_of_line;
  }

  std::any Visit(const Table::Member& object) override {
    return object.maybe_used ? MaxOutOfLine(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Table::Member::Used& object) override {
    return ObjectAlign(MaxOutOfLine(object.type_ctor->type));
  }

  std::any Visit(const Union& object) override {
    DataSize max_out_of_line = 0;

    for (const auto& member : object.members) {
      if (wire_format() == WireFormat::kV2) {
        if (UnalignedSize(member, wire_format()) <= 4) {
          continue;
        }
      }
      max_out_of_line =
          std::max(max_out_of_line,
                   ObjectAlign(UnalignedSize(member, wire_format())) + MaxOutOfLine(member));
    }

    return max_out_of_line;
  }

  std::any Visit(const Union::Member& object) override {
    return object.maybe_used ? MaxOutOfLine(*object.maybe_used) : DataSize(0);
  }

  std::any Visit(const Union::Member::Used& object) override {
    return MaxOutOfLine(object.type_ctor->type);
  }

  std::any Visit(const Protocol& object) override { return DataSize(0); }

  std::any Visit(const ZxExperimentalPointerType& object) override {
    return ObjectAlign(MaxOutOfLine(object.pointee_type));
  }

 private:
  DataSize MaxOutOfLine(const Object& object) { return object.Accept(this); }

  DataSize MaxOutOfLine(const Object* object) { return MaxOutOfLine(*object); }
};

class HasPaddingVisitor final : public TypeShapeVisitor<bool> {
 public:
  using TypeShapeVisitor<bool>::TypeShapeVisitor;

  std::any Visit(const ArrayType& object) override { return HasPadding(object.element_type); }

  std::any Visit(const VectorType& object) override {
    auto element_has_innate_padding = [&] { return HasPadding(object.element_type); };

    auto element_has_trailing_padding = [&] {
      // A vector will always have padding out-of-line for its contents unless its element_type's
      // natural size is a multiple of 8.
      return Padding(UnalignedSize(object.element_type, wire_format()), 8) != 0;
    };

    return element_has_trailing_padding() || element_has_innate_padding();
  }

  std::any Visit(const StringType& object) override { return true; }

  std::any Visit(const HandleType& object) override { return false; }

  std::any Visit(const PrimitiveType& object) override { return false; }

  std::any Visit(const InternalType& object) override {
    switch (object.subtype) {
      case InternalSubtype::kFrameworkErr:
        return false;
    }
  }

  std::any Visit(const IdentifierType& object) override {
    thread_local RecursionDetector recursion_detector;

    auto guard = recursion_detector.Enter(&object);
    if (!guard) {
      return false;
    }

    switch (object.nullability) {
      case Nullability::kNullable:
        switch (object.type_decl->kind) {
          case Decl::Kind::kProtocol:
          case Decl::Kind::kService:
            return false;
          // TODO(https://fxbug.dev/70186): this should be handled as a box and nullable structs
          // should never be visited
          case Decl::Kind::kStruct:
          case Decl::Kind::kUnion:
            return Padding(UnalignedSize(object.type_decl, wire_format()), 8) > 0 ||
                   HasPadding(object.type_decl);
          case Decl::Kind::kBits:
          case Decl::Kind::kBuiltin:
          case Decl::Kind::kConst:
          case Decl::Kind::kEnum:
          case Decl::Kind::kNewType:
          case Decl::Kind::kResource:
          case Decl::Kind::kTable:
          case Decl::Kind::kAlias:
          case Decl::Kind::kOverlay:
            ZX_PANIC("HasPadding(IdentifierType&) called on invalid nullable kind");
        }
      case Nullability::kNonnullable:
        return HasPadding(object.type_decl);
    }
  }

  std::any Visit(const NewType& object) override { return HasPadding(object.type_ctor->type); }

  std::any Visit(const BoxType& object) override { return HasPadding(object.boxed_type); }

  std::any Visit(const TransportSideType& object) override { return false; }

  std::any Visit(const Enum& object) override { return HasPadding(object.subtype_ctor->type); }

  std::any Visit(const Bits& object) override { return HasPadding(object.subtype_ctor->type); }

  std::any Visit(const Service& object) override { return false; }

  std::any Visit(const Struct& object) override {
    for (const auto& member : object.members) {
      if (HasPadding(member)) {
        return true;
      }
    }

    return false;
  }

  std::any Visit(const Struct::Member& object) override {
    return object.fieldshape(wire_format()).padding > 0 || HasPadding(object.type_ctor->type);
  }

  std::any Visit(const Overlay& object) override {
    for (const auto& member : object.members) {
      if (HasPadding(member)) {
        return true;
      }
    }

    return false;
  }

  std::any Visit(const Overlay::Member& object) override {
    return object.maybe_used ? HasPadding(*object.maybe_used) : false;
  }

  std::any Visit(const Overlay::Member::Used& object) override {
    return object.fieldshape(wire_format()).padding > 0 || HasPadding(object.type_ctor->type);
  }

  std::any Visit(const Table& object) override {
    for (const auto& member : object.members) {
      if (HasPadding(member)) {
        return true;
      }
    }

    return false;
  }

  std::any Visit(const Table::Member& object) override {
    return object.maybe_used ? HasPadding(*object.maybe_used) : false;
  }

  std::any Visit(const Table::Member::Used& object) override {
    return Padding(UnalignedSize(object.type_ctor->type, wire_format()), 8) > 0 ||
           HasPadding(object.type_ctor->type) || object.fieldshape(wire_format()).padding > 0;
  }

  std::any Visit(const Union& object) override {
    // TODO(https://fxbug.dev/36332): Unions currently return true for has_padding in all cases,
    // which should be fixed.
    return true;
  }

  std::any Visit(const Union::Member& object) override {
    return object.maybe_used ? HasPadding(*object.maybe_used) : false;
  }

  std::any Visit(const Union::Member::Used& object) override {
    // TODO(https://fxbug.dev/36331): This code only accounts for inline padding for the union
    // member. We also need to account for out-of-line padding.
    return object.fieldshape(wire_format()).padding > 0;
  }

  std::any Visit(const Protocol& object) override { return false; }

  std::any Visit(const ZxExperimentalPointerType& object) override { return false; }

 private:
  bool HasPadding(const Object& object) { return object.Accept(this); }

  bool HasPadding(const Object* object) { return HasPadding(*object); }
};

class HasFlexibleEnvelopeVisitor final : public TypeShapeVisitor<bool> {
 public:
  using TypeShapeVisitor<bool>::TypeShapeVisitor;

  std::any Visit(const ArrayType& object) override {
    return HasFlexibleEnvelope(object.element_type, wire_format());
  }

  std::any Visit(const VectorType& object) override {
    return HasFlexibleEnvelope(object.element_type, wire_format());
  }

  std::any Visit(const StringType& object) override { return false; }

  std::any Visit(const HandleType& object) override { return false; }

  std::any Visit(const PrimitiveType& object) override { return false; }

  std::any Visit(const InternalType& object) override {
    switch (object.subtype) {
      case InternalSubtype::kFrameworkErr:
        return false;
    }
  }

  std::any Visit(const IdentifierType& object) override {
    thread_local RecursionDetector recursion_detector;

    auto guard = recursion_detector.Enter(&object);
    if (!guard) {
      return false;
    }

    return HasFlexibleEnvelope(object.type_decl, wire_format());
  }

  std::any Visit(const NewType& object) override {
    return HasFlexibleEnvelope(object.type_ctor->type, wire_format());
  }

  std::any Visit(const BoxType& object) override {
    return HasFlexibleEnvelope(object.boxed_type, wire_format());
  }

  std::any Visit(const TransportSideType& object) override { return false; }

  std::any Visit(const Enum& object) override {
    return HasFlexibleEnvelope(object.subtype_ctor->type, wire_format());
  }

  std::any Visit(const Bits& object) override {
    return HasFlexibleEnvelope(object.subtype_ctor->type, wire_format());
  }

  std::any Visit(const Service& object) override { return false; }

  std::any Visit(const Struct& object) override {
    for (const auto& member : object.members) {
      if (HasFlexibleEnvelope(member, wire_format())) {
        return true;
      }
    }

    return false;
  }

  std::any Visit(const Struct::Member& object) override {
    return HasFlexibleEnvelope(object.type_ctor->type, wire_format());
  }

  std::any Visit(const Overlay& object) override {
    for (const auto& member : object.members) {
      if (HasFlexibleEnvelope(member, wire_format())) {
        return true;
      }
    }

    return false;
  }

  std::any Visit(const Overlay::Member& object) override {
    return object.maybe_used
               ? HasFlexibleEnvelope(object.maybe_used->type_ctor->type, wire_format())
               : false;
  }

  std::any Visit(const Overlay::Member::Used& object) override {
    return HasFlexibleEnvelope(object.type_ctor->type, wire_format());
  }

  std::any Visit(const Table& object) override {
    if (object.strictness == Strictness::kFlexible) {
      return true;
    }

    for (const auto& member : object.members) {
      if (HasFlexibleEnvelope(member, wire_format())) {
        return true;
      }
    }

    return false;
  }

  std::any Visit(const Table::Member& object) override {
    return object.maybe_used ? HasFlexibleEnvelope(*object.maybe_used, wire_format()) : false;
  }

  std::any Visit(const Table::Member::Used& object) override {
    return HasFlexibleEnvelope(object.type_ctor->type, wire_format());
  }

  std::any Visit(const Union& object) override {
    if (object.strictness == Strictness::kFlexible) {
      return true;
    }

    for (const auto& member : object.members) {
      if (HasFlexibleEnvelope(member, wire_format())) {
        return true;
      }
    }

    return false;
  }

  std::any Visit(const Union::Member& object) override {
    return object.maybe_used ? HasFlexibleEnvelope(*object.maybe_used, wire_format()) : false;
  }

  std::any Visit(const Union::Member::Used& object) override {
    return HasFlexibleEnvelope(object.type_ctor->type, wire_format());
  }

  std::any Visit(const Protocol& object) override { return false; }

  std::any Visit(const ZxExperimentalPointerType& object) override { return false; }
};

DataSize UnalignedSize(const Object& object, WireFormat wire_format) {
  UnalignedSizeVisitor v(wire_format);
  return object.Accept(&v);
}

[[maybe_unused]] DataSize UnalignedSize(const Object* object, WireFormat wire_format) {
  return UnalignedSize(*object, wire_format);
}

DataSize Alignment(const Object& object, WireFormat wire_format) {
  AlignmentVisitor v(wire_format);
  return object.Accept(&v);
}

[[maybe_unused]] DataSize Alignment(const Object* object, WireFormat wire_format) {
  return Alignment(*object, wire_format);
}

DataSize Depth(const Object& object, WireFormat wire_format) {
  DepthVisitor v(wire_format);
  return object.Accept(&v);
}

[[maybe_unused]] DataSize Depth(const Object* object, WireFormat wire_format) {
  return Depth(*object, wire_format);
}

DataSize MaxHandles(const Object& object) {
  MaxHandlesVisitor v;
  return object.Accept(&v);
}

[[maybe_unused]] DataSize MaxHandles(const Object* object) { return MaxHandles(*object); }

DataSize MaxOutOfLine(const Object& object, WireFormat wire_format) {
  MaxOutOfLineVisitor v(wire_format);
  return object.Accept(&v);
}

[[maybe_unused]] DataSize MaxOutOfLine(const Object* object, WireFormat wire_format) {
  return MaxOutOfLine(*object, wire_format);
}

bool HasPadding(const Object& object, WireFormat wire_format) {
  HasPaddingVisitor v(wire_format);
  return object.Accept(&v);
}

[[maybe_unused]] bool HasPadding(const Object* object, WireFormat wire_format) {
  return HasPadding(*object, wire_format);
}

bool HasFlexibleEnvelope(const Object& object, WireFormat wire_format) {
  HasFlexibleEnvelopeVisitor v(wire_format);
  return object.Accept(&v);
}

[[maybe_unused]] bool HasFlexibleEnvelope(const Object* object, WireFormat wire_format) {
  return HasFlexibleEnvelope(*object, wire_format);
}

}  // namespace

TypeShape::TypeShape(const Object& object, WireFormat wire_format)
    : inline_size(AlignedSize(object, wire_format)),
      alignment(Alignment(object, wire_format)),
      depth(Depth(object, wire_format)),
      max_handles(MaxHandles(object)),
      max_out_of_line(MaxOutOfLine(object, wire_format)),
      has_padding(HasPadding(object, wire_format)),
      has_flexible_envelope(HasFlexibleEnvelope(object, wire_format)) {}

TypeShape::TypeShape(const Object* object, WireFormat wire_format)
    : TypeShape(*object, wire_format) {}

TypeShape TypeShape::ForEmptyPayload() { return TypeShape(0, 0); }

FieldShape::FieldShape(const StructMember& member, WireFormat wire_format) {
  ZX_ASSERT(member.parent);
  const Struct& parent = *member.parent;

  // Our parent struct must have at least one member if fieldshape() on a member is being
  // called.
  ZX_ASSERT(!parent.members.empty());
  const std::vector<StructMember>& members = parent.members;

  for (size_t i = 0; i < members.size(); i++) {
    const StructMember* it = &members.at(i);

    DataSize alignment;
    if (i + 1 < members.size()) {
      const auto& next = members.at(i + 1);
      alignment = Alignment(next, wire_format);
    } else {
      alignment = Alignment(parent, wire_format);
    }

    uint32_t size = UnalignedSize(*it, wire_format);

    padding = Padding(offset + size, alignment);

    if (it == &member)
      break;

    offset += size + padding;
  }
}

FieldShape::FieldShape(const TableMemberUsed& member, WireFormat wire_format)
    : padding(Padding(UnalignedSize(member, wire_format), 8)) {}

FieldShape::FieldShape(const UnionMemberUsed& member, WireFormat wire_format)
    : padding(Padding(UnalignedSize(member, wire_format), Alignment(member.parent, wire_format))) {}

FieldShape::FieldShape(const OverlayMemberUsed& member, WireFormat wire_format) {
  ZX_ASSERT(member.parent);
  const Overlay& parent = *member.parent;

  // Our parent overlay must have at least one member if fieldshape() on a member is being
  // called.
  ZX_ASSERT(!parent.members.empty());
  const std::vector<OverlayMember>& members = parent.members;

  // After the ordinal.
  offset = 8;

  // Alignment is dictated by the ordinal.
  DataSize alignment = 8;

  DataSize max_member_size = 0;
  for (const auto& member : members) {
    if (member.maybe_used) {
      max_member_size =
          std::max(max_member_size, UnalignedSize(member.maybe_used->type_ctor->type, wire_format));
    }
  }
  padding = AlignTo(max_member_size, alignment) - UnalignedSize(member, wire_format);
}

}  // namespace fidlc
