// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package project

import (
	"path/filepath"
)

var Config *ProjectConfig

func init() {
	Config = &ProjectConfig{}
}

type ProjectConfig struct {
	FuchsiaDir string `json:"fuchsiaDir"`
	BuildDir   string `json:"buildDir"`

	GnPath              string `json:"gnPath"`
	GenProjectFile      string `json:"genProjectFile"`
	GenIntermediateFile string `json:"genIntermediateFile"`
	Target              string `json:"target"`

	OutputLicenseFile bool `json:"outputLicenseFile"`

	// Paths to temporary directories holding README.fuchsia files.
	// These files will eventually migrate to their correct locations in
	// the Fuchsia repository.
	Readmes []*Readme `json:"readmes"`

	// Keywords signifying where the license information for one project
	// ends, and the license info for another project begins.
	// (e.g. "third_party")
	Barriers []*Barrier `json:"barriers"`

	// The strings in this map match targets that are found in $root_build_dir/project.json.
	// These targets will be filtered out during project filtering.
	PruneTargets map[string]bool `json:"pruneTargets"`
}

type Readme struct {
	Paths []string `json:"paths"`
	Notes []string `json:"notes"`
}

type Barrier struct {
	Paths      []string `json:"paths"`
	Exceptions []string `json:"exceptions"`
	Notes      []string `json:"notes"`
}

// IsBarrier returns true if the given path is a part of the parent project.
// For example, directories under //third_party are independent projects that
// the parent Fuchsia license file may not apply to.
//
// These "barrier" directories are set in the config file.
func IsBarrier(path string) bool {
	base := filepath.Base(path)
	isBarrier := false
	for _, barrier := range Config.Barriers {
		for _, bPath := range barrier.Paths {
			if base == bPath {
				isBarrier = true
				break
			}
		}
		for _, ePath := range barrier.Exceptions {
			if ePath == path {
				isBarrier = false
				break
			}
		}
	}
	return isBarrier
}

func NewConfig() *ProjectConfig {
	return &ProjectConfig{
		Readmes:      make([]*Readme, 0),
		Barriers:     make([]*Barrier, 0),
		PruneTargets: make(map[string]bool, 0),
	}
}

func (c *ProjectConfig) Merge(other *ProjectConfig) {
	if c.FuchsiaDir == "" {
		c.FuchsiaDir = other.FuchsiaDir
	}
	if c.GnPath == "" {
		c.GnPath = other.GnPath
	}
	if c.GenIntermediateFile == "" {
		c.GenIntermediateFile = other.GenIntermediateFile
	}
	if c.GenProjectFile == "" {
		c.GenProjectFile = other.GenProjectFile
	}
	if c.Target == "" {
		c.Target = other.Target
	}
	if c.BuildDir == "" {
		c.BuildDir = other.BuildDir
	}

	c.Readmes = append(c.Readmes, other.Readmes...)
	c.Barriers = append(c.Barriers, other.Barriers...)
	c.OutputLicenseFile = c.OutputLicenseFile || other.OutputLicenseFile
	c.OutputLicenseFile = c.OutputLicenseFile || other.OutputLicenseFile

	for k, v := range other.PruneTargets {
		c.PruneTargets[k] = v
	}

	// Barrier objects need to be merged together,
	// otherwise the exceptions paths may not work properly.
	mergedBarrier := &Barrier{
		Paths:      make([]string, 0),
		Exceptions: make([]string, 0),
		Notes:      make([]string, 0),
	}
	for _, b := range c.Barriers {
		for _, p := range b.Paths {
			mergedBarrier.Paths = append(mergedBarrier.Paths, p)
		}
		for _, e := range b.Exceptions {
			mergedBarrier.Exceptions = append(mergedBarrier.Exceptions, e)
		}
		for _, n := range b.Notes {
			mergedBarrier.Notes = append(mergedBarrier.Notes, n)
		}
	}
	c.Barriers = []*Barrier{mergedBarrier}
}
