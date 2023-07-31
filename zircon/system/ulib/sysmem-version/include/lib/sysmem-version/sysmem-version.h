// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYSMEM_VERSION_SYSMEM_VERSION_H_
#define LIB_SYSMEM_VERSION_SYSMEM_VERSION_H_

#ifndef _LIBCPP_ENABLE_HARDENED_MODE
#define _LIBCPP_ENABLE_HARDENED_MODE 1
#endif

#include <fidl/fuchsia.images2/cpp/fidl.h>
#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/fidl/cpp/wire/traits.h>
#include <lib/fpromise/result.h>

#include <type_traits>

// In sysmem V1, there's a PixelFormat FIDL struct that includes both pixel_format and
// pixel_format_modifier, used as a self-contained structure / sub-structure in various places in
// FIDL and C++ code. While that's sensible from a data organization point of view and handy for
// the sysmem server, the sub-structure means more typing than necessary for a typical sysmem usage.
//
// In V2 we instead just have two separate fields in each parent table. We use this C++ struct in
// the C++ code to avoid needing to plumb two values everywhere in the sysmem server. This is also
// used as the V2 analog of fuchsia_sysmem::PixelFormat (V1) when converting between V1 and V2.
struct PixelFormatAndModifier {
  // Inits fields to zero when default-constructed.
  PixelFormatAndModifier()
      : pixel_format(fuchsia_images2::PixelFormat::kInvalid),
        pixel_format_modifier(fuchsia_images2::kFormatModifierNone) {
    static_assert(fuchsia_images2::kFormatModifierLinear == fuchsia_images2::kFormatModifierNone);
  }
  PixelFormatAndModifier(fuchsia_images2::PixelFormat pixel_format_param,
                         uint64_t pixel_format_modifier_param)
      : pixel_format(pixel_format_param), pixel_format_modifier(pixel_format_modifier_param) {}

  fuchsia_images2::PixelFormat pixel_format;
  uint64_t pixel_format_modifier;
};

inline PixelFormatAndModifier PixelFormatAndModifierFromConstraints(
    const fuchsia_sysmem2::ImageFormatConstraints& constraints) {
  ZX_ASSERT(constraints.pixel_format().has_value());
  static_assert(fuchsia_images2::kFormatModifierLinear == fuchsia_images2::kFormatModifierNone);
  uint64_t pixel_format_modifier = constraints.pixel_format_modifier().has_value()
                                       ? *constraints.pixel_format_modifier()
                                       : fuchsia_images2::kFormatModifierNone;
  return PixelFormatAndModifier(*constraints.pixel_format(), pixel_format_modifier);
}

inline PixelFormatAndModifier PixelFormatAndModifierFromImageFormat(
    const fuchsia_images2::ImageFormat& image_format) {
  ZX_ASSERT(image_format.pixel_format().has_value());
  static_assert(fuchsia_images2::kFormatModifierLinear == fuchsia_images2::kFormatModifierNone);
  uint64_t pixel_format_modifier = image_format.pixel_format_modifier().has_value()
                                       ? *image_format.pixel_format_modifier()
                                       : fuchsia_images2::kFormatModifierNone;
  return PixelFormatAndModifier(*image_format.pixel_format(), pixel_format_modifier);
}

inline PixelFormatAndModifier PixelFormatAndModifierFromImageFormat(
    const fuchsia_images2::wire::ImageFormat& image_format) {
  ZX_ASSERT(image_format.has_pixel_format());
  static_assert(fuchsia_images2::wire::kFormatModifierLinear ==
                fuchsia_images2::wire::kFormatModifierNone);
  uint64_t pixel_format_modifier = image_format.has_pixel_format_modifier()
                                       ? image_format.pixel_format_modifier()
                                       : fuchsia_images2::wire::kFormatModifierNone;
  return PixelFormatAndModifier(image_format.pixel_format(), pixel_format_modifier);
}

namespace sysmem {

namespace internal {

// Can be replaced with std::type_identity<> when C++20.
template <typename T>
struct TypeIdentity {
  using type = T;
};
template <typename T>
using TypeIdentity_t = typename TypeIdentity<T>::type;

template <typename T, typename Enable = void>
struct HasOperatorUInt32 : std::false_type {};
template <typename T>
struct HasOperatorUInt32<
    T,
    std::enable_if_t<std::is_same_v<uint32_t, decltype((std::declval<T>().operator uint32_t()))>>>
    : std::true_type {};

static_assert(!HasOperatorUInt32<fuchsia_sysmem::PixelFormatType>::value);
static_assert(HasOperatorUInt32<fuchsia_images2::PixelFormat>::value);
static_assert(!HasOperatorUInt32<fuchsia_sysmem::HeapType>::value);
static_assert(!HasOperatorUInt32<fuchsia_sysmem2::HeapType>::value);

template <typename T, typename Enable = void>
struct HasOperatorUInt64 : std::false_type {};
template <typename T>
struct HasOperatorUInt64<
    T,
    std::enable_if_t<std::is_same_v<uint64_t, decltype((std::declval<T>().operator uint64_t()))>>>
    : std::true_type {};

static_assert(!HasOperatorUInt64<fuchsia_sysmem::PixelFormatType>::value);
static_assert(!HasOperatorUInt64<fuchsia_images2::PixelFormat>::value);
static_assert(!HasOperatorUInt64<fuchsia_sysmem::HeapType>::value);
static_assert(HasOperatorUInt64<fuchsia_sysmem2::HeapType>::value);

// The meaning of "fidl enum" here includes flexible enums, which are actually just final classes
// with a single private scalar field after codegen, but the have an operator uint32_t() or
// operator uint64_t() (the ones we care about here) so we detect that way (at least for now).
template <typename T, typename Enable = void>
struct IsFidlEnum : std::false_type {};
template <typename T>
struct IsFidlEnum<
    T, typename std::enable_if<fidl::IsFidlType<T>::value && std::is_enum<T>::value>::type>
    : std::true_type {};
template <typename T>
struct IsFidlEnum<T, typename std::enable_if<fidl::IsFidlType<T>::value &&
                                             (internal::HasOperatorUInt32<T>::value ||
                                              internal::HasOperatorUInt64<T>::value)>::type>
    : std::true_type {};

enum TestEnum {
  kTestEnumZero,
  kTestEnumOne,
};
static_assert(!IsFidlEnum<TestEnum>::value);
static_assert(IsFidlEnum<fuchsia_sysmem::ColorSpaceType>::value);
static_assert(IsFidlEnum<fuchsia_images2::ColorSpace>::value);
static_assert(!IsFidlEnum<uint32_t>::value);
static_assert(!IsFidlEnum<uint64_t>::value);

// FidlUnderlyingTypeOrType<T>::type gets std::underlying_type<T>::type if T is a FIDL enum, or T
// otherwise.  The notion "is an enum" in this context includes FIDL flexible enums despite not
// being C++ enums after LLCPP FIDL codegen.  For such LLCPP FIDL flexible enums this returns the
// type which they implicitly convert to/from.
template <typename T, typename Enable = void>
struct FidlUnderlyingTypeOrType : TypeIdentity<T> {};
template <typename T>
struct FidlUnderlyingTypeOrType<
    T, typename std::enable_if<IsFidlEnum<T>::value && std::is_enum<T>::value>::type>
    : std::underlying_type<T> {};
template <typename T>
struct FidlUnderlyingTypeOrType<
    T, typename std::enable_if<IsFidlEnum<T>::value && HasOperatorUInt32<T>::value>::type> {
  using type = uint32_t;
};
template <typename T>
struct FidlUnderlyingTypeOrType<
    T, typename std::enable_if<IsFidlEnum<T>::value && HasOperatorUInt64<T>::value>::type> {
  using type = uint64_t;
};

static_assert(
    std::is_same<uint32_t, FidlUnderlyingTypeOrType<fuchsia_sysmem::PixelFormatType>::type>::value);
static_assert(
    std::is_same<uint64_t, FidlUnderlyingTypeOrType<fuchsia_sysmem::HeapType>::type>::value);
static_assert(
    std::is_same<uint32_t, FidlUnderlyingTypeOrType<fuchsia_images2::PixelFormat>::type>::value);
static_assert(
    std::is_same<uint64_t, FidlUnderlyingTypeOrType<fuchsia_sysmem2::HeapType>::type>::value);

}  // namespace internal

template <typename T>
constexpr bool IsFidlEnum_v = internal::IsFidlEnum<T>::value;

template <typename T>
using FidlUnderlyingTypeOrType_t = typename internal::FidlUnderlyingTypeOrType<T>::type;

template <typename T>
constexpr FidlUnderlyingTypeOrType_t<T> fidl_underlying_cast(const T& value) {
  return static_cast<FidlUnderlyingTypeOrType_t<T>>(value);
}

static_assert(2 == fidl_underlying_cast(static_cast<fuchsia_sysmem2::HeapType>(2)));

///////////////////////
// V2 Copy/Move from V1
///////////////////////

// We provide copy when the v1 Layout=Simple struct has MaxNumHandles == 0.
// We provide move when the v1 Layout=Simple struct has MaxNumHandles != 0.

// When we provide move, we only provide move from llcpp, not from FIDL C.
//
// See fidl_struct.h's TakeAsLlcpp() for a way to convert from FIDL C to llcpp first.

[[nodiscard]] fuchsia_sysmem2::wire::HeapType V2CopyFromV1HeapType(
    fuchsia_sysmem::wire::HeapType heap_type);

// For cases that also need to convey pixel_format_modifier, see
// V2CopyFromV1PixelFormat. The implied modifier when not provided or not set
// is FORMAT_MODIFIER_NONE (aka LINEAR).
[[nodiscard]] fuchsia_images2::PixelFormat V2CopyFromV1PixelFormatType(
    const fuchsia_sysmem::PixelFormatType& v1);

[[nodiscard]] PixelFormatAndModifier V2CopyFromV1PixelFormat(const fuchsia_sysmem::PixelFormat& v1);
[[nodiscard]] PixelFormatAndModifier V2CopyFromV1PixelFormat(
    const fuchsia_sysmem::wire::PixelFormat& v1);

[[nodiscard]] uint64_t V2ConvertFromV1PixelFormatModifier(uint64_t v1_pixel_format_modifier);

[[nodiscard]] fuchsia_images2::ColorSpace V2CopyFromV1ColorSpace(
    const fuchsia_sysmem::ColorSpace& v1);
[[nodiscard]] fuchsia_images2::wire::ColorSpace V2CopyFromV1ColorSpace(
    const fuchsia_sysmem::wire::ColorSpace& v1);

[[nodiscard]] fpromise::result<fuchsia_sysmem2::ImageFormatConstraints>
V2CopyFromV1ImageFormatConstraints(const fuchsia_sysmem::ImageFormatConstraints& v1);
[[nodiscard]] fpromise::result<fuchsia_sysmem2::wire::ImageFormatConstraints>
V2CopyFromV1ImageFormatConstraints(fidl::AnyArena& allocator,
                                   const fuchsia_sysmem::wire::ImageFormatConstraints& v1);

[[nodiscard]] fpromise::result<fuchsia_sysmem2::BufferUsage> V2CopyFromV1BufferUsage(
    const fuchsia_sysmem::BufferUsage& v1);
[[nodiscard]] fpromise::result<fuchsia_sysmem2::wire::BufferUsage> V2CopyFromV1BufferUsage(
    fidl::AnyArena& allocator, const fuchsia_sysmem::wire::BufferUsage& v1);

[[nodiscard]] fpromise::result<fuchsia_sysmem2::BufferMemoryConstraints>
V2CopyFromV1BufferMemoryConstraints(const fuchsia_sysmem::BufferMemoryConstraints& v1);
[[nodiscard]] fpromise::result<fuchsia_sysmem2::wire::BufferMemoryConstraints>
V2CopyFromV1BufferMemoryConstraints(fidl::AnyArena& allocator,
                                    const fuchsia_sysmem::wire::BufferMemoryConstraints& v1);

[[nodiscard]] fpromise::result<fuchsia_sysmem2::BufferCollectionConstraints>
V2CopyFromV1BufferCollectionConstraints(
    const fuchsia_sysmem::BufferCollectionConstraints* v1,
    const fuchsia_sysmem::BufferCollectionConstraintsAuxBuffers* aux_buffers_v1);
[[nodiscard]] fpromise::result<fuchsia_sysmem2::wire::BufferCollectionConstraints>
V2CopyFromV1BufferCollectionConstraints(
    fidl::AnyArena& allocator, const fuchsia_sysmem::wire::BufferCollectionConstraints* v1,
    const fuchsia_sysmem::wire::BufferCollectionConstraintsAuxBuffers* aux_buffers_v1);

[[nodiscard]] fpromise::result<fuchsia_images2::ImageFormat> V2CopyFromV1ImageFormat(
    const fuchsia_sysmem::ImageFormat2& v1);
[[nodiscard]] fpromise::result<fuchsia_images2::wire::ImageFormat> V2CopyFromV1ImageFormat(
    fidl::AnyArena& allocator, const fuchsia_sysmem::wire::ImageFormat2& v1);

[[nodiscard]] fuchsia_sysmem2::BufferMemorySettings V2CopyFromV1BufferMemorySettings(
    const fuchsia_sysmem::BufferMemorySettings& v1);
[[nodiscard]] fuchsia_sysmem2::wire::BufferMemorySettings V2CopyFromV1BufferMemorySettings(
    fidl::AnyArena& allocator, const fuchsia_sysmem::wire::BufferMemorySettings& v1);

[[nodiscard]] fpromise::result<fuchsia_sysmem2::SingleBufferSettings>
V2CopyFromV1SingleBufferSettings(const fuchsia_sysmem::SingleBufferSettings& v1);
[[nodiscard]] fpromise::result<fuchsia_sysmem2::wire::SingleBufferSettings>
V2CopyFromV1SingleBufferSettings(fidl::AnyArena& allocator,
                                 const fuchsia_sysmem::wire::SingleBufferSettings& v1);

[[nodiscard]] fuchsia_sysmem2::VmoBuffer V2MoveFromV1VmoBuffer(fuchsia_sysmem::VmoBuffer v1);
[[nodiscard]] fuchsia_sysmem2::wire::VmoBuffer V2MoveFromV1VmoBuffer(
    fidl::AnyArena& allocator, fuchsia_sysmem::wire::VmoBuffer v1);

[[nodiscard]] fpromise::result<fuchsia_sysmem2::BufferCollectionInfo>
V2MoveFromV1BufferCollectionInfo(fuchsia_sysmem::BufferCollectionInfo2 v1);
[[nodiscard]] fpromise::result<fuchsia_sysmem2::wire::BufferCollectionInfo>
V2MoveFromV1BufferCollectionInfo(fidl::AnyArena& allocator,
                                 fuchsia_sysmem::wire::BufferCollectionInfo2 v1);

///////////////////////
// V1 Copy/Move from V2
///////////////////////

[[nodiscard]] fuchsia_sysmem::wire::HeapType V1CopyFromV2HeapType(
    fuchsia_sysmem2::wire::HeapType heap_type);
[[nodiscard]] fpromise::result<
    std::pair<std::optional<fuchsia_sysmem::BufferCollectionConstraints>,
              std::optional<fuchsia_sysmem::BufferCollectionConstraintsAuxBuffers>>>
V1CopyFromV2BufferCollectionConstraints(const fuchsia_sysmem2::BufferCollectionConstraints& v2);
[[nodiscard]] fpromise::result<
    std::pair<std::optional<fuchsia_sysmem::wire::BufferCollectionConstraints>,
              std::optional<fuchsia_sysmem::wire::BufferCollectionConstraintsAuxBuffers>>>
V1CopyFromV2BufferCollectionConstraints(
    const fuchsia_sysmem2::wire::BufferCollectionConstraints& v2);

[[nodiscard]] fpromise::result<fuchsia_sysmem::BufferMemoryConstraints>
V1CopyFromV2BufferMemoryConstraints(const fuchsia_sysmem2::BufferMemoryConstraints& v2);
[[nodiscard]] fpromise::result<fuchsia_sysmem::wire::BufferMemoryConstraints>
V1CopyFromV2BufferMemoryConstraints(const fuchsia_sysmem2::wire::BufferMemoryConstraints& v2);

[[nodiscard]] fuchsia_sysmem::BufferUsage V1CopyFromV2BufferUsage(
    const fuchsia_sysmem2::BufferUsage& v2);
[[nodiscard]] fuchsia_sysmem::wire::BufferUsage V1CopyFromV2BufferUsage(
    const fuchsia_sysmem2::wire::BufferUsage& v2);

[[nodiscard]] fuchsia_sysmem::BufferMemorySettings V1CopyFromV2BufferMemorySettings(
    const fuchsia_sysmem2::BufferMemorySettings& v2);
[[nodiscard]] fuchsia_sysmem::wire::BufferMemorySettings V1CopyFromV2BufferMemorySettings(
    const fuchsia_sysmem2::wire::BufferMemorySettings& v2);

[[nodiscard]] fuchsia_sysmem::PixelFormat V1CopyFromV2PixelFormat(const PixelFormatAndModifier& v2);
[[nodiscard]] fuchsia_sysmem::wire::PixelFormat V1WireCopyFromV2PixelFormat(
    const PixelFormatAndModifier& v2);

// For cases that also need to convey pixel_format_modifier, see
// V2CopyFromV1PixelFormat. The implied modifier when not provided or not set
// is FORMAT_MODIFIER_NONE (aka LINEAR).
[[nodiscard]] fuchsia_sysmem::PixelFormatType V1CopyFromV2PixelFormatType(
    const fuchsia_images2::PixelFormat& v2);

[[nodiscard]] uint64_t V1ConvertFromV2PixelFormatModifier(uint64_t v2_pixel_format_modifier);

[[nodiscard]] fuchsia_sysmem::ColorSpace V1CopyFromV2ColorSpace(
    const fuchsia_images2::ColorSpace& v2);
[[nodiscard]] fuchsia_sysmem::wire::ColorSpace V1WireCopyFromV2ColorSpace(
    const fuchsia_images2::wire::ColorSpace& v2);

[[nodiscard]] fpromise::result<fuchsia_sysmem::ImageFormatConstraints>
V1CopyFromV2ImageFormatConstraints(const fuchsia_sysmem2::ImageFormatConstraints& v2);
[[nodiscard]] fpromise::result<fuchsia_sysmem::wire::ImageFormatConstraints>
V1CopyFromV2ImageFormatConstraints(const fuchsia_sysmem2::wire::ImageFormatConstraints& v2);

[[nodiscard]] fpromise::result<fuchsia_sysmem::ImageFormat2> V1CopyFromV2ImageFormat(
    fuchsia_images2::ImageFormat& v2);
[[nodiscard]] fpromise::result<fuchsia_sysmem::wire::ImageFormat2> V1CopyFromV2ImageFormat(
    fuchsia_images2::wire::ImageFormat& v2);

[[nodiscard]] fpromise::result<fuchsia_sysmem::SingleBufferSettings>
V1CopyFromV2SingleBufferSettings(const fuchsia_sysmem2::SingleBufferSettings& v2);
[[nodiscard]] fpromise::result<fuchsia_sysmem::wire::SingleBufferSettings>
V1CopyFromV2SingleBufferSettings(const fuchsia_sysmem2::wire::SingleBufferSettings& v2);

[[nodiscard]] fuchsia_sysmem::VmoBuffer V1MoveFromV2VmoBuffer(fuchsia_sysmem2::VmoBuffer v2);
[[nodiscard]] fuchsia_sysmem::wire::VmoBuffer V1MoveFromV2VmoBuffer(
    fuchsia_sysmem2::wire::VmoBuffer v2);

[[nodiscard]] fuchsia_sysmem::VmoBuffer V1AuxBuffersMoveFromV2VmoBuffer(
    fuchsia_sysmem2::VmoBuffer v2);
[[nodiscard]] fuchsia_sysmem::wire::VmoBuffer V1AuxBuffersMoveFromV2VmoBuffer(
    fuchsia_sysmem2::wire::VmoBuffer v2);

[[nodiscard]] fpromise::result<fuchsia_sysmem::BufferCollectionInfo2>
V1MoveFromV2BufferCollectionInfo(fuchsia_sysmem2::BufferCollectionInfo v2);
[[nodiscard]] fpromise::result<fuchsia_sysmem::wire::BufferCollectionInfo2>
V1MoveFromV2BufferCollectionInfo(fuchsia_sysmem2::wire::BufferCollectionInfo v2);

[[nodiscard]] fpromise::result<fuchsia_sysmem::BufferCollectionInfo2>
V1AuxBuffersMoveFromV2BufferCollectionInfo(fuchsia_sysmem2::BufferCollectionInfo v2);
[[nodiscard]] fpromise::result<fuchsia_sysmem::wire::BufferCollectionInfo2>
V1AuxBuffersMoveFromV2BufferCollectionInfo(fuchsia_sysmem2::wire::BufferCollectionInfo v2);

///////////
// V2 Clone
///////////

// For natural types, we only need an explicit clone if copy construction / assignment isn't
// provided by codegen, which is when IsResource<>.

[[nodiscard]] fuchsia_sysmem2::wire::BufferMemorySettings V2CloneBufferMemorySettings(
    fidl::AnyArena& allocator, const fuchsia_sysmem2::wire::BufferMemorySettings& src);
[[nodiscard]] fuchsia_sysmem2::wire::ImageFormatConstraints V2CloneImageFormatConstraints(
    fidl::AnyArena& allocator, const fuchsia_sysmem2::wire::ImageFormatConstraints& src);
[[nodiscard]] fuchsia_sysmem2::wire::SingleBufferSettings V2CloneSingleBufferSettings(
    fidl::AnyArena& allocator, const fuchsia_sysmem2::wire::SingleBufferSettings& src);

[[nodiscard]] fpromise::result<fuchsia_sysmem2::VmoBuffer, zx_status_t> V2CloneVmoBuffer(
    const fuchsia_sysmem2::VmoBuffer& src, uint32_t vmo_rights_mask, uint32_t aux_vmo_rights_mask);
[[nodiscard]] fpromise::result<fuchsia_sysmem2::wire::VmoBuffer, zx_status_t> V2CloneVmoBuffer(
    fidl::AnyArena& allocator, const fuchsia_sysmem2::wire::VmoBuffer& src,
    uint32_t vmo_rights_mask, uint32_t aux_vmo_rights_mask);

fpromise::result<fuchsia_sysmem2::BufferCollectionInfo, zx_status_t> V2CloneBufferCollectionInfo(
    const fuchsia_sysmem2::BufferCollectionInfo& src, uint32_t vmo_rights_mask,
    uint32_t aux_vmo_rights_mask);
[[nodiscard]] fpromise::result<fuchsia_sysmem2::wire::BufferCollectionInfo, zx_status_t>
V2CloneBufferCollectionInfo(fidl::AnyArena& allocator,
                            const fuchsia_sysmem2::wire::BufferCollectionInfo& src,
                            uint32_t vmo_rights_mask, uint32_t aux_vmo_rights_mask);

[[nodiscard]] fuchsia_sysmem2::wire::CoherencyDomainSupport V2CloneCoherencyDomainSuppoort(
    fidl::AnyArena& allocator, const fuchsia_sysmem2::wire::CoherencyDomainSupport& src);
[[nodiscard]] fuchsia_sysmem2::wire::HeapProperties V2CloneHeapProperties(
    fidl::AnyArena& allocator, const fuchsia_sysmem2::wire::HeapProperties& src);
[[nodiscard]] fuchsia_sysmem2::wire::BufferCollectionConstraints V2CloneBufferCollectionConstraints(
    fidl::AnyArena& allocator, const fuchsia_sysmem2::wire::BufferCollectionConstraints& src);
[[nodiscard]] fuchsia_sysmem2::wire::BufferUsage V2CloneBufferUsage(
    fidl::AnyArena& allocator, const fuchsia_sysmem2::wire::BufferUsage& src);
[[nodiscard]] fuchsia_sysmem2::wire::BufferMemoryConstraints V2CloneBufferMemoryConstraints(
    fidl::AnyArena& allocator, const fuchsia_sysmem2::wire::BufferMemoryConstraints& src);

}  // namespace sysmem

#endif  // LIB_SYSMEM_VERSION_SYSMEM_VERSION_H_
