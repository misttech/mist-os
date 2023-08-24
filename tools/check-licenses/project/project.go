// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package project

import (
	"fmt"
	"path/filepath"
	"strings"

	spdx "github.com/spdx/tools-golang/spdx/v2_2"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/file"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project/readme"
)

// Project struct follows the format of README.fuchsia files.
// For more info, see the following article:
//
//	https://fuchsia.dev/fuchsia-src/development/source_code/third-party-metadata
type Project struct {
	Root                   string `json:"root"`
	Name                   string `json:"name"`
	URL                    string
	LicenseFiles           []*file.File `licenseFiles`
	RegularFiles           []*file.File
	SearchableRegularFiles []*file.File
	ReadmeFile             *readme.Readme

	// Projects that this project depends on.
	// Constructed from the GN dependency tree.
	Children map[string]*Project

	// SPDX fields
	Package *spdx.Package "json:'package'"
	SPDXID  string        `json:"spdxid"`

	// Compliance fields.
	BeingSurfaced      bool
	SourceCodeIncluded bool
}

// Order implements sort.Interface for []*Project based on the Root field.
type Order []*Project

func (a Order) Len() int           { return len(a) }
func (a Order) Swap(i, j int)      { a[i], a[j] = a[j], a[i] }
func (a Order) Less(i, j int) bool { return a[i].Root < a[j].Root }

// NewProject creates a Project object from a README.fuchsia file.
func NewProject(r *readme.Readme, projectRootPath string) (*Project, error) {
	var err error

	// Make all projectRootPath values relative to Config.FuchsiaDir.
	if strings.Contains(projectRootPath, Config.FuchsiaDir) {
		projectRootPath, err = filepath.Rel(Config.FuchsiaDir, projectRootPath)
		if err != nil {
			return nil, err
		}
	}

	// See if we've already processed this project.
	// If so, return the previously created instance.
	if _, ok := AllProjects[projectRootPath]; ok {
		plusVal(NumPreviousProjectRetrieved, projectRootPath)
		return AllProjects[projectRootPath], nil
	}

	p := &Project{
		Root:                   projectRootPath,
		Name:                   r.Name,
		URL:                    r.URL,
		ReadmeFile:             r,
		LicenseFiles:           make([]*file.File, 0),
		RegularFiles:           make([]*file.File, 0),
		SearchableRegularFiles: make([]*file.File, 0),
		Children:               make(map[string]*Project, 0),
	}

	for _, l := range r.Licenses {
		if l.LicenseFilePath == "" {
			continue
		}
		if l.LicenseFileFormat == "" {
			l.LicenseFileFormat = "Single License File"
		}

		path := filepath.Join(p.Root, l.LicenseFilePath)
		f, err := file.LoadFile(path, file.FileType(l.LicenseFileFormat), r.Name)
		if err != nil {
			return nil, fmt.Errorf("failed to load license file %s: %w\n", path, err)
		}
		f.SetURL(l.LicenseFileURL)
		p.LicenseFiles = append(p.LicenseFiles, f)
	}

	AllProjects[p.Root] = p
	plusVal(NumProjects, p.Root)

	return p, nil
}

func (p *Project) AddFile(f *file.File) error {
	if f.FileType() != file.RegularFile {
		// This could be a license file.
		// Make sure this file isn't already in the list.
		for _, l := range p.LicenseFiles {
			if l.RelPath() == f.RelPath() {
				return nil
			}
		}

		p.LicenseFiles = append(p.LicenseFiles, f)
		return nil
	}

	p.RegularFiles = append(p.RegularFiles, f)

	ext := filepath.Ext(f.RelPath())
	if _, ok := file.Config.Extensions[ext]; ok {
		p.SearchableRegularFiles = append(p.SearchableRegularFiles, f)
	}

	return nil
}

func getGitInfo(path string) (string, string, error) {
	url, err := git.GetURL(ctx, path)
	if err != nil {
		return "", "", fmt.Errorf("Failed to get git URL for path %s: %w", path, err)
	}

	// Turquoise repos return "sso" urls, so convert them to
	// http links that a user can view in a browser.
	url = strings.ReplaceAll(url, "sso://turquoise-internal", "https://turquoise-internal.googlesource.com")

	// Retrieve the hash URL for the current commit.
	hash, err := git.GetCommitHash(ctx, path)
	if err != nil {
		return "", "", fmt.Errorf("Failed to get git commit hash for path %s: %w", path, err)
	}

	return url, hash, nil
}
