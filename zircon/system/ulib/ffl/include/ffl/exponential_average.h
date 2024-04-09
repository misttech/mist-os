// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef FFL_EXPONENTIAL_AVERAGE_H_
#define FFL_EXPONENTIAL_AVERAGE_H_

#include <ffl/fixed.h>

namespace ffl {

// Represents an exponential moving average. It is calculated as follows:
//
// If the caller specifies only alpha, then a single-rate moving average is used. Letting Yn be the
// value of the last observation, the moving average is calculated as:
//   Sn = Sn-1 + a * (Yn - Sn-1)
//
// If the caller specifies both alpha and beta, then a dual-rate moving average is used:
//   D  = Yn - Sn-1
//        { Sn-1 + a * D      if D < 0
//   Sn = {
//        { Sn-1 + b * D      if D >= 0
//
// In either case, the smoothing factors (a and b) are always required to be between 0 and 1.
template <typename Value, typename Alpha, typename Beta, typename = void>
class ExponentialAverage;

template <typename V, size_t VFractionalBits,        //
          typename A, size_t AFractionalBits,        //
          typename B, size_t BFractionalBits>        //
class ExponentialAverage<Fixed<V, VFractionalBits>,  //
                         Fixed<A, AFractionalBits>,  //
                         Fixed<B, BFractionalBits>> {
 public:
  using Value = Fixed<V, VFractionalBits>;
  using Alpha = Fixed<A, AFractionalBits>;
  using Beta = Fixed<B, BFractionalBits>;

  constexpr ExponentialAverage(Value value, Alpha alpha)
      : ExponentialAverage(value, alpha, alpha) {}

  constexpr ExponentialAverage(Value value, Alpha alpha, Beta beta)
      : average_(value), alpha_(alpha), beta_(beta) {
    if (alpha < 0) {
      __builtin_abort();
    }
    if (alpha > 1) {
      __builtin_abort();
    }
    if (beta < 0) {
      __builtin_abort();
    }
    if (beta > 1) {
      __builtin_abort();
    }
  }

  constexpr void AddSample(Value sample) {
    const Value delta = sample - average_;
    average_ += delta >= 0 ? Value{beta_ * delta} : Value{alpha_ * delta};
  }

  constexpr Value value() { return average_; }

 private:
  Value average_;
  const Alpha alpha_;
  const Beta beta_;
};

// User-defined deduction guide for construction of a single-rate exponential average.
template <typename V, size_t VFractionalBits,        //
          typename A, size_t AFractionalBits>        //
ExponentialAverage(Fixed<V, VFractionalBits>,        //
                   Fixed<A, AFractionalBits>)        //
    ->ExponentialAverage<Fixed<V, VFractionalBits>,  //
                         Fixed<A, AFractionalBits>,  //
                         Fixed<A, AFractionalBits>>;

// User-defined deduction guide for construction of a dual-rate exponential average.
template <typename V, size_t VFractionalBits,        //
          typename A, size_t AFractionalBits,        //
          typename B, size_t BFractionalBits>        //
ExponentialAverage(Fixed<V, VFractionalBits>,        //
                   Fixed<A, AFractionalBits>,        //
                   Fixed<B, BFractionalBits>)        //
    ->ExponentialAverage<Fixed<V, VFractionalBits>,  //
                         Fixed<A, AFractionalBits>,  //
                         Fixed<B, BFractionalBits>>;

}  // namespace ffl

#endif  // FFL_EXPONENTIAL_AVERAGE_H_
