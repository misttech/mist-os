// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"path/filepath"
	"testing"
)

func TestGolibReadmeGenerationGithub(t *testing.T) {
	path := "third_party/golibs/vendor/github.com/foo/bar"
	runGolibTest(t, path)
}

func TestGolibReadmeGenerationGolangOrg(t *testing.T) {
	path := "third_party/golibs/vendor/golang.org/foo/bar"
	runGolibTest(t, path)
}

func runGolibTest(t *testing.T, root string) {
	t.Helper()

	wantPath := filepath.Join(*testDataDir, "go", root, "want.json")

	got, err := NewGolibReadme(filepath.Join(*testDataDir, "go", root))
	if err != nil {
		t.Fatalf("%v: expected no error, got %v.", t.Name(), err)
	}

	runReadmeDiffTest(t, wantPath, got)
}
