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

// only std::ranges::find_if will use std::invoke, so we just write this ourselves.
template <class It, typename Pred>
It find_if(It begin, It end, Pred pred) {
  for (; begin != end; ++begin) {
    if (std::invoke(pred, *begin)) {
      return begin;
    }
  }
  return end;
}

template <class Range, typename Pred>
class filter_view {
 public:
  using value_type = typename Range::value_type;

  filter_view(Range range, Pred pred) : range_{std::move(range)}, filter_{pred} {}

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

    filter_iterator(filter_view& parent, Iter iter) : iter_{iter}, parent_{parent} {}
    filter_iterator(const filter_iterator&) = default;

    filter_iterator& operator++() {
      iter_ = find_if(++iter_, parent_.range_.end(), parent_.filter_);
      return *this;
    }

    filter_iterator operator++(int) {
      filter_iterator old = *this;
      ++*this;
      return old;
    }

    template <bool BiDir = std::is_same_v<iterator_category, std::bidirectional_iterator_tag>,
              typename = std::enable_if_t<BiDir>>
    filter_iterator& operator--() {
      while (!std::invoke(parent_.filter_, *--iter_))
        ;
      return *this;
    }
    template <bool BiDir = std::is_same_v<iterator_category, std::bidirectional_iterator_tag>,
              typename = std::enable_if_t<BiDir>>
    filter_iterator operator--(int) {
      filter_iterator old = *this;
      --*this;
      return old;
    }

    bool operator==(const filter_iterator& other) const { return iter_ == other.iter_; }
    bool operator!=(const filter_iterator& other) const { return iter_ != other.iter_; }

    auto& operator*() { return *iter_; }
    auto* operator->() { return iter_.operator->(); }

   private:
    Iter iter_;
    filter_view& parent_;
  };

  template <class Iter>
  filter_iterator(const filter_view&, Iter) -> filter_iterator<Iter>;

  using iterator = filter_iterator<typename Range::iterator>;

  iterator begin() { return {*this, find_if(range_.begin(), range_.end(), filter_)}; }
  iterator end() { return {*this, range_.end()}; }

 private:
  Range range_;
  [[no_unique_address]] Pred filter_;
};

#endif

}  // namespace ld::internal

#endif  // LIB_LD_INTERNAL_FILTER_VIEW_H_
