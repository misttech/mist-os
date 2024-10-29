// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package gnerator_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"go.fuchsia.dev/fuchsia/build/tools/gnerator"
	"go.starlark.net/starlark"
	"go.starlark.net/syntax"
)

// toSyntaxTree is a test helper that parses the input string (content of a
// BUILD.bazel file) to a *syntax.File.
func toSyntaxTree(t *testing.T, s string) *syntax.File {
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

// bazelToGN is a test helper that converts a *syntax.File to content of a
// BUILD.gn.
func bazelToGN(t *testing.T, f *syntax.File) string {
	t.Helper()

	var gotLines []string
	for _, stmt := range f.Stmts {
		lines, err := gnerator.StmtToGN(stmt)
		if err != nil {
			t.Fatalf("Failed to convert Bazel statement to GN: %v", err)
		}
		gotLines = append(gotLines, lines...)
	}
	return strings.Join(gotLines, "\n")
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
  name = "gnerator",
  srcs = [
    "gnerator.go",
  ],
  importpath = "go.fuchsia.dev/fuchsia/build/tools/gnerator",
  target_compatible_with = [
    "@platforms//os:linux",
    "@platforms//cpu:x86_64",
  ],
  deps = [
    "//third_party/golibs:go.starlark.net/syntax",
  ],
)

go_binary(
  name = "cmd",
  srcs = [
    "cmd/main.go",
  ],
  target_compatible_with = [
    "@platforms//os:linux",
    "@platforms//cpu:x86_64",
  ],
  deps = [
    ":gnerator",
    "//third_party/golibs:go.starlark.net/starlark",
    "//third_party/golibs:go.starlark.net/syntax",
  ],
)

go_test(
  name = "gnerator_tests",
  embed = [ ":gnerator" ],
  srcs = [
    "gnerator_test.go",
  ],
  target_compatible_with = [
    "@platforms//os:linux",
    "@platforms//cpu:x86_64",
  ],
  deps = [
    "//third_party/golibs:github.com/google/go-cmp/cmp",
    "//third_party/golibs:go.starlark.net/starlark",
    "//third_party/golibs:go.starlark.net/syntax",
  ],
)`,
			wantGN: `go_library("gnerator") {
  sources = [
    "gnerator.go",
  ]
  importpath = "go.fuchsia.dev/fuchsia/build/tools/gnerator"
  deps = [
    "//third_party/golibs:go.starlark.net/syntax",
  ]
}
go_binary("cmd") {
  sources = [
    "cmd/main.go",
  ]
  deps = [
    ":gnerator",
    "//third_party/golibs:go.starlark.net/starlark",
    "//third_party/golibs:go.starlark.net/syntax",
  ]
}
go_test("gnerator_tests") {
  embed = [
    ":gnerator",
  ]
  sources = [
    "gnerator_test.go",
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
			f := toSyntaxTree(t, tc.bazel)
			gotGN := bazelToGN(t, f)
			if diff := cmp.Diff(gotGN, tc.wantGN); diff != "" {
				t.Errorf("Diff found after GN conversion (-got +want):\n%s\nBazel source:\n%s", diff, tc.bazel)
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
			f := toSyntaxTree(t, tc.bazel)
			gotGN := bazelToGN(t, f)
			if diff := cmp.Diff(gotGN, tc.wantGN); diff != "" {
				t.Errorf("Diff found after GN conversion (-got +want):\n%s\nBazel source:\n%s", diff, tc.bazel)
			}
		})
	}
}
