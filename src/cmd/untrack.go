package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"
)

var untrackYes bool

var untrackCmd = &cobra.Command{
	Use:   "untrack <file|dir>...",
	Short: "Remove files from the storage directory",
	Long: `Remove files or directories from the storage directory.
Untracked files are moved to ~/.local/share/cubby/untracked/ rather than
deleted, preserving the same path structure. To retrack, simply run cubby
on the live file again.`,
	Args: cobra.MinimumNArgs(1),
	RunE: runUntrack,
	ValidArgsFunction: func(cmd *cobra.Command, args []string, toComplete string) ([]string, cobra.ShellCompDirective) {
		return nil, cobra.ShellCompDirectiveDefault
	},
}

func init() {
	untrackCmd.Flags().BoolVarP(&untrackYes, "yes", "y", false, "skip confirmation prompt")
	rootCmd.AddCommand(untrackCmd)
}

func untrackDir() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".local", "share", "cubby", "untracked")
}

func runUntrack(cmd *cobra.Command, args []string) error {
	home, _ := os.UserHomeDir()
	store := resolvedStoreDir()
	cwd, _ := os.Getwd()
	debugf("home=%s store=%s cwd=%s", home, store, cwd)

	for _, target := range args {
		// resolve via cwd like restore does
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
		dst := filepath.Join(untrackDir(), rel)
		debugf("rel=%s src=%s dst=%s", rel, src, dst)

		if _, err := os.Stat(src); os.IsNotExist(err) {
			errorf("%s is not in storage", rel)
			continue
		}

		if !untrackYes {
			if !confirm(fmt.Sprintf("untrack %s?", rel)) {
				continue
			}
		}

		if err := os.MkdirAll(filepath.Dir(dst), 0755); err != nil {
			errorf("could not create untracked directory: %v", err)
			continue
		}

		if err := os.Rename(src, dst); err != nil {
			errorf("could not move %s: %v", rel, err)
			continue
		}

		fmt.Printf("%s  %s  %s\n",
			yellow("↗"),
			bold(rel),
			dim("→  ~/.local/share/cubby/untracked/"+rel),
		)
		logEntry("untrack", rel)
	}

	return nil
}
