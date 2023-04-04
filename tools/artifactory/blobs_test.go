// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package artifactory

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestBlobsUpload(t *testing.T) {
	dir := t.TempDir()
	name := filepath.Join(dir, "all_blobs.json")
	content := []byte("[" +
		"{" +
		"\"source_path\":\"\"," +
		"\"path\":\"\"," +
		"\"merkle\":\"0000000000000000000000000000000000000000000000000000000000000000\"," +
		"\"size\":0" +
		"}," +
		"{" +
		"\"source_path\":\"\"," +
		"\"path\":\"\"," +
		"\"merkle\":\"1111111111111111111111111111111111111111111111111111111111111111\"," +
		"\"size\":0" +
		"}" +
		"]")
	if err := os.WriteFile(name, content, 0o600); err != nil {
		t.Fatalf("failed to write to fake all_blobs.json file: %s", err)
	}
	expectedUploads := []Upload{
		{
			Source:      "blobs/0000000000000000000000000000000000000000000000000000000000000000",
			Destination: "namespace/0000000000000000000000000000000000000000000000000000000000000000",
			Deduplicate: true,
		},
		{
			Source:      "blobs/1111111111111111111111111111111111111111111111111111111111111111",
			Destination: "namespace/1111111111111111111111111111111111111111111111111111111111111111",
			Deduplicate: true,
		},
	}
	actualUploads, err := BlobsUploads(name, "blobs", "namespace")
	if err != nil {
		t.Fatalf("BlobsUploads failed: %s", err)
	}
	if diff := cmp.Diff(actualUploads, expectedUploads); diff != "" {
		t.Fatalf("unexpected blobs uploads, diff:\n%s", diff)
	}
}
