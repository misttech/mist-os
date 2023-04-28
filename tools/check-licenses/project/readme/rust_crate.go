// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"context"
	"fmt"
	"io/ioutil"
	"path/filepath"
	"strings"

	"github.com/BurntSushi/toml"
)

const (
	rustCrateURLPrefix = "https://fuchsia.googlesource.com/fuchsia"

	rustCrateEmptyRootDir     = "third_party/rust_crates/empty"
	rustCrateEmptyLicenseFile = "../../../../LICENSE"
	rustCrateEmptyLicenseURL  = "https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/LICENSE"
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
	var err error

	hash, err := git.GetCommitHash(context.Background(), path)
	if err != nil {
		return nil, err
	}
	name := filepath.Base(path)
	parentName := filepath.Base(filepath.Dir(path))
	url := fmt.Sprintf("%s/+/%s/third_party/rust_crates/%s/%s", rustCrateURLPrefix, hash, parentName, name)

	b.setPath(path)
	b.setName(name)
	b.setURL(url)

	// Find all license files for this project.
	// They should all live in the root directory of this project.
	directoryContents, err := ioutil.ReadDir(path)
	if err != nil {
		return nil, err
	}
	for _, item := range directoryContents {
		lower := strings.ToLower(item.Name())
		// In practice, all license files for rust projects are either named
		// COPYING or LICENSE
		if !(strings.Contains(lower, "licen") ||
			strings.Contains(lower, "copying")) {
			continue
		}

		// There are some instances of rust source files and template files
		// that fit the above criteria. Skip those files.
		ext := filepath.Ext(item.Name())
		if ext == ".rs" || ext == ".tmpl" || strings.Contains(lower, "template") {
			continue
		}

		licenseUrl := fmt.Sprintf("%s/%s", url, item.Name())
		b.addLicense(item.Name(), licenseUrl, singleLicenseFile)
	}

	parentPath := filepath.Dir(path)
	if strings.HasSuffix(parentPath, rustCrateEmptyRootDir) {
		b.addLicense(rustCrateEmptyLicenseFile, rustCrateEmptyLicenseURL, singleLicenseFile)
	}
	return NewReadme(strings.NewReader(b.build()), path)
}

func loadRustCrateTomlFields(b builder, path string) error {
	var cargo CargoTomlFile

	_, err := toml.DecodeFile(filepath.Join(path, "Cargo.toml"), &cargo)
	if err != nil {
		return fmt.Errorf("Failed to decode Cargo.toml file for project %s: %w", path, err)
	}

	b.setName(cargo.Package.Name)
	b.setURL(cargo.Package.Repository)
	b.setVersion(cargo.Package.Version)

	return nil
}
