// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
package orchestrate

import (
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestAppendingToPath(t *testing.T) {
	testCases := []struct {
		name       string
		pathValues []string
		env        []string
		wantResult []string
	}{
		{
			name:       "EmptyEnvAddsPath",
			env:        []string{},
			pathValues: []string{"foo/bar"},
			wantResult: []string{"PATH=foo/bar"},
		},
		{
			name:       "EmptyEnvEmptyArgsAddsNothing",
			env:        []string{},
			pathValues: []string{},
			wantResult: nil,
		},
		{
			name:       "EmptyEnvAddsMultiple",
			env:        []string{},
			pathValues: []string{"foo/bar", "baz/mumble"},
			wantResult: []string{"PATH=foo/bar:baz/mumble"},
		},
		{
			name:       "ExistingPathAppendsInEnv",
			env:        []string{"THIS=hello", "PATH=foo/bar"},
			pathValues: []string{"baz/mumble"},
			wantResult: []string{"THIS=hello", "PATH=foo/bar:baz/mumble"},
		},
		{
			name:       "MultipleArgsConcatenate",
			env:        []string{"THIS=hello", "PATH=foo/bar"},
			pathValues: []string{"baz/mumble", "bingo", "bongo"},
			wantResult: []string{"THIS=hello", "PATH=foo/bar:baz/mumble:bingo:bongo"},
		},
		{
			name:       "EmptyArgsPreservesEnv",
			env:        []string{"THIS=hello", "THAT=foo/bar"},
			pathValues: []string{},
			wantResult: []string{"THIS=hello", "THAT=foo/bar"},
		},
		{
			name:       "NonEmptyEnvCreatesPath",
			env:        []string{"THIS=hello", "THAT=foo/bar"},
			pathValues: []string{"foo/bar"},
			wantResult: []string{"THIS=hello", "THAT=foo/bar", "PATH=foo/bar"},
		},
	}
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			got := AppendPath(tc.env, tc.pathValues...)
			if diff := cmp.Diff(tc.wantResult, got); diff != "" {
				t.Errorf("AppendPath() returned unexpected diff (-want +got):\n%s", diff)
			}
		})
	}
}
