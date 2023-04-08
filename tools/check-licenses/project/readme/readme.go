// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"bufio"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/file"
)

const (
	singleLicenseFile = "Single License File"
)

var (
	AllReadmes = []*Readme{}

	knownDirectives = map[string]bool{
		"Name":                true,
		"License":             true,
		"License File":        true,
		"License File Format": true,
		"License File URL":    true,
		"Version":             true,
		"Modifications":       true,
		"Local Modifications": true,
		"Description":         true,
		"URL":                 true,
		"Upstream git":        true,
		"Upstream Git":        true,
		"Security Critical":   true,

		// Unused or non-standard
		"License Android Compatible": true,
		"Source":                     true,
		"Short Name":                 true,
		"Git Commit":                 true,
		"Commit":                     true,
		"Revision":                   true,
		"Date":                       true,
		"Deprecated":                 true,

		// DEPRECATED: soft transition
		"check-licenses": true,
		"#License File":  true,
		"LICENSE":        true,
	}
)

// Readme struct follows the format of README.fuchsia files.
// For more info, see the following article:
//
//	https://fuchsia.dev/fuchsia-src/development/source_code/third-party-metadata
type Readme struct {
	Name               string           `json:"name"`
	URL                string           `json:"url"`
	Version            string           `json:"version"`
	Licenses           []*ReadmeLicense `json:"licenseInfo"`
	UpstreamGit        string           `json:"upstreamGit"`
	SecurityCritical   bool             `json:"securityCritical"`
	Description        string           `json:"description"`
	LocalModifications string           `json:"localModifications"`

	// Custom fields for the Fuchsia repository.
	ReadmePath      string `json:"readmePath"`
	RegularFileType file.FileType

	// For Compliance worksheet
	ShouldBeDisplayed  bool
	SourceCodeIncluded bool

	// Logging
	MalformedLines []string `json:"malformedLines"`
}

// Several directives specify information about a given license file.
// Group them together in this ReadmeLicense data structure.
type ReadmeLicense struct {
	License           string     `json:"license"`
	LicenseFilePath   string     `json:"licenseFilePath"`
	LicenseFile       *file.File `json:"licenseFile"`
	LicenseFileFormat string     `json:"licenseFileFormat"`
	LicenseFileURL    string     `json:"licenseFileURL"`
}

// Create a Readme object from a README.* file on the filesystem.
func NewReadmeFromFile(readmePath string) (*Readme, error) {
	if _, err := os.Stat(readmePath); os.IsNotExist(err) {
		return nil, err
	}
	f, err := os.Open(readmePath)
	if err != nil {
		return nil, fmt.Errorf("newReadme(%s): %w\n", readmePath, err)
	}
	defer f.Close()

	return NewReadme(f)
}

// Create a Readme object from a path on the filesystem.
//
// Certain projects in the repo do not currently (and never will) provide
// a README.fuchsia file. This generates a Readme object by using other
// information about the project.
func NewReadmeCustom(projectRoot string) (*Readme, error) {
	switch {
	case strings.Contains(projectRoot, "dart-pkg"):
		return NewDartPkgReadme(projectRoot)
	case strings.Contains(projectRoot, "golibs"):
		return NewGolibReadme(projectRoot)
	case strings.Contains(projectRoot, "rust_crates"):
		return NewRustCrateReadme(projectRoot)
	default:
		return nil, fmt.Errorf("Custom readme generation for project root [%s] is not supported", projectRoot)
	}
}

// NewReadme creates a new Readme object from an io.Reader.
func NewReadme(r io.Reader) (*Readme, error) {
	readme := &Readme{
		Licenses:       make([]*ReadmeLicense, 0),
		MalformedLines: make([]string, 0),
	}

	s := bufio.NewScanner(r)
	s.Split(bufio.ScanLines)

	line := ""
	getNextLine := true

	for {
		if getNextLine {
			if !s.Scan() {
				break
			}
		} else {
			getNextLine = true
		}
		line = s.Text()
		if len(strings.TrimSpace(line)) == 0 {
			continue
		}

		directive, value, err := parseReadmeLine(line)
		if err != nil {
			readme.MalformedLines = append(readme.MalformedLines, line)
			continue
		}

		switch directive {
		case "Name":
			readme.Name = value
		case "Source":
			fallthrough
		case "URL":
			readme.URL = value
		case "Version":
			readme.Version = value
		case "LICENSE":
			fallthrough
		case "License":
			readme.ProcessReadmeLicense(&ReadmeLicense{License: value})
		case "License File Format":
			readme.ProcessReadmeLicense(&ReadmeLicense{LicenseFileFormat: value})
		case "License File":
			readme.ProcessReadmeLicense(&ReadmeLicense{LicenseFilePath: value})
		case "License File URL":
			readme.ProcessReadmeLicense(&ReadmeLicense{LicenseFileURL: value})
		case "Upstream git":
			fallthrough
		case "Upstream Git":
			readme.UpstreamGit = value
		case "Security Critical":
			readme.SecurityCritical = strings.ToLower(value) == "yes"
		case "Description":
			getNextLine = false
			directive, value, readme.Description = parseReadmeMultiLineDirective(s, value)
		case "Modifications":
			fallthrough
		case "Local Modifications":
			getNextLine = false
			directive, value, readme.LocalModifications = parseReadmeMultiLineDirective(s, value)

		// Unused multi-line directives still need to be processed here.
		case "Deprecated":
			getNextLine = false
			parseReadmeMultiLineDirective(s, value)

		// Empty space is OK
		case "":
			// Do nothing.
		}
	}

	AllReadmes = append(AllReadmes, readme)
	return readme, nil
}

// License file directives can be listed in any order.
// e.g. URL info may come before or after information about where the file lives on the filesystem.
// We solve this by merging ReadmeLicense structs together that don't have overlapping information.
func (r *Readme) ProcessReadmeLicense(rl *ReadmeLicense) {
	var last *ReadmeLicense
	l := len(r.Licenses)
	if l > 0 {
		last = r.Licenses[l-1]
	}

	switch {
	case l == 0:
	case rl.License != "" && last.License != "":
	case rl.LicenseFileFormat != "" && last.LicenseFileFormat != "":
	case rl.LicenseFileURL != "" && last.LicenseFileURL != "":
	case rl.LicenseFilePath != "" && last.LicenseFilePath != "":
	default:
		last.License = last.License + rl.License
		last.LicenseFileFormat = last.LicenseFileFormat + rl.LicenseFileFormat
		last.LicenseFileURL = last.LicenseFileURL + rl.LicenseFileURL
		last.LicenseFilePath = last.LicenseFilePath + rl.LicenseFilePath
		return
	}
	r.Licenses = append(r.Licenses, rl)
}

// Parse a single line in a README.fuchsia file.
// Find the colon ':', directives are before that, values are after that.
func parseReadmeLine(line string) (string, string, error) {
	colon := strings.Index(line, ":")
	if colon < 0 {
		return "", line, fmt.Errorf("Failed to find ':' in line '%s'", line)
	}
	directive := strings.TrimSpace(line[:colon])
	value := strings.TrimSpace(line[colon+1:])

	if _, ok := knownDirectives[directive]; !ok {
		return "", line, fmt.Errorf("Unknown directive '%s'", directive)
	}
	return directive, value, nil
}

// Some directives can span multiple lines (e.g. "Description").
// In this case, keep parsing until we find another directive, or we reach the end of the file.
func parseReadmeMultiLineDirective(s *bufio.Scanner, value string) (string, string, string) {
	var err error
	var b strings.Builder
	var line, directive string

	b.WriteString(fmt.Sprintf("%s\n", value))
	for s.Scan() {
		line = s.Text()
		directive, value, err = parseReadmeLine(line)
		if err == nil {
			break
		} else {
			b.WriteString(fmt.Sprintf("%s\n", line))
		}
	}

	return directive, value, b.String()
}

// Quickly list all files in a given directory, recursively.
// This is used to ensure all license files in a custom readme project
// are collected and processed correctly.
func listFilesRecursive(root string) ([]string, error) {
	paths := make([]string, 0)
	err := filepath.WalkDir(root, func(currentPath string, info fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if !info.IsDir() {
			rel, err := filepath.Rel(root, currentPath)
			if err != nil {
				return err
			}
			paths = append(paths, rel)
		}
		return nil
	})

	if err != nil {
		return nil, err
	}
	return paths, nil
}

// Sort the internal fields of the Readme struct, so a readme object can deterministically
// be compared against other readme objects.
func (r *Readme) Sort() {
	sort.Slice(r.Licenses[:], func(i, j int) bool {
		return r.Licenses[i].LicenseFilePath < r.Licenses[j].LicenseFilePath
	})
}
