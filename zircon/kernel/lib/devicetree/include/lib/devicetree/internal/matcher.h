// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_DEVICETREE_INCLUDE_LIB_DEVICETREE_INTERNAL_MATCHER_H_
#define ZIRCON_KERNEL_LIB_DEVICETREE_INCLUDE_LIB_DEVICETREE_INTERNAL_MATCHER_H_

#include <cstddef>
#include <cstdint>
#include <string_view>
#include <tuple>
#include <type_traits>

#include <fbl/macros.h>

#include "../devicetree.h"

namespace devicetree {

// Defined in <lib/devicetree/matcher.h>
enum class ScanState;

namespace internal {

DECLARE_HAS_MEMBER_FN(HasOnNode, OnNode);
DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(OnNodeSignature, OnNode,
                                     ScanState (C::*)(const NodePath&, const PropertyDecoder&));

DECLARE_HAS_MEMBER_FN(HasOnWalk, OnWalk);
DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(OnWalkSignature, OnWalk, ScanState (C::*)());

DECLARE_HAS_MEMBER_FN(HasOnError, OnError);
DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(OnErrorSignature, OnError, void (C::*)(std::string_view));

template <typename T>
struct HasMaxScansMember {
 private:
  template <typename C>
  static std::true_type test(std::add_pointer_t<decltype(C::kMaxScans)>);

  template <typename C>
  static std::false_type test(...);

 public:
  static constexpr bool value = decltype(test<T>(nullptr))::value;
};

template <typename Matcher>
constexpr bool CheckInterface() {
  static_assert(HasOnNode<Matcher>::value, "|Matcher| must implement |OnNode| member function.");
  static_assert(OnNodeSignature<Matcher>::value, "|Matcher::OnNode| has incorrect signature.");

  static_assert(HasOnWalk<Matcher>::value, "|Matcher| must implement |OnWalk| member function.");
  static_assert(OnWalkSignature<Matcher>::value, "|Matcher::OnWalk| has incorrect signature.");

  static_assert(HasOnError<Matcher>::value, "|Matcher| must implement |OnError| member function.");
  static_assert(OnErrorSignature<Matcher>::value, "|Matcher::OnError| has incorrect signature.");

  static_assert(HasMaxScansMember<Matcher>::value,
                "|Matcher| must declare |size_t kMaxScans| static member");
  static_assert(std::is_same_v<decltype(Matcher::kMaxScans), const size_t>,
                "|Matcher::kMaxScans| must be of type |size_t|.");
  static_assert(Matcher::kMaxScans > 0, "|Matcher::kMaxScans| must be greater than zero.");
  return true;
}

template <typename Matcher>
constexpr bool kIsMatcher = OnNodeSignature_v<Matcher> && OnWalkSignature_v<Matcher> &&
                            OnErrorSignature_v<Matcher> && HasMaxScansMember<Matcher>::value;

// All visitors have the same return type.
template <typename Visitor, typename... Matchers>
using VisitorResultType =
    std::invoke_result_t<Visitor, typename std::tuple_element_t<0, std::tuple<Matchers...>>,
                         size_t>;

// Helper for visiting each matcher with their respective index (matcher0, .... matcherN-1,
// where N == sizeof...(Matchers...)).
template <typename Visitor, size_t... Is, typename... Matchers>
constexpr void ForEachMatcher(Visitor&& visitor, std::index_sequence<Is...> is,
                              Matchers&&... matchers) {
  // Visitor has void return type, wrap on some callable that just returns true and discard
  // the result.
  auto wrapper = [&](auto& matcher, size_t index) -> bool {
    visitor(matcher, index);
    return true;
  };
  [[maybe_unused]] auto res = {wrapper(matchers, Is)...};
}

// Helper for visiting each matcher.
template <typename Visitor, typename... Matchers>
constexpr void ForEachMatcher(Visitor&& visitor, Matchers&&... matchers) {
  // All visitor must be invocable with the visit state and the matchers.
  static_assert((... && std::is_invocable_v<Visitor, Matchers, size_t>),
                "|Visitor| must provide an overload for each provided matcher.");

  static_assert((... && std::is_same_v<VisitorResultType<Visitor, Matchers...>,
                                       std::invoke_result_t<Visitor, Matchers, size_t>>),
                "|Visitor| must have the same return type for all matcher types.");
  ForEachMatcher(std::forward<Visitor>(visitor), std::make_index_sequence<sizeof...(Matchers)>{},
                 std::forward<Matchers>(matchers)...);
}

// Per matcher state kept by the |Match| infrastructre. This state determines whether a matcher
// is active(visiting all nodes), suspended(not visiting current subtree) or done(not visiting
// anything anymore).
template <typename State = ScanState>
class MatcherVisit {
 public:
  constexpr MatcherVisit() = default;
  constexpr MatcherVisit(const MatcherVisit&) = default;
  constexpr MatcherVisit& operator=(const MatcherVisit&) = default;

  // Initialize and assign from
  explicit constexpr MatcherVisit(State state) : state_(state) {}

  constexpr State state() const { return state_; }
  constexpr void set_state(State state) {
    state_ = state;
    if (state_ == State::kNeedsPathResolution) {
      extra_alias_scan_ = 1;
    }
  }

  // Prevents the associated matcher's callback from being called in any nodes
  // that are part of the subtree of the current node.
  void Prune(const NodePath& path) { mark_ = &path.back(); }

  // Counterpart to |Prune|, which marks the end of a subtree, and allows for
  // matcher's callbacks to be called again.
  void Unprune(const NodePath& path) {
    if (mark_ == &path.back()) {
      *this = MatcherVisit();
    }
  }

  // Extra
  constexpr size_t extra_alias_scan() const { return extra_alias_scan_; }

 private:
  State state_ = State::kActive;
  const Node* mark_ = nullptr;
  size_t extra_alias_scan_ = 0;
};

template <typename Matcher>
constexpr size_t GetMaxScans() {
  using MatcherType = std::decay_t<Matcher>;
  return MatcherType::kMaxScans;
}

// A Matcher whose sole pupose is to guarantee that aliases will be resolved if present. This allows
// indicating that alias still need to be resolved, if no user provided matcher can make progress
// due to path resolution.
class AliasMatcher {
 public:
  // Aliases must be resolved within one walk.
  static constexpr size_t kMaxScans = 1;

  ScanState OnNode(const NodePath& path, const PropertyDecoder& decoder);

  ScanState OnWalk();

  void OnError(std::string_view error);
};

static_assert(CheckInterface<AliasMatcher>());

}  // namespace internal

}  // namespace devicetree

#endif  // ZIRCON_KERNEL_LIB_DEVICETREE_INCLUDE_LIB_DEVICETREE_INTERNAL_MATCHER_H_
