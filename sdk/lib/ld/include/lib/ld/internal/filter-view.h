// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_INTERNAL_FILTER_VIEW_H_
#define LIB_LD_INTERNAL_FILTER_VIEW_H_

#include <lib/stdcompat/version.h>

#include <algorithm>
#include <ranges>

namespace ld::internal {

#if __cpp_lib_ranges >= 201911L

using std::ranges::filter_view;

#else

// Only std::ranges::find_if will use std::invoke, so we just write this
// ourselves.
template <class It, typename Pred>
It FindIf(It begin, It end, Pred&& pred) {
  while (begin != end && !std::invoke(pred, *begin)) {
    ++begin;
  }
  return begin;
}

template <class Range, typename Pred>
class filter_view {
 private:
  // Forward declaration, see below.
  template <class Iter>
  class filter_iterator;

 public:
  using value_type = typename Range::value_type;

  constexpr filter_view(Range range, Pred pred) : range_{std::move(range)}, filter_{pred} {}

  using iterator = filter_iterator<decltype(std::begin(std::declval<Range>()))>;
  using const_iterator = filter_iterator<decltype(std::cbegin(std::declval<Range>()))>;

  iterator begin() { return {*this, FindIf(std::begin(range_), std::end(range_), filter_)}; }
  const_iterator begin() const {
    return {*this, FindIf(std::cbegin(range_), std::cend(range_), filter_)};
  }

  iterator end() { return {*this, std::end(range_)}; }
  const_iterator end() const { return {*this, std::cend(range_)}; }

 private:
  using RangeConstIterator = decltype(std::cbegin(std::declval<Range>()));

  template <class Iter>
  class filter_iterator : public std::iterator_traits<Iter> {
   public:
    using difference_type = ptrdiff_t;
    using value_type = std::remove_reference_t<decltype(*std::declval<Iter>())>;
    using reference = value_type&;
    using iterator_category = std::conditional_t<
        std::is_base_of_v<std::bidirectional_iterator_tag,
                          typename std::iterator_traits<Iter>::iterator_category>,
        std::bidirectional_iterator_tag, std::forward_iterator_tag>;

    constexpr filter_iterator() = default;

    constexpr filter_iterator(const filter_iterator&) = default;

    constexpr filter_iterator& operator++() {
      iter_ = FindIf(++iter_, end(), parent_->filter_);
      return *this;
    }

    constexpr filter_iterator operator++(int) {
      filter_iterator old = *this;
      ++*this;
      return old;
    }

    template <bool BiDir = std::is_same_v<iterator_category, std::bidirectional_iterator_tag>,
              typename = std::enable_if_t<BiDir>>
    constexpr filter_iterator& operator--() {
      while (!std::invoke(parent_->filter_, *--iter_))
        ;
      return *this;
    }

    template <bool BiDir = std::is_same_v<iterator_category, std::bidirectional_iterator_tag>,
              typename = std::enable_if_t<BiDir>>
    constexpr filter_iterator operator--(int) {
      filter_iterator old = *this;
      --*this;
      return old;
    }

    constexpr bool operator==(const filter_iterator& other) const { return iter_ == other.iter_; }

    constexpr bool operator!=(const filter_iterator& other) const { return iter_ != other.iter_; }

    constexpr auto& operator*() { return *iter_; }

    constexpr auto* operator->() { return iter_.operator->(); }

   private:
    friend filter_view;

    constexpr filter_iterator(const filter_view& parent, Iter iter)
        : iter_{iter}, parent_{&parent} {}

    constexpr auto end() const {
      if constexpr (std::is_same_v<Iter, RangeConstIterator>) {
        return std::cend(parent_->range_);
      } else {
        return std::end(parent_->range_);
      }
    }

    Iter iter_;
    const filter_view* parent_ = nullptr;
  };

  [[no_unique_address]] Range range_;
  [[no_unique_address]] Pred filter_;
};

#endif

}  // namespace ld::internal

#endif  // LIB_LD_INTERNAL_FILTER_VIEW_H_
