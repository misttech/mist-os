// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"fmt"
	"path/filepath"
	"strings"

	"github.com/BurntSushi/toml"
)

type (
	// Represents Cargo.toml files found in rust crates across the repo.
	CargoTomlFile struct {
		Package CargoTomlPackage
	}

	// Represents the "Package" table inside of each Cargo.toml file.
	CargoTomlPackage struct {
		Name       string `toml:"name"`
		Version    string `toml:"version"`
		Repository string `toml:"repository"`
	}
)

// Create an in-memory representation of a new README.fuchsia file
// using data pulled from the Cargo.toml file of the given Rust crate.
func NewRustCrateReadme(path string) (*Readme, error) {
	var b builder
	var cargo CargoTomlFile

	_, err := toml.DecodeFile(filepath.Join(path, "Cargo.toml"), &cargo)
	if err != nil {
		return nil, fmt.Errorf("Failed to decode Cargo.toml file for project %s: %w", path, err)
	}

	b.setName(cargo.Package.Name)
	b.setURL(cargo.Package.Repository)
	b.setVersion(cargo.Package.Version)

	// Find all license files for this project.
	// They should all live in the root directory of this project.
	directoryContents, err := listFilesRecursive(path)
	if err != nil {
		return nil, err
	}
	for _, item := range directoryContents {
		lower := strings.ToLower(item)
		// In practice, all license files for rust projects are either named
		// COPYING or LICENSE
		if !(strings.Contains(lower, "licen") ||
			strings.Contains(lower, "copying")) {
			continue
		}

		// There are some instances of rust source files and template files
		// that fit the above criteria. Skip those files.
		ext := filepath.Ext(item)
		if ext == ".rs" || ext == ".tmpl" || strings.Contains(lower, "template") {
			continue
		}

		licenseUrl := fmt.Sprintf("%s/%s", cargo.Package.Repository, item)
		b.addLicense(item, licenseUrl, singleLicenseFile)
	}

	return NewReadme(strings.NewReader(b.build()))
}
