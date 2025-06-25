// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_HANDOFF_REQUIREMENTS_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_HANDOFF_REQUIREMENTS_H_

// The information about the kernel stage needed at the phys stage to properly
// prepare it.
//
// The expectation is that structure will be recorded as an ELF note in the
// kernel to be loaded and parsed during hand-off preparation.
//
// TODO(https://fxbug.dev/42164859): Sizes and alignments relating to C++ ABI
// set-up (e.g., stack sizes).
struct PhysHandoffRequirements {};

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_HANDOFF_REQUIREMENTS_H_
