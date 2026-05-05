package cmd

import (
	"os"

	"github.com/spf13/cobra"
)

var completionCmd = &cobra.Command{
	Use:    "completion [bash|zsh|fish]",
	Short:  "Generate shell completion script",
	Hidden: true,
	Long: `Generate a shell completion script for cubby.

Bash:
  cubby completion bash > /etc/bash_completion.d/cubby

Zsh:
  cubby completion zsh > "${fpath[1]}/_cubby"

Fish:
  cubby completion fish > ~/.config/fish/completions/cubby.fish
`,
	Args:                   cobra.MatchAll(cobra.ExactArgs(1), cobra.OnlyValidArgs),
	ValidArgs:              []string{"bash", "zsh", "fish"},
	DisableFlagsInUseLine: true,
	RunE: func(cmd *cobra.Command, args []string) error {
		switch args[0] {
		case "zsh":
			return rootCmd.GenZshCompletion(os.Stdout)
		case "bash":
			return rootCmd.GenBashCompletion(os.Stdout)
		case "fish":
			return rootCmd.GenFishCompletion(os.Stdout, true)
		}
		return nil
	},
}
