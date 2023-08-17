// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"fmt"
	"io/ioutil"
	"path/filepath"
	"strings"
)

const (
	dartPkgCustomReadme = "tools/check-licenses/assets/readmes/"
)

// Create an in-memory representation of a new README.fuchsia file
// by inferring info about a Dart package given it's location in the repo.
func NewDartPkgReadme(path string) (*Readme, error) {
	var b builder

	b.setPath(path)
	b.setName(filepath.Base(path))

	url := fmt.Sprintf("https://pub.dev/packages/%s", b.name)
	b.setURL(url)

	// Find all license files for this project.
	// They should all live in the root directory of this project.
	directoryContents, err := ioutil.ReadDir(path)
	if err != nil {
		return nil, err
	}
	for _, item := range directoryContents {
		lower := strings.ToLower(item.Name())
		// In practice, all license files for dart packages either have "COPYING"
		// or "license" in their name.
		if !(strings.Contains(lower, "licen") ||
			strings.Contains(lower, "copying")) {
			continue
		}

		// There are some instances of dart source files and template files
		// that fit the above criteria. Skip those files.
		ext := filepath.Ext(item.Name())
		if ext == ".dart" || ext == ".tmpl" || strings.Contains(lower, "template") {
			continue
		}

		licenseUrl := fmt.Sprintf("%s/license", url)
		b.addLicense(item.Name(), licenseUrl, singleLicenseFile)
	}
	customReadmePath := filepath.Join(dartPkgCustomReadme, path)
	return NewReadme(strings.NewReader(b.build()), path, customReadmePath)
}
