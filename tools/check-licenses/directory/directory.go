// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package directory

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/file"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project/readme"
)

// Directory is an in memory representation of the state of the repository.
type Directory struct {
	Name     string           `json:"name,omitempty"`
	Path     string           `json:"path,omitempty"`
	Files    []*file.File     `json:"files,omitempty"`
	Children []*Directory     `json:"children,omitempty"`
	Parent   *Directory       `json:"-"`
	Project  *project.Project `json:"-"`
}

// Order implements sort.Interface for []*Directory based on the Path field.
type Order []*Directory

func (a Order) Len() int           { return len(a) }
func (a Order) Swap(i, j int)      { a[i], a[j] = a[j], a[i] }
func (a Order) Less(i, j int) bool { return a[i].Path < a[j].Path }

// NewDirectory returns an instance of Directory.
func NewDirectory(root string, parent *Directory) (*Directory, error) {
	return newDirectoryWithConfig(root, parent, Config)
}

// Call NewDirectory with a passed-in config variable (instead of using
// the static Config variable). This allows for easier testing.
func newDirectoryWithConfig(root string, parent *Directory, config *DirectoryConfig) (*Directory, error) {
	d := Directory{}
	if RootDirectory == nil {
		RootDirectory = &d
	}

	d.Name = filepath.Base(root)
	d.Path = root
	d.Parent = parent
	d.Project = project.UnknownProject

	// If we are not at a "barrier" directory (e.g. prebuilt, third_party),
	// then the license info of the parent directory also applies to this directory.
	// Propagate that information down here.
	if !project.IsBarrier(root) && parent != nil {
		d.Project = parent.Project
	}

	if !project.IsBarrier(root) && parent != nil {
		// If we are not at a "barrier" directory (e.g. prebuilt, third_party),
		// then the license info of the parent directory also applies to this directory.
		// Propagate that information down here.
		d.Project = parent.Project
	}

	var r *readme.Readme
	var p *project.Project
	var err error

	// If a README.fuchsia file exists in the current directory, load it.
	if readmePath, exists := readmeFileExists(root); exists {
		if r, err = readme.NewReadmeFromFile(readmePath); err != nil {
			return nil, fmt.Errorf("error loading readme file [%s]: %w\n",
				readmePath, err)
		}
	} else if readmeFileWillNeverExist(root) {
		// Some 3P projects don't have (and never will have) a README.fuchsia file.
		// In those cases, generate an in-memory README file that describes
		// the project.
		if r, err = readme.NewReadmeCustom(root); err != nil {
			return nil, fmt.Errorf("error creating custom readme [%s]: %w\n",
				root, err)
		}
	}

	// If this project was already created during initialization, set it here.
	var ok bool
	if p, ok = project.AllProjects[root]; ok {
		d.Project = p
		r = d.Project.ReadmeFile
	} else if r != nil {
		// Create a new Project using the readme data.
		p, err = project.NewProject(r, root)
		if err != nil {
			return nil, err
		}
		d.Project = p
	}

	directoryContents, err := os.ReadDir(root)
	if err != nil {
		return nil, err
	}
	// Then traverse the rest of the contents of this directory.
	for _, item := range directoryContents {
		path := filepath.Join(root, item.Name())

		// Check the config file to see if we should skip this file / folder.
		if config.shouldSkip(path) {
			plusVal(Skipped, path)
			continue
		}

		// Directories
		if item.IsDir() {
			plus1(NumFolders)
			child, err := newDirectoryWithConfig(path, &d, config)
			if err != nil {
				return nil, err
			}
			d.Children = append(d.Children, child)
			continue
		}

		// Files
		fileType := file.RegularFile
		if file.IsPossibleLicenseFile(path) {
			fileType = file.SingleLicense
		}
		f, err := file.LoadFile(path, fileType, d.Project.Name)
		if err != nil {
			// Likely a symlink issue
			continue
		} else {
			plus1(NumFiles)
			d.Files = append(d.Files, f)
			d.Project.AddFile(f)
		}

		if fileType != file.RegularFile && f.URL() == "" {
			r = d.Project.ReadmeFile
			if r != nil {
				url, err := r.GetLicenseURLForPath(path)
				if err != nil {
					return nil, fmt.Errorf("failed to get URL for file %s: %w\n", path, err)
				}
				f.SetURL(url)
			}
		}
	}

	sort.Sort(Order(d.Children))
	AllDirectories[d.Path] = &d
	return &d, nil
}
