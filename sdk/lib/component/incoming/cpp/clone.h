// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_COMPONENT_INCOMING_CPP_CLONE_H_
#define LIB_COMPONENT_INCOMING_CPP_CLONE_H_

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/component/incoming/cpp/internal.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>

#include <string_view>
#include <type_traits>
#include <utility>

namespace component {

// Passing this value to |component::Clone| implies opting out of any
// compile-time checks the the FIDL protocol supports |fuchsia.io/Node.Clone|.
// This option should be used with care. See documentation on |component::Clone|.
constexpr inline auto AssumeProtocolComposesNode =
    internal::AssumeProtocolComposesNodeTag::kAssumeProtocolComposesNode;

// Typed channel wrapper around |fuchsia.unknown/Cloneable.Clone| and |fuchsia.io/Node.Clone|.
//
// Given an unowned client end |client|, returns an owned clone as a new connection using protocol
// request pipelining.
//
// |client| must be a channel that supports at least one of the following protocols:
//   * |fuchsia.unknown/Cloneable|
//   * |fuchsia.io/Node|
//
// This function looks a little involved due to the template programming; here
// is an example how it could be used:
//
// ```
//   // |node| could be |fidl::ClientEnd| or |fidl::UnownedClientEnd|.
//   auto clone = component::Clone(node);
// ```
//
// By default, this function will verify that the protocol type supports cloning
// (i.e. satisfies the protocol requirement above). Under special circumstances,
// it is possible to explicitly state that the protocol actually composes
// |fuchsia.io/Node| at run-time, even though it may not be defined this way
// in the FIDL schema. This could happen as a result of implicit or unsupported
// multiplexing of FIDL protocols. There will not be any compile-time
// validation that the cloning is supported, if the extra
// |component::AssumeProtocolComposesNode| argument is provided. Note that if
// the channel does not implement |fuchsia.io/Node.Clone|, the remote endpoint
// of the cloned node will be asynchronously closed.
//
// As such, this override should be used sparingly, and with caution:
//
// ```
//   // Assume that |node| supports the |fuchsia.io/Node.Clone| method call.
//   // If that is not the case, there will be runtime failures at a later
//   // stage when |clone| is actually used.
//   auto clone = component::Clone(node, component::AssumeProtocolComposesNode);
// ```
//
// See documentation on |fuchsia.io/Node.Clone| for details.
template <typename Protocol, typename Tag = std::nullptr_t,
          typename = std::enable_if_t<
              std::disjunction_v<std::is_same<Tag, std::nullptr_t>,
                                 std::is_same<Tag, internal::AssumeProtocolComposesNodeTag>>>>
zx::result<fidl::ClientEnd<Protocol>> Clone(fidl::UnownedClientEnd<Protocol> client,
                                            Tag tag = nullptr) {
  static_assert(internal::is_complete_v<Protocol>,
                "|Protocol| must be fully defined in order to use |component::Clone|");
  static_assert(
      !(internal::has_fidl_method_fuchsia_unknown_clone_v<Protocol> &&
        internal::has_fidl_method_fuchsia_io_clone_v<Protocol>),
      "|Protocol| must not compose both |fuchsia.unknown/Cloneable| and |fuchsia.io/Node| when "
      "using |component::Clone|. Otherwise, the correct clone implementation to dispatch to is "
      "ambiguous.");

  constexpr bool kShouldAssumeProtocolComposesNode =
      std::is_same_v<Tag, internal::AssumeProtocolComposesNodeTag>;

  zx::result<zx::channel> result;
  if constexpr (internal::has_fidl_method_fuchsia_unknown_clone_v<Protocol>) {
    static_assert(!kShouldAssumeProtocolComposesNode,
                  "|Protocol| already appears to compose the |fuchsia.unknown/Cloneable| protocol. "
                  "There is no need to specify |AssumeProtocolComposesNode|.");
    result =
        internal::CloneRaw(fidl::UnownedClientEnd<fuchsia_unknown::Cloneable>(client.channel()));
  } else if constexpr (kShouldAssumeProtocolComposesNode ||
                       internal::has_fidl_method_fuchsia_io_clone_v<Protocol>) {
    static_assert(!(internal::has_fidl_method_fuchsia_io_clone_v<Protocol> &&
                    kShouldAssumeProtocolComposesNode),
                  "|Protocol| already appears to compose the |fuchsia.io/Node| protocol. "
                  "There is no need to specify |AssumeProtocolComposesNode|.");
    result = internal::CloneRaw(fidl::UnownedClientEnd<fuchsia_io::Node>(client.channel()));
  } else {
    // This assertion will always fail.
    static_assert(
        internal::has_fidl_method_fuchsia_io_clone_v<Protocol> ||
            internal::has_fidl_method_fuchsia_unknown_clone_v<Protocol>,
        "|Protocol| should compose either |fuchsia.unknown/Cloneable| or |fuchsia.io/Node|.");
    __builtin_unreachable();
  }

  if (!result.is_ok()) {
    return result.take_error();
  }
  return zx::ok(fidl::ClientEnd<Protocol>(std::move(*result)));
}

// Overload of |component::Clone| to emulate implicit conversion from a
// |const fidl::ClientEnd&| into |fidl::UnownedClientEnd|. C++ cannot consider
// actual implicit conversions when performing template argument deduction.
template <typename Protocol, typename Tag = std::nullptr_t>
zx::result<fidl::ClientEnd<Protocol>> Clone(const fidl::ClientEnd<Protocol>& node,
                                            Tag tag = nullptr) {
  return Clone(node.borrow(), tag);
}

// Typed channel wrapper around |fuchsia.io/Node.Clone|.
//
// Different from |Clone|, this version swallows any synchronous error and will
// return an invalid client-end in those cases. As such, |component::Clone| should
// be preferred over this function.
template <typename Protocol, typename Tag = std::nullptr_t,
          typename = std::enable_if_t<
              std::disjunction_v<std::is_same<Tag, std::nullptr_t>,
                                 std::is_same<Tag, internal::AssumeProtocolComposesNodeTag>>>>
fidl::ClientEnd<Protocol> MaybeClone(fidl::UnownedClientEnd<Protocol> node, Tag tag = nullptr) {
  auto result = Clone(node, tag);
  if (!result.is_ok()) {
    return {};
  }
  return std::move(*result);
}

// Overload of |component::MaybeClone| to emulate implicit conversion from a
// |const fidl::ClientEnd&| into |fidl::UnownedClientEnd|. C++ cannot consider
// actual implicit conversions when performing template argument deduction.
template <typename Protocol, typename Tag = std::nullptr_t>
fidl::ClientEnd<Protocol> MaybeClone(const fidl::ClientEnd<Protocol>& node, Tag tag = nullptr) {
  return MaybeClone(node.borrow(), tag);
}

}  // namespace component

#endif  // LIB_COMPONENT_INCOMING_CPP_CLONE_H_
