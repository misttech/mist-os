// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package artifactory

import (
	"archive/tar"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"go.fuchsia.dev/fuchsia/tools/build"
)

// Implements imgModules
type mockModules struct {
	imgs     []build.Image
	buildDir string
}

func (m mockModules) BuildDir() string {
	return m.buildDir
}

func (m mockModules) ImageManifest() string {
	return "BUILD_DIR/IMAGE_MANIFEST"
}

func (m mockModules) Images() []build.Image {
	return m.imgs
}

func TestImageUploads(t *testing.T) {
	// Create a temporary disk.raw image.
	dir := t.TempDir()
	name := filepath.Join(dir, "disk.raw")
	content := []byte("Hello World!")
	if err := os.WriteFile(name, content, 0o600); err != nil {
		t.Fatalf("failed to write to fake disk.raw file: %s", err)
	}
	m := &mockModules{
		buildDir: dir,
		imgs: []build.Image{
			{
				Name: "uefi-disk",
				Path: "disk.raw",
				Type: "blk",
			},
		},
	}
	want := []Upload{
		{
			Source:      "BUILD_DIR/IMAGE_MANIFEST",
			Destination: "namespace/IMAGE_MANIFEST",
			Signed:      true,
		},
		{
			Source:      name,
			Destination: filepath.Join("namespace", gceUploadName),
			Compress:    true,
			Signed:      true,
			TarHeader: &tar.Header{
				Format: tar.FormatGNU,
				Name:   gceImageName,
				Mode:   0o666,
				Size:   int64(len(content)),
			},
		},
	}
	got, err := imageUploads(m, "namespace")
	if err != nil {
		t.Fatalf("imageUploads failed: %s", err)
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("unexpected image uploads (-want +got):\n%s", diff)
	}
}
