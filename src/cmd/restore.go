package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"
)

var (
	restoreYes    bool
	restoreDryRun bool
)

var restoreCmd = &cobra.Command{
	Use:   "restore [file|dir]...",
	Short: "Restore files from your storage directory",
	Long: `Restore files and directories from your storage directory back to
their original locations under $HOME.

If no arguments are provided, cubby will scan all tracked files and restore
any that are missing or modified in your home directory from the storage
directory.

Paths are resolved relative to your current working directory, mirrored into
the storage directory.`,
	Args: cobra.ArbitraryArgs,
	RunE: runRestore,
	ValidArgsFunction: func(cmd *cobra.Command, args []string, toComplete string) ([]string, cobra.ShellCompDirective) {
		return nil, cobra.ShellCompDirectiveDefault
	},
}

func init() {
	restoreCmd.Flags().BoolVarP(&restoreYes, "yes", "y", false, "skip overwrite/confirmation prompts")
	restoreCmd.Flags().BoolVarP(&restoreDryRun, "dry-run", "n", false, "simulate without making changes")
}

func runRestore(cmd *cobra.Command, args []string) error {
	home, _ := os.UserHomeDir()
	store := resolvedStoreDir()
	cwd, _ := os.Getwd()
	debugf("home=%s store=%s cwd=%s", home, store, cwd)

	if len(args) == 0 {
		return runRestoreAll(home, store)
	}

	for _, target := range args {
		abs, err := filepath.Abs(filepath.Join(cwd, target))
		if err != nil {
			errorf("cannot resolve %s: %v", target, err)
			continue
		}

		if !strings.HasPrefix(abs, home+string(filepath.Separator)) {
			errorf("%s is outside $HOME", abs)
			continue
		}

		rel := strings.TrimPrefix(abs, home+string(filepath.Separator))
		src := filepath.Join(store, rel)
		dst := abs
		debugf("target=%s abs=%s rel=%s src=%s dst=%s", target, abs, rel, src, dst)

		info, err := os.Stat(src)
		if err != nil {
			errorf("%s not found in storage directory", rel)
			continue
		}

		if info.IsDir() {
			if restoreDryRun {
				printDryRunHeader(store+"/"+rel, "~/"+rel)
				syncDir(src, dst, true)
				continue
			}
			if err := os.MkdirAll(dst, 0755); err != nil {
				errorf("%v", err)
				continue
			}
			if err := syncDir(src, dst, false); err != nil {
				errorf("restore failed for %s: %v", target, err)
				continue
			}
		} else {
			if restoreDryRun {
				printDryRunHeader(store+"/"+rel, "~/"+rel)
				dryRunFile(src, dst)
				continue
			}
			if _, err := os.Stat(dst); err == nil && !restoreYes {
				if !confirm(fmt.Sprintf("overwrite ~/%s?", rel)) {
					continue
				}
			}
			if err := os.MkdirAll(filepath.Dir(dst), 0755); err != nil {
				errorf("%v", err)
				continue
			}
			if err := copyFile(src, dst); err != nil {
				errorf("restore failed for %s: %v", target, err)
				continue
			}
		}

		printRestore(rel, store)
		logEntry("restore", rel)
	}

	return nil
}

func runRestoreAll(home, store string) error {
	if _, err := os.Stat(store); os.IsNotExist(err) {
		errorf("storage directory not found: %s", store)
		return nil
	}

	// collect modified files using same logic as status
	var toRestore []statusEntry

	err := filepath.Walk(store, func(storePath string, info os.FileInfo, err error) error {
		if err != nil {
			return nil
		}

		rel, _ := filepath.Rel(store, storePath)
		if rel == "." || isIgnored(rel) {
			if info.IsDir() && rel != "." {
				return filepath.SkipDir
			}
			return nil
		}

		if info.IsDir() {
			return nil
		}

		livePath := filepath.Join(home, rel)
		liveInfo, liveErr := os.Stat(livePath)
		_ = liveInfo

		if os.IsNotExist(liveErr) {
			// missing from live — also restore these
			toRestore = append(toRestore, statusEntry{rel: rel, status: statusMissing})
			return nil
		}

		storeHash, err1 := hashFile(storePath)
		liveHash, err2 := hashFile(livePath)
		if err1 != nil || err2 != nil {
			debugf("could not hash %s", rel)
			return nil
		}

		if string(storeHash) != string(liveHash) {
			toRestore = append(toRestore, statusEntry{rel: rel, status: statusModified})
		}

		return nil
	})

	if err != nil {
		errorf("error walking storage directory: %v", err)
		return nil
	}

	if len(toRestore) == 0 {
		fmt.Printf("%s  %s\n", green("✓"), dim("everything in sync, nothing to restore"))
		return nil
	}

	// preview what will be restored
	fmt.Printf("\n%s\n", bold("files to restore:"))
	for _, e := range toRestore {
		label := "modified"
		if e.status == statusMissing {
			label = "missing"
		}
		fmt.Printf("  %s  %s  %s\n", cyan("↓"), e.rel, dim("("+label+")"))
	}
	fmt.Println()

	if restoreDryRun {
		fmt.Printf("%s\n", dim(fmt.Sprintf("%d files would be restored", len(toRestore))))
		return nil
	}

	if !restoreYes {
		if !confirm(fmt.Sprintf("restore %d files from storage?", len(toRestore))) {
			fmt.Println(dim("aborted."))
			return nil
		}
	}

	// restore each file
	errors := 0
	for _, e := range toRestore {
		src := filepath.Join(store, e.rel)
		dst := filepath.Join(home, e.rel)

		if err := os.MkdirAll(filepath.Dir(dst), 0755); err != nil {
			errorf("could not create directory for %s: %v", e.rel, err)
			errors++
			continue
		}
		if err := copyFile(src, dst); err != nil {
			errorf("restore failed for %s: %v", e.rel, err)
			errors++
			continue
		}
		printRestore(e.rel, store)
		logEntry("restore", e.rel)
	}

	fmt.Printf("\n%s\n", dim(fmt.Sprintf(
		"%d restored  ·  %d errors",
		len(toRestore)-errors, errors,
	)))

	return nil
}
