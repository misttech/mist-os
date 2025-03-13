// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_COUNTER_DISPATCHER_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_COUNTER_DISPATCHER_H_

#include <zircon/rights.h>
#include <zircon/types.h>

#include <ktl/atomic.h>
#include <object/dispatcher.h>
#include <object/handle.h>

class CounterDispatcher : public SoloDispatcher<CounterDispatcher, ZX_DEFAULT_COUNTER_RIGHTS> {
 public:
  ~CounterDispatcher() override;

  // Creates a CounterDispatcher.
  static zx_status_t Create(KernelHandle<CounterDispatcher>* handle, zx_rights_t* rights);

  zx_obj_type_t get_type() const final { return ZX_OBJ_TYPE_COUNTER; }

  // Returns the counter's value.
  //
  // Synchronizes-with |SetValue| or |Add|.
  int64_t Value() const {
    Guard<CriticalMutex> guard{get_lock()};
    return value_;
  }

  // Sets the counter's value, asserting/deasserting signals as appropriate.
  //
  // Synchronizes-with |Value| or |Add|.
  void SetValue(int64_t new_value) {
    Guard<CriticalMutex> guard{get_lock()};
    SetValueLocked(new_value);
  }

  // Adds |amount| to this counter.
  //
  // Synchronizes-with |SetValue| or |Value|.
  //
  // Returns an error if the value would underflow/overflow.
  zx_status_t Add(int64_t amount) {
    Guard<CriticalMutex> guard{get_lock()};

    const int64_t before = value_;
    int64_t after;
    if (add_overflow(before, amount, &after)) {
      return ZX_ERR_OUT_OF_RANGE;
    }

    SetValueLocked(after);

    return ZX_OK;
  }

 private:
  CounterDispatcher();

  // Returns true iff |old_value| is non-positive and |new_value| is positive.
  static bool NewlyPositive(int64_t old_value, int64_t new_value) {
    return (old_value <= 0 && new_value > 0);
  }

  // Returns true iff |old_value| is positive and |new_value| is non-positive.
  static bool NewlyNonPositive(int64_t old_value, int64_t new_value) {
    return (old_value > 0 && new_value <= 0);
  }

  void SetValueLocked(int64_t new_value) TA_REQ(get_lock()) {
    const int64_t old_value = value_;
    value_ = new_value;

    if (NewlyPositive(old_value, new_value)) {
      UpdateStateLocked(ZX_COUNTER_NON_POSITIVE, ZX_COUNTER_POSITIVE);
    } else if (NewlyNonPositive(old_value, new_value)) {
      UpdateStateLocked(ZX_COUNTER_POSITIVE, ZX_COUNTER_NON_POSITIVE);
    }
  }

  int64_t value_ TA_GUARDED(get_lock()){};
};

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_COUNTER_DISPATCHER_H_
