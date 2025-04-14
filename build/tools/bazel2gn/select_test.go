// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package bazel2gn_test

import (
	"github.com/google/go-cmp/cmp"
	"testing"
)

func TestSelectConversion(t *testing.T) {
	for _, tc := range []struct {
		name   string
		bazel  string
		wantGN string
	}{
		{
			name: "simple select",
			bazel: `load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
  name = "simple_select",
  srcs = select({
    "@platforms//os:fuchsia": [ "fuchsia.rs" ],
    "@platforms//os:linux": [ "linux.rs" ],
    "//conditions:default": [ "default.rs" ],
  }),
)`,
			wantGN: `rustc_library("simple_select") {
  if (is_fuchsia) {
    sources = [
      "fuchsia.rs",
    ]
  } else if (is_linux) {
    sources = [
      "linux.rs",
    ]
  } else {
    sources = [
      "default.rs",
    ]
  }
}`,
		},
		{
			name: "list concatenation",
			bazel: `load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
  name = "list_concatenation",
  srcs = [
    "foo.rs",
  ] + select({
    "@platforms//os:fuchsia": [ "fuchsia.rs" ],
    "@platforms//os:linux": [ "linux.rs" ],
  }),
)`,
			wantGN: `rustc_library("list_concatenation") {
  sources = []
  sources += [
    "foo.rs",
  ]
  if (is_fuchsia) {
    sources += [
      "fuchsia.rs",
    ]
  } else if (is_linux) {
    sources += [
      "linux.rs",
    ]
  }
}`,
		},
		{
			name: "select no_match_error",
			bazel: `load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
  name = "no_match_error",
	srcs = [
		"foo.rs",
	] + select(
		{
			"@platforms//os:fuchsia": [ "bar.rs" ],
			"@platforms//os:linux": [ "baz.rs" ],
		},
		no_match_error = "unknown platform!",
	),
)`,
			wantGN: `rustc_library("no_match_error") {
  sources = []
  sources += [
    "foo.rs",
  ]
  if (is_fuchsia) {
    sources += [
      "bar.rs",
    ]
  } else if (is_linux) {
    sources += [
      "baz.rs",
    ]
  } else {
    assert(false, "unknown platform!")
  }
}`,
		},
		{
			name: "multiple selects",
			bazel: `load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
	name = "multiple_selects",
	srcs = [
		"foo.rs",
	] + select(
		{
			"@platforms//os:fuchsia": [ "bar.rs" ],
			"@platforms//os:linux": [ "baz.rs" ],
		},
		no_match_error = "unknown platform!",
	) + [
		"yet_another_foo.rs",
	] + select(
		{
			"@platforms//os:fuchsia": [ "yet_another_bar.rs" ],
		},
	),
)`,
			wantGN: `rustc_library("multiple_selects") {
  sources = []
  sources += [
    "foo.rs",
  ]
  if (is_fuchsia) {
    sources += [
      "bar.rs",
    ]
  } else if (is_linux) {
    sources += [
      "baz.rs",
    ]
  } else {
    assert(false, "unknown platform!")
  }
  sources += [
    "yet_another_foo.rs",
  ]
  if (is_fuchsia) {
    sources += [
      "yet_another_bar.rs",
    ]
  }
}`,
		},
		{
			name: "consecutive selects",
			bazel: `load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
	name = "consecutive_selects",
	srcs = select(
		{
			"@platforms//os:fuchsia": [ "foo.rs" ],
			"@platforms//os:linux": [ "bar.rs" ],
		},
		no_match_error = "unknown platform!",
	) + select(
		{
			"@platforms//os:linux": [ "baz.rs" ],
      "//conditions:default": [ "qux.rs" ],
		},
	),
)`,
			wantGN: `rustc_library("consecutive_selects") {
  sources = []
  if (is_fuchsia) {
    sources += [
      "foo.rs",
    ]
  } else if (is_linux) {
    sources += [
      "bar.rs",
    ]
  } else {
    assert(false, "unknown platform!")
  }
  if (is_linux) {
    sources += [
      "baz.rs",
    ]
  } else {
    sources += [
      "qux.rs",
    ]
  }
}`,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := toSyntaxFile(t, tc.bazel)
			gotGN, err := bazelToGN(f)
			if err != nil {
				t.Fatalf("Unexpected failure converting Bazel build targets: %v", err)
			}
			if diff := cmp.Diff(gotGN, tc.wantGN); diff != "" {
				t.Errorf("Diff found after GN conversion (-got +want):\n%s\n\n====== Bazel source ======\n\n%s\n\n====== Converted GN source ======\n\n%s\n", diff, tc.bazel, gotGN)
			}
		})
	}
}

func TestSelectConversionErrors(t *testing.T) {
	for _, tc := range []struct {
		name  string
		bazel string
	}{
		{
			name: "unsupported operator",
			bazel: `load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
  name = "unsupported_operator",
  srcs = select({
    "@platforms//os:fuchsia": [ "fuchsia.rs" ],
    "@platforms//os:linux": [ "linux.rs" ],
		"//conditions:default": [ "default.rs" ],
  }) - [ "minux.rs" ],
)`,
		},
		{
			name: "unsupported select condition",
			bazel: `load("@rules_rust//rust:defs.bzl", "rust_library")

rust_library(
  name = "unsupported_condition",
  srcs = select({
		"unknown_condition": [],
		"//conditions:default": [ "default.rs" ],
  }),
)`,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := toSyntaxFile(t, tc.bazel)
			if _, err := bazelToGN(f); err == nil {
				t.Fatalf("Unexpected success converting Bazel build targets, want failure")
			}
		})
	}
}
