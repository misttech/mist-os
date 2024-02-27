// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package directory

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

var testDataDir = flag.String("test_data_dir", "", "Path to test data directory")

// NewDirectory(empty) should produce a directory object that correctly
// represents an empty directory.
func TestDirectoryCreateEmpty(t *testing.T) {
	runDirectoryTest("empty", t)
}

// NewDirectory(simple) should produce a directory object that correctly
// represents the simple testdata directory.
func TestDirectoryCreateSimple(t *testing.T) {
	runDirectoryTest("simple", t)
}

// NewDirectory(skip) should produce a directory object that correctly
// skips the configured directories.
func TestDirectoryWithSkips(t *testing.T) {
	runDirectoryTest("skipdir", t)
}

func runDirectoryTest(name string, t *testing.T) {
	t.Helper()

	testDir := filepath.Join(*testDataDir, name)
	root := filepath.Join(testDir, "root")

	// Create a Directory object from the want.json file.
	gnArchName, ok := map[string]string{"amd64": "x64", "arm64": "arm64"}[runtime.GOARCH]
	if !ok {
		t.Fatal("unknown arch ", runtime.GOARCH)
	}
	wantPath := filepath.Join(testDir, fmt.Sprintf("want_%v.json", gnArchName))
	want := &Directory{}
	decodeJSON(wantPath, want, t)

	configPath := filepath.Join(testDir, "config.json")
	config := NewConfig()
	decodeJSON(configPath, config, t)
	cleanConfig(config, root)

	got, err := newDirectoryWithConfig(root, nil, config)
	if err != nil {
		t.Fatal(err)
	}

	diffDirectories(want, got, t)
}

func cleanConfig(c *DirectoryConfig, root string) {
	for _, s := range c.Skips {
		for i, p := range s.Paths {
			s.Paths[i] = strings.ReplaceAll(p, "{root}", root)
		}
	}
}

func decodeJSON(path string, obj interface{}, t *testing.T) {
	t.Helper()

	contents, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}

	decoder := json.NewDecoder(strings.NewReader(string(contents)))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(obj); err != nil {
		t.Fatalf("%v: failed to decode %s into struct: %v.", t.Name(), path, err)
	}
}

func diffDirectories(want, got *Directory, t *testing.T) {
	t.Helper()

	if want.Name != got.Name {
		t.Errorf("%s: directory name mismatch: (-want +got):\n-%s\n+%s", t.Name(), want.Name, got.Name)
	}

	if len(want.Files) != len(got.Files) {
		t.Errorf("%s: files length mismatch:(-want +got):\n-%d\n+%d", t.Name(), len(want.Files), len(got.Files))
	}
	for i := range want.Files {
		w := want.Files[i]
		g := got.Files[i]

		if w.Name() != g.Name() {
			t.Errorf("%s: file names mismatch:(-want +got):\n-%s\n+%s", t.Name(), w.Name(), g.Name())
		}
		if w.SPDXID() != g.SPDXID() {
			t.Errorf("%s: file SPDXID mismatch:(-want +got):\n-%s\n+%s", t.Name(), w.SPDXID(), g.SPDXID())
		}
	}

	if len(want.Children) != len(got.Children) {
		t.Errorf("%s: children length mismatch:(-want +got):\n-%d\n+%d", t.Name(), len(want.Children), len(got.Children))
	}
	for i := range want.Children {
		diffDirectories(want.Children[i], got.Children[i], t)
	}
}
