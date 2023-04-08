// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"path/filepath"
	"testing"
)

func TestDartPkgReadmeGeneration(t *testing.T) {
	wantPath := filepath.Join(*testDataDir, "dart", "foo", "want.json")

	got, err := NewDartPkgReadme(filepath.Join(*testDataDir, "dart", "foo"))
	if err != nil {
		t.Fatalf("%v: expected no error, got %v.", t.Name(), err)
	}

	runReadmeDiffTest(t, wantPath, got)
}
