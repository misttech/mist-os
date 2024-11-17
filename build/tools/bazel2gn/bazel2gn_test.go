// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package bazel2gn_test

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"go.fuchsia.dev/fuchsia/build/tools/bazel2gn"
	"go.starlark.net/starlark"
	"go.starlark.net/syntax"
)

// toSyntaxFile is a test helper that parses the input string (content of a
// BUILD.bazel file) to a *syntax.File.
func toSyntaxFile(t *testing.T, s string) *syntax.File {
	t.Helper()

	p := filepath.Join(t.TempDir(), "BUILD.bazel.test")
	if err := os.WriteFile(p, []byte(s), 0600); err != nil {
		t.Fatalf("Failed to write test Bazel file: %v", err)
	}

	opts := new(syntax.FileOptions)
	f, _, err := starlark.SourceProgramOptions(opts, p, nil, func(string) bool { return true })
	if err != nil {
		t.Fatalf("Failed to parse test Bazel build file: %v, file content:\n%s", err, s)
	}
	return f
}

// bazelToGN is a test helper that converts all statements in a *syntax.File to
// content of a BUILD.gn.
func bazelToGN(f *syntax.File) (string, error) {
	var gotLines []string
	for _, stmt := range f.Stmts {
		lines, err := bazel2gn.StmtToGN(stmt)
		if err != nil {
			return "", fmt.Errorf("converting Bazel statement to GN: %v", err)
		}
		gotLines = append(gotLines, lines...)
	}
	return strings.Join(gotLines, "\n"), nil
}

func TestStmtToGN(t *testing.T) {
	for _, tc := range []struct {
		name   string
		bazel  string
		wantGN string
	}{
		{
			name: "Simple Go targets",
			bazel: `load("@io_bazel_rules_go//go:def.bzl", "go_binary", "go_library", "go_test")

go_library(
  name = "bazel2gn",
  srcs = [
    "bazel2gn.go",
  ],
  importpath = "go.fuchsia.dev/fuchsia/build/tools/bazel2gn",
  deps = [
    "//third_party/golibs:go.starlark.net/syntax",
  ],
)

go_binary(
  name = "cmd",
  srcs = [
    "cmd/main.go",
  ],
  deps = [
    ":bazel2gn",
    "//third_party/golibs:go.starlark.net/starlark",
    "//third_party/golibs:go.starlark.net/syntax",
  ],
)

go_test(
  name = "bazel2gn_tests",
  embed = [ ":bazel2gn" ],
  srcs = [
    "bazel2gn_test.go",
  ],
  deps = [
    "//third_party/golibs:github.com/google/go-cmp/cmp",
    "//third_party/golibs:go.starlark.net/starlark",
    "//third_party/golibs:go.starlark.net/syntax",
  ],
)`,
			wantGN: `go_library("bazel2gn") {
  sources = [
    "bazel2gn.go",
  ]
  importpath = "go.fuchsia.dev/fuchsia/build/tools/bazel2gn"
  deps = [
    "//third_party/golibs:go.starlark.net/syntax",
  ]
}
go_binary("cmd") {
  sources = [
    "cmd/main.go",
  ]
  deps = [
    ":bazel2gn",
    "//third_party/golibs:go.starlark.net/starlark",
    "//third_party/golibs:go.starlark.net/syntax",
  ]
}
go_test("bazel2gn_tests") {
  embed = [
    ":bazel2gn",
  ]
  sources = [
    "bazel2gn_test.go",
  ]
  deps = [
    "//third_party/golibs:github.com/google/go-cmp/cmp",
    "//third_party/golibs:go.starlark.net/starlark",
    "//third_party/golibs:go.starlark.net/syntax",
  ]
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
				t.Errorf("Diff found after GN conversion (-got +want):\n%s\nBazel source:\n%s", diff, tc.bazel)
			}
		})
	}
}

func TestTargetCompatibleWith(t *testing.T) {
	for _, tc := range []struct {
		name   string
		bazel  string
		wantGN string
	}{
		{
			name: "success",
			bazel: `
load("@platforms//host:constraints.bzl", "HOST_CONSTRAINTS")

go_binary(
	name = "host_tool",
	srcs = [
	  "main.go",
	],
	target_compatible_with = HOST_CONSTRAINTS,
)`,
			wantGN: `if (is_host) {
  go_binary("host_tool") {
    sources = [
      "main.go",
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
				t.Errorf("Diff found after GN conversion (-got +want):\n%s\nBazel source:\n%s", diff, tc.bazel)
			}
		})
	}
}

func TestTargetCompatibleWithErrors(t *testing.T) {
	for _, tc := range []struct {
		name  string
		bazel string
	}{
		{
			name: "unexpected target_compatible_with variable",
			bazel: `
go_binary(
	name = "host_tool",
	srcs = [
	  "main.go",
	],
	target_compatible_with = UNSUPPORTED_CONSTRAINTS,
)`,
		},
		{
			name: "list of constraints not supported yet",
			bazel: `
go_binary(
	name = "host_tool",
	srcs = [
	  "main.go",
	],
  target_compatible_with = [
    "@platforms//os:linux",
    "@platforms//cpu:x86_64",
  ],
)`,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := toSyntaxFile(t, tc.bazel)
			_, err := bazelToGN(f)
			if err == nil {
				t.Fatal("Expecting failure converting Bazel targets, got nil")
			}
		})
	}
}

func TestVisibility(t *testing.T) {
	for _, tc := range []struct {
		name   string
		bazel  string
		wantGN string
	}{
		{
			name: "public",
			bazel: `go_library(
  name = "test",
  visibility = [
    "//visibility:public",
  ],
)`,
			wantGN: `go_library("test") {
  visibility = [
    "*",
  ]
}`,
		},
		{
			name: "private",
			bazel: `go_library(
  name = "test",
  visibility = [
    "//visibility:private",
  ],
)`,
			wantGN: `go_library("test") {
  visibility = [
    ":*",
  ]
}`,
		},
		{
			name: "pkg and subpackages",
			bazel: `go_library(
  name = "test",
  visibility = [
    "//path/to/foo:__pkg__",
    "//path/to/bar:__subpackages__",
  ],
)`,
			wantGN: `go_library("test") {
  visibility = [
    "//path/to/foo:*",
    "//path/to/bar/*",
  ]
}`,
		},
		{
			name: "package group is unchanged",
			bazel: `go_library(
  name = "test",
  visibility = [
    "//path/to/foo:__pkg__",
    "//path/to/bar:bar",
  ],
)`,
			wantGN: `go_library("test") {
  visibility = [
    "//path/to/foo:*",
    "//path/to/bar:bar",
  ]
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
				t.Errorf("Diff found after GN conversion (-got +want):\n%s\nBazel source:\n%s", diff, tc.bazel)
			}
		})
	}
}
