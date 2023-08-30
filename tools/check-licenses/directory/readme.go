// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package directory

import (
	"os"
	"path/filepath"
)

func readmeFileExists(root string) (string, bool) {
	var path string

	directoryContents, err := os.ReadDir(root)
	if err != nil {
		return "", false
	}

	// If multiple README.* files exist in a given directory,
	// README.fuchsia files will take precedence.
	for _, item := range directoryContents {
		switch item.Name() {
		case "README.fuchsia":
			path = item.Name()

		case "README.chromium", "README.crashpad":
			if path == "" {
				path = item.Name()
			}
		}
	}
	if path != "" {
		return filepath.Join(root, path), true
	}
	return "", false
}

func readmeFileWillNeverExist(root string) bool {
	// There are several other 3p projects that don't (and will never) have a README.(*) file.
	// Handle those projects separately.
	return isCustomGolibProject(root) ||
		isCustomRustCrateProject(root) ||
		isCustomDartPkgProject(root)
}

func isCustomDartPkgProject(path string) bool {
	parent := filepath.Dir(path)
	return parent == "third_party/dart-pkg/pub"
}

func isCustomRustCrateProject(path string) bool {
	parent := filepath.Dir(path)

	switch parent {
	case "third_party/rust_crates/vendor",
		"third_party/rust_crates/forks",
		"third_party/rust_crates/ask2patch",
		"third_party/rust_crates/compat",
		"third_party/rust_crates/empty",
		"third_party/rust_crates/intree",
		"third_party/rust_crates/mirrors":
		return true
	}
	return false
}

func isCustomGolibProject(path string) bool {
	parent := filepath.Dir(path)
	grandparent := filepath.Dir(parent)

	// third_party/golibs/vendor/cloud.google.com/go is a project.
	// Most of the subfolders in that directory are also projects, except for the
	// "internal" subfolder.
	// The easiest way to handle all cases is to explicitly skip creating a project
	// for the internal directory here.
	if path == "third_party/golibs/vendor/cloud.google.com/go/internal" {
		return false
	}

	switch grandparent {
	case "third_party/golibs/vendor/github.com":
		return path != "third_party/golibs/vendor/github.com/googeapis/gax-go"

	case "third_party/golibs/vendor/github.com/googleapis":
		return path == "third_party/golibs/vendor/github.com/googeapis/gax-go/v2"

	case "third_party/golibs/vendor/cloud.google.com",
		"third_party/golibs/vendor/golang.org",
		"third_party/golibs/vendor/gonum.org",
		"third_party/golibs/vendor/golang.opencensus.io":
		return true

	case "third_party/syzkaller/vendor/github.com",
		"third_party/syzkaller/vendor/golang.com",
		"third_party/syzkaller/vendor/honnef.co":
		return true
	}

	switch parent {
	case "third_party/golibs/vendor/google.golang.org",
		"third_party/golibs/vendor/cloud.google.com",
		"third_party/golibs/vendor/go.uber.org",
		"third_party/golibs/vendor/gvisor.dev",
		"third_party/golibs/vendor/gopkg.in":
		return true

	case "third_party/syzkaller/vendor/cloud.google.com",
		"third_party/syzkaller/vendor/golang.org/x",
		"third_party/syzkaller/vendor/google.golang.org":
		return true
	}

	switch path {
	case "third_party/syzkaller/vendor/go.opencensus.io",
		"third_party/golibs/vendor/go.opencensus.io":
		return true
	}

	return false
}
