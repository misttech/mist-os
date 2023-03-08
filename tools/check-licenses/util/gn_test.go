// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package util

import (
	"compress/gzip"
	"encoding/json"
	"flag"
	"io"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

var (
	testDataDir = flag.String("test_data_dir", "", "Path to test data directory")
	gnPath      = flag.String("gn_path", "", "Path to gn executable")
	buildDir    = flag.String("build_dir", "", "Path to out directory")
)

func TestFilterTargetsEmpty(t *testing.T) {
	root := filepath.Join(*testDataDir, "empty")
	zippedProjectJson := filepath.Join(root, "project.json.gz")
	projectJson := unzipProjectJson(t, zippedProjectJson)
	gen, err := NewGen(projectJson)
	if err != nil {
		t.Errorf("%v: expected no error, got: %v.", t.Name(), err)
	}

	if len(gen.Targets) > 0 {
		t.Errorf("%v: expected to find no targets, found %d: %v.", t.Name(), len(gen.Targets), gen.Targets)
	}
}

func TestFilterTargets(t *testing.T) {
	root := filepath.Join(*testDataDir, "example")
	zippedProjectJson := filepath.Join(root, "project.json.gz")
	projectJson := unzipProjectJson(t, zippedProjectJson)
	gen, err := NewGen(projectJson)
	if err != nil {
		t.Fatalf("%v: expected no error, (project.json: %v) got %v.", t.Name(), projectJson, err)
	}

	target := "//tools/check-licenses/util/testdata/example:example"
	err = gen.FilterTargets(target, make(map[string]bool, 0))
	if err != nil {
		t.Fatalf("%v: expected no error, (target: %v) got %v.", t.Name(), target, err)
	}

	want := loadWantJSON(filepath.Join(root, "want.json"), t)

	// No need to verify target.Children fields.
	for _, ft := range gen.FilteredTargets {
		ft.Children = nil
	}

	if d := cmp.Diff(want, gen.FilteredTargets); d != "" {
		t.Errorf("%v: compare Gens mismatch: (-want +got):\n%s", t.Name(), d)
	}
}

func loadWantJSON(wantFile string, t *testing.T) map[string]*Target {
	wantFileContent, err := os.ReadFile(wantFile)
	if err != nil {
		t.Fatalf("%v: failed to read in want.json file [%v]: %v\n", t.Name(), wantFile, err)
	}

	var want map[string]*Target
	err = json.Unmarshal(wantFileContent, &want)
	if err != nil {
		t.Fatalf("%v: failed to unmarshal want.json data [%v]: %v\n", t.Name(), wantFile, err)
	}
	return want
}

func unzipProjectJson(t *testing.T, path string) string {
	t.Helper()

	f, err := os.Open(path)
	if err != nil {
		t.Fatalf("%s: failed to open zipped project.json file [%s]: %v", t.Name(), path, err)
	}
	defer f.Close()

	stream, err := gzip.NewReader(f)
	if err != nil {
		t.Fatalf("%s: failed to create gzip reader: %v", t.Name(), err)
	}

	dir := t.TempDir()
	outputFilePath := filepath.Join(dir, "project.json")
	outputFile, err := os.Create(outputFilePath)
	if err != nil {
		t.Fatalf("%s: failed to create project.json file [%s]: %v", t.Name(), outputFilePath, err)
	}
	defer outputFile.Close()

	_, err = io.Copy(outputFile, stream)
	if err != nil {
		t.Fatalf("%s: failed to copy zipped contents into output file [%s]: %v", t.Name(), outputFilePath, err)
	}

	return outputFilePath
}
