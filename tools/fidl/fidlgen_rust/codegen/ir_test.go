// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package codegen

import (
	"fmt"
	"strings"
	"testing"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgentest"
)

func TestDerivesToString(t *testing.T) {
	cases := []struct {
		input    derives
		expected string
	}{
		{0, ""},
		{derivesDebug, "#[derive(Debug)]"},
		{derivesPartialOrd, "#[derive(PartialOrd)]"},
	}
	for _, ex := range cases {
		actual := ex.input.String()
		if actual != ex.expected {
			t.Errorf("%d: expected '%s', actual '%s'", ex.input, ex.expected, actual)
		}
	}
}

func TestDerivesCalculation(t *testing.T) {
	cases := []struct {
		fidl     string
		expected string
	}{
		{
			fidl:     `type MyStruct = struct { field string; };`,
			expected: "#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]",
		},
		{
			fidl:     `type MyStruct = struct { field float32; };`,
			expected: "#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]",
		},
		{
			fidl:     `type MyStruct = resource struct { field client_end:P; }; protocol P {};`,
			expected: "#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]",
		},
		{
			fidl:     `type MyStruct = resource struct { field uint64; };`,
			expected: "#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]",
		},
		{
			fidl:     `type MyStruct = resource struct {};`,
			expected: "#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]",
		},
	}
	for _, ex := range cases {
		root := Compile(fidlgentest.EndToEndTest{T: t}.Single(`library example; ` + ex.fidl))
		actual := root.Structs[0].Derives.String()
		if ex.expected != actual {
			t.Errorf("%s: expected %s, found %s", ex.fidl, ex.expected, actual)
		}
	}
}

func TestExperiments(t *testing.T) {
	default_experiments := []string{"unknown_interactions"}

	cases := []struct {
		desc        string
		experiments []string
	}{
		{
			desc:        "no experiments",
			experiments: []string{},
		},
		{
			desc:        "single experiment",
			experiments: []string{"allow_new_types"},
		},
		{
			desc:        "multiple experiments",
			experiments: []string{"allow_new_types", "unknown_interactions_new_defaults"},
		},
	}
	for _, test := range cases {
		gen := NewGenerator("", "")
		e2e := fidlgentest.EndToEndTest{T: t}
		for _, ex := range test.experiments {
			e2e = e2e.WithExperiment(ex)
		}

		root := Compile(e2e.Single("library foo.bar;"))
		bytes, err := gen.ExecuteTemplate("GenerateSourceFile", root)
		if err != nil {
			t.Fatalf("case (%s): could not apply template: %s", test.desc, err)
		}

		rust := string(bytes)
		all := root.Experiments
		total_experiments := len(test.experiments) + len(default_experiments)
		if len(all) != total_experiments {
			t.Errorf("case (%s): got %d experiment markers, want %d", test.desc, len(all), total_experiments)
		}
		for _, ex := range test.experiments {
			if !strings.Contains(rust, fmt.Sprintf("\n// fidl_experiment = %s", ex)) {
				t.Errorf("case (%s): could not find '// fidl_experiment = %s' in output", test.desc, ex)
			}
		}
		for _, ex := range default_experiments {
			if !strings.Contains(rust, fmt.Sprintf("\n// fidl_experiment = %s", ex)) {
				t.Errorf("case (%s): could not find '// fidl_experiment = %s' in output", test.desc, ex)
			}
		}
	}
}
