// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file contains macros used by the FUCHSIA_API_LEVEL_*() macros in availability.h.
// It is a separate file to allow API level support to evolve separately from the
// API level-independent macros in that file.

// Special named API levels.
#define FUCHSIA_INTERNAL_LEVEL_HEAD_() FUCHSIA_INTERNAL_USE_ONLY_FUCHSIA_HEAD_()
// TODO(https://fxbug.dev/323889271): Add support for `NEXT` as a parameter.
// #define FUCHSIA_INTERNAL_LEVEL_NEXT_() TBD

// Published stable API levels.
//
// Only existing stable API levels should be included below to ensure `NEXT` and
// similar are used as appropriate. The levels are defined in decreasing order
// to allow newer levels to be used in conditions for defining older levels.
//
// Although sunset and retired API levels are not supported by the SDK,
// downstream code may still have conditionals based on those levels, and
// removing support for those conditionals could unnecessarily break those
// conditional, which are still correct even if obsolete. Therefore, support for
// retired API levels must be carefully considered and only applied at the
// `NEXT` API level.
//
// TODO(https://fxbug.dev/42084512): Generate the numbered levels from version_history.json.
// TODO(https://fxbug.dev/323889271): Remove the in-development API level number
// once `NEXT` is supported.
#define FUCHSIA_INTERNAL_LEVEL_21_() 21
#define FUCHSIA_INTERNAL_LEVEL_20_() 20
#define FUCHSIA_INTERNAL_LEVEL_19_() 19
#define FUCHSIA_INTERNAL_LEVEL_18_() 18
#define FUCHSIA_INTERNAL_LEVEL_17_() 17
#define FUCHSIA_INTERNAL_LEVEL_16_() 16
#define FUCHSIA_INTERNAL_LEVEL_15_() 15
