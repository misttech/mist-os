// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package project

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/file"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/license"
)

const (
	urlPrefixGo = "https://fuchsia.googlesource.com/fuchsia/+/%s/%s"
)

// NewSpecialProject creates a Project object from a directory.
//
// There are hundreds of rust_crate projects that don't contain a README.fuchsia file.
// The same goes for golibs and dart_pkg project. We can't expect these 3p projects
// to maintain a README file specifically for fuchsia license detection.
//
// We handle them special here by automatically creating a project object based at the
// root of the project. This assumes all license files are named LICENSE.* or LICENCE.*
// which, in practice, has been correct.
func NewSpecialProject(projectRootPath string) (*Project, error) {
	var err error
	licenseFilePaths := make([]string, 0)

	projectName := filepath.Base(projectRootPath)

	directoryContents, err := os.ReadDir(projectRootPath)
	if err != nil {
		return nil, err
	}

	for _, item := range directoryContents {
		if item.IsDir() {
			continue
		}

		// Skip lists are defined in the Directory package.
		// That package depends on this one, so I cannot access
		// that list from this package (dependency cycle).
		// TODO(jcecil): Move the skip list into a location that both packages can access.
		if strings.Contains(item.Name(), "other_license.go") { // spdx/tools-golang/spdx/v2_2/other_license.go
			continue
		}

		if strings.Contains(strings.ToLower(item.Name()), "licen") &&
			!strings.Contains(strings.ToLower(item.Name()), "tmpl") &&
			filepath.Ext(item.Name()) != ".go" {
			licenseFilePaths = append(licenseFilePaths, item.Name())
		}
	}

	if len(licenseFilePaths) == 0 {
		return nil, os.ErrNotExist
	}

	p := &Project{
		Name:                            projectName,
		Root:                            projectRootPath,
		LicenseFileType:                 file.SingleLicense,
		RegularFileType:                 file.Any,
		ShouldBeDisplayed:               true,
		SourceCodeIncluded:              false,
		Children:                        make(map[string]*Project, 0),
		LicenseFileSearchResultsDeduped: make(map[string]*license.SearchResult, 0),
	}

	switch {
	case strings.Contains(p.Root, "dart-pkg"):
		p.URL = fmt.Sprintf("https://pub.dev/packages/%v", projectName)

	case strings.Contains(p.Root, "golibs"):
		if hash, err := git.GetCommitHash(ctx, p.Root); err != nil {
			return nil, fmt.Errorf("Failed to get git hash for path %s: %w", p.Root, err)
		} else {
			p.URL = fmt.Sprintf(urlPrefixGo, hash, p.Root)
		}

	case strings.Contains(p.Root, "rust_crates"):
		url, err := git.GetURL(ctx, p.Root)
		if err != nil {
			return nil, err
		}
		hash, err := git.GetCommitHash(ctx, p.Root)
		if err != nil {
			return nil, err
		}
		p.URL = fmt.Sprintf("%v/+/%v", url, hash)
	}

	for _, l := range licenseFilePaths {
		l = filepath.Join(p.Root, l)
		l = filepath.Clean(l)

		licenseFile, err := file.NewFile(l, p.LicenseFileType, p.Name)
		if err != nil {
			return nil, err
		}
		p.LicenseFile = append(p.LicenseFile, licenseFile)

		switch {
		case strings.Contains(l, "dart-pkg"):
			licenseFile.URL = fmt.Sprintf("%v/license", p.URL)

		case strings.Contains(l, "golibs"):
			relPath, err := filepath.Rel(p.Root, licenseFile.RelPath)
			if err != nil {
				return nil, err
			}
			licenseFile.URL = fmt.Sprintf("%s/%s", p.URL, relPath)

		case strings.Contains(l, "rust_crates"):
			rel := l
			if strings.Contains(l, Config.FuchsiaDir) {
				rel, _ = filepath.Rel(Config.FuchsiaDir, l)
			}
			licenseFile.URL = fmt.Sprintf("https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/%v", rel)
		}
	}

	plusVal(NumProjects, p.Root)
	plusVal(ProjectURLs, fmt.Sprintf("%v - %v", p.Root, p.URL))
	AllProjects[p.Root] = p

	if err := p.setSPDXFields(); err != nil {
		return nil, err
	}

	return p, nil
}
