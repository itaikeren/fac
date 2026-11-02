// From aider --help and the help of each subcommand, aider 0.86.2, 2026-11-02.
export default {
  name: "aider",
  options: [
    {name: ["-h", "--help"], description: "show this help message and exit"},
    {
      name: "--model",
      description: "Specify the model to use for the main chat",
      args: {name: "model"},
    },
    {
      name: "--openai-api-key",
      description: "Specify the OpenAI API key",
      args: {name: "openai_api_key"},
    },
    {
      name: "--anthropic-api-key",
      description: "Specify the Anthropic API key",
      args: {name: "anthropic_api_key"},
    },
    {
      name: "--openai-api-base",
      description: "Specify the api base url",
      args: {name: "openai_api_base"},
    },
    {
      name: "--openai-api-type",
      description: "(deprecated, use --set-env OPENAI_API_TYPE=<value>)",
      args: {name: "openai_api_type"},
    },
    {
      name: "--openai-api-version",
      description: "(deprecated, use --set-env OPENAI_API_VERSION=<value>)",
      args: {name: "openai_api_version"},
    },
    {
      name: "--openai-api-deployment-id",
      description: "(deprecated, use --set-env OPENAI_API_DEPLOYMENT_ID=<value>)",
      args: {name: "openai_api_deployment_id"},
    },
    {
      name: "--openai-organization-id",
      description: "(deprecated, use --set-env OPENAI_ORGANIZATION=<value>)",
      args: {name: "openai_organization_id"},
    },
    {
      name: "--set-env",
      description: "Set an environment variable (to control API settings, can be used multiple times)",
      args: {name: "ENV_VAR_NAME=value"},
      isRepeatable: true,
    },
    {
      name: "--api-key",
      description: "Set an API key for a provider (eg: --api-key provider=<key> sets PROVIDER_API_KEY=<key>)",
      args: {name: "PROVIDER=KEY"},
    },
    {
      name: ["--list-models", "--models"],
      description: "List known models which match the (partial) MODEL name",
      args: {name: "model"},
    },
    {
      name: "--model-settings-file",
      description: "Specify a file with aider model settings for unknown models",
      args: {name: "model_settings_file", template: "filepaths"},
    },
    {
      name: "--model-metadata-file",
      description: "Specify a file with context window and costs for unknown models",
      args: {name: "model_metadata_file", template: "filepaths"},
    },
    {
      name: "--alias",
      description: "Add a model alias (can be used multiple times)",
      args: {name: "alias:model"},
      isRepeatable: true,
    },
    {
      name: "--reasoning-effort",
      description: "Set the reasoning_effort API parameter (default: not set)",
      args: {name: "reasoning_effort"},
    },
    {
      name: "--thinking-tokens",
      description: "Set the thinking token budget for models that support it. Use 0 to disable. (default: not set)",
      args: {name: "thinking_tokens"},
    },
    {
      name: "--verify-ssl",
      description: "Verify the SSL cert when connecting to models (default: True)",
      exclusiveOn: ["--no-verify-ssl"],
    },
    {
      name: "--no-verify-ssl",
      description: "Verify the SSL cert when connecting to models (default: True)",
      exclusiveOn: ["--verify-ssl"],
    },
    {
      name: "--timeout",
      description: "Timeout in seconds for API calls (default: None)",
      args: {name: "timeout"},
    },
    {
      name: ["--edit-format", "--chat-mode"],
      description: "Specify what edit format the LLM should use (default depends on model)",
      args: {name: "edit_format"},
    },
    {name: "--architect", description: "Use architect edit format for the main chat"},
    {
      name: "--auto-accept-architect",
      description: "Enable/disable automatic acceptance of architect changes (default: True)",
      exclusiveOn: ["--no-auto-accept-architect"],
    },
    {
      name: "--no-auto-accept-architect",
      description: "Enable/disable automatic acceptance of architect changes (default: True)",
      exclusiveOn: ["--auto-accept-architect"],
    },
    {
      name: "--weak-model",
      description: "Specify the model to use for commit messages and chat history summarization (default depends on --model)",
      args: {name: "weak_model"},
    },
    {
      name: "--editor-model",
      description: "Specify the model to use for editor tasks (default depends on --model)",
      args: {name: "editor_model"},
    },
    {
      name: "--editor-edit-format",
      description: "Specify the edit format for the editor model (default: depends on editor model)",
      args: {name: "editor_edit_format"},
    },
    {
      name: "--show-model-warnings",
      description: "Only work with models that have meta-data available (default: True)",
      exclusiveOn: ["--no-show-model-warnings"],
    },
    {
      name: "--no-show-model-warnings",
      description: "Only work with models that have meta-data available (default: True)",
      exclusiveOn: ["--show-model-warnings"],
    },
    {
      name: "--check-model-accepts-settings",
      description: "Check if model accepts settings like reasoning_effort/thinking_tokens (default: True)",
      exclusiveOn: ["--no-check-model-accepts-settings"],
    },
    {
      name: "--no-check-model-accepts-settings",
      description: "Check if model accepts settings like reasoning_effort/thinking_tokens (default: True)",
      exclusiveOn: ["--check-model-accepts-settings"],
    },
    {
      name: "--max-chat-history-tokens",
      description: "Soft limit on tokens for chat history, after which summarization begins",
      args: {name: "max_chat_history_tokens"},
    },
    {
      name: "--cache-prompts",
      description: "Enable caching of prompts (default: False)",
      exclusiveOn: ["--no-cache-prompts"],
    },
    {
      name: "--no-cache-prompts",
      description: "Enable caching of prompts (default: False)",
      exclusiveOn: ["--cache-prompts"],
    },
    {
      name: "--cache-keepalive-pings",
      description: "Number of times to ping at 5min intervals to keep prompt cache warm (default: 0)",
      args: {name: "cache_keepalive_pings"},
    },
    {
      name: "--map-tokens",
      description: "Suggested number of tokens to use for repo map, use 0 to disable",
      args: {name: "map_tokens"},
    },
    {
      name: "--map-refresh",
      description: "Control how often the repo map is refreshed. Options: auto, always, files, manual (default: auto)",
      args: {name: "map-refresh", suggestions: ["auto", "always", "files", "manual"]},
    },
    {
      name: "--map-multiplier-no-files",
      description: "Multiplier for map tokens when no files are specified (default: 2)",
      args: {name: "map_multiplier_no_files"},
    },
    {
      name: "--input-history-file",
      description: "Specify the chat input history file (default: .aider.input.history)",
      args: {name: "input_history_file", template: "filepaths"},
    },
    {
      name: "--chat-history-file",
      description: "Specify the chat history file (default: .aider.chat.history.md)",
      args: {name: "chat_history_file", template: "filepaths"},
    },
    {
      name: "--restore-chat-history",
      description: "Restore the previous chat history messages (default: False)",
      exclusiveOn: ["--no-restore-chat-history"],
    },
    {
      name: "--no-restore-chat-history",
      description: "Restore the previous chat history messages (default: False)",
      exclusiveOn: ["--restore-chat-history"],
    },
    {
      name: "--llm-history-file",
      description: "Log the conversation with the LLM to this file (for example, .aider.llm.history)",
      args: {name: "llm_history_file", template: "filepaths"},
    },
    {
      name: "--dark-mode",
      description: "Use colors suitable for a dark terminal background (default: False)",
    },
    {
      name: "--light-mode",
      description: "Use colors suitable for a light terminal background (default: False)",
    },
    {
      name: "--pretty",
      description: "Enable/disable pretty, colorized output (default: True)",
      exclusiveOn: ["--no-pretty"],
    },
    {
      name: "--no-pretty",
      description: "Enable/disable pretty, colorized output (default: True)",
      exclusiveOn: ["--pretty"],
    },
    {
      name: "--stream",
      description: "Enable/disable streaming responses (default: True)",
      exclusiveOn: ["--no-stream"],
    },
    {
      name: "--no-stream",
      description: "Enable/disable streaming responses (default: True)",
      exclusiveOn: ["--stream"],
    },
    {
      name: "--user-input-color",
      description: "Set the color for user input (default: #00cc00)",
      args: {name: "user_input_color"},
    },
    {
      name: "--tool-output-color",
      description: "Set the color for tool output (default: None)",
      args: {name: "tool_output_color"},
    },
    {
      name: "--tool-error-color",
      description: "Set the color for tool error messages (default: #FF2222)",
      args: {name: "tool_error_color"},
    },
    {
      name: "--tool-warning-color",
      description: "Set the color for tool warning messages (default: #FFA500)",
      args: {name: "tool_warning_color"},
    },
    {
      name: "--assistant-output-color",
      description: "Set the color for assistant output (default: #0088ff)",
      args: {name: "assistant_output_color"},
    },
    {
      name: "--completion-menu-color",
      description: "Set the color for the completion menu (default: terminal's default text color)",
      args: {name: "color"},
    },
    {
      name: "--completion-menu-bg-color",
      description: "Set the background color for the completion menu (default: terminal's default background color)",
      args: {name: "color"},
    },
    {
      name: "--completion-menu-current-color",
      description: "Set the color for the current item in the completion menu (default: terminal's default background color)",
      args: {name: "color"},
    },
    {
      name: "--completion-menu-current-bg-color",
      description: "Set the background color for the current item in the completion menu (default: terminal's default text color)",
      args: {name: "color"},
    },
    {
      name: "--code-theme",
      description: "Set the markdown code theme (default",
      args: {name: "code_theme"},
    },
    {name: "--show-diffs", description: "Show diffs when committing changes (default: False)"},
    {
      name: "--git",
      description: "Enable/disable looking for a git repo (default: True)",
      exclusiveOn: ["--no-git"],
    },
    {
      name: "--no-git",
      description: "Enable/disable looking for a git repo (default: True)",
      exclusiveOn: ["--git"],
    },
    {
      name: "--gitignore",
      description: "Enable/disable adding .aider* to .gitignore (default: True)",
      exclusiveOn: ["--no-gitignore"],
    },
    {
      name: "--no-gitignore",
      description: "Enable/disable adding .aider* to .gitignore (default: True)",
      exclusiveOn: ["--gitignore"],
    },
    {
      name: "--add-gitignore-files",
      description: "Enable/disable the addition of files listed in .gitignore to Aider's editing scope.",
      exclusiveOn: ["--no-add-gitignore-files"],
    },
    {
      name: "--no-add-gitignore-files",
      description: "Enable/disable the addition of files listed in .gitignore to Aider's editing scope.",
      exclusiveOn: ["--add-gitignore-files"],
    },
    {
      name: "--aiderignore",
      description: "Specify the aider ignore file (default: .aiderignore in git root)",
      args: {name: "aiderignore", template: "filepaths"},
    },
    {
      name: "--subtree-only",
      description: "Only consider files in the current subtree of the git repository",
    },
    {
      name: "--auto-commits",
      description: "Enable/disable auto commit of LLM changes (default: True)",
      exclusiveOn: ["--no-auto-commits"],
    },
    {
      name: "--no-auto-commits",
      description: "Enable/disable auto commit of LLM changes (default: True)",
      exclusiveOn: ["--auto-commits"],
    },
    {
      name: "--dirty-commits",
      description: "Enable/disable commits when repo is found dirty (default: True)",
      exclusiveOn: ["--no-dirty-commits"],
    },
    {
      name: "--no-dirty-commits",
      description: "Enable/disable commits when repo is found dirty (default: True)",
      exclusiveOn: ["--dirty-commits"],
    },
    {
      name: "--attribute-author",
      description: "Attribute aider code changes in the git author name (default: True)",
      exclusiveOn: ["--no-attribute-author"],
    },
    {
      name: "--no-attribute-author",
      description: "Attribute aider code changes in the git author name (default: True)",
      exclusiveOn: ["--attribute-author"],
    },
    {
      name: "--attribute-committer",
      description: "Attribute aider commits in the git committer name (default: True)",
      exclusiveOn: ["--no-attribute-committer"],
    },
    {
      name: "--no-attribute-committer",
      description: "Attribute aider commits in the git committer name (default: True)",
      exclusiveOn: ["--attribute-committer"],
    },
    {
      name: "--attribute-commit-message-author",
      description: "Prefix commit messages with 'aider: ' if aider authored the changes (default: False)",
      exclusiveOn: ["--no-attribute-commit-message-author"],
    },
    {
      name: "--no-attribute-commit-message-author",
      description: "Prefix commit messages with 'aider: ' if aider authored the changes (default: False)",
      exclusiveOn: ["--attribute-commit-message-author"],
    },
    {
      name: "--attribute-commit-message-committer",
      description: "Prefix all commit messages with 'aider: ' (default: False)",
      exclusiveOn: ["--no-attribute-commit-message-committer"],
    },
    {
      name: "--no-attribute-commit-message-committer",
      description: "Prefix all commit messages with 'aider: ' (default: False)",
      exclusiveOn: ["--attribute-commit-message-committer"],
    },
    {
      name: "--attribute-co-authored-by",
      description: "Attribute aider edits using the Co-authored-by trailer in the commit message (default: True)",
      exclusiveOn: ["--no-attribute-co-authored-by"],
    },
    {
      name: "--no-attribute-co-authored-by",
      description: "Attribute aider edits using the Co-authored-by trailer in the commit message (default: True)",
      exclusiveOn: ["--attribute-co-authored-by"],
    },
    {
      name: "--git-commit-verify",
      description: "Enable/disable git pre-commit hooks with --no-verify (default: False)",
      exclusiveOn: ["--no-git-commit-verify"],
    },
    {
      name: "--no-git-commit-verify",
      description: "Enable/disable git pre-commit hooks with --no-verify (default: False)",
      exclusiveOn: ["--git-commit-verify"],
    },
    {
      name: "--commit",
      description: "Commit all pending changes with a suitable commit message, then exit",
    },
    {
      name: "--commit-prompt",
      description: "Specify a custom prompt for generating commit messages",
      args: {name: "prompt"},
    },
    {
      name: "--dry-run",
      description: "Perform a dry run without modifying files (default: False)",
      exclusiveOn: ["--no-dry-run"],
    },
    {
      name: "--no-dry-run",
      description: "Perform a dry run without modifying files (default: False)",
      exclusiveOn: ["--dry-run"],
    },
    {
      name: "--skip-sanity-check-repo",
      description: "Skip the sanity check for the git repository (default: False)",
    },
    {
      name: "--watch-files",
      description: "Enable/disable watching files for ai coding comments (default: False)",
      exclusiveOn: ["--no-watch-files"],
    },
    {
      name: "--no-watch-files",
      description: "Enable/disable watching files for ai coding comments (default: False)",
      exclusiveOn: ["--watch-files"],
    },
    {name: "--lint", description: "Lint and fix provided files, or dirty files if none provided"},
    {
      name: "--lint-cmd",
      description: "Specify lint commands to run for different languages, eg",
      args: {name: "lint_cmd"},
      isRepeatable: true,
    },
    {
      name: "--auto-lint",
      description: "Enable/disable automatic linting after changes (default: True)",
      exclusiveOn: ["--no-auto-lint"],
    },
    {
      name: "--no-auto-lint",
      description: "Enable/disable automatic linting after changes (default: True)",
      exclusiveOn: ["--auto-lint"],
    },
    {name: "--test-cmd", description: "Specify command to run tests", args: {name: "test_cmd"}},
    {
      name: "--auto-test",
      description: "Enable/disable automatic testing after changes (default: False)",
      exclusiveOn: ["--no-auto-test"],
    },
    {
      name: "--no-auto-test",
      description: "Enable/disable automatic testing after changes (default: False)",
      exclusiveOn: ["--auto-test"],
    },
    {name: "--test", description: "Run tests, fix problems found and then exit"},
    {
      name: "--analytics",
      description: "Enable/disable analytics for current session (default: random)",
      exclusiveOn: ["--no-analytics"],
    },
    {
      name: "--no-analytics",
      description: "Enable/disable analytics for current session (default: random)",
      exclusiveOn: ["--analytics"],
    },
    {
      name: "--analytics-log",
      description: "Specify a file to log analytics events",
      args: {name: "analytics_log_file", template: "filepaths"},
    },
    {name: "--analytics-disable", description: "Permanently disable analytics"},
    {
      name: "--analytics-posthog-host",
      description: "Send analytics to custom PostHog instance",
      args: {name: "analytics_posthog_host"},
    },
    {
      name: "--analytics-posthog-project-api-key",
      description: "Send analytics to custom PostHog project",
      args: {name: "analytics_posthog_project_api_key"},
    },
    {
      name: "--just-check-update",
      description: "Check for updates and return status in the exit code",
    },
    {
      name: "--check-update",
      description: "Check for new aider versions on launch",
      exclusiveOn: ["--no-check-update"],
    },
    {
      name: "--no-check-update",
      description: "Check for new aider versions on launch",
      exclusiveOn: ["--check-update"],
    },
    {
      name: "--show-release-notes",
      description: "Show release notes on first run of new version (default: None, ask user)",
      exclusiveOn: ["--no-show-release-notes"],
    },
    {
      name: "--no-show-release-notes",
      description: "Show release notes on first run of new version (default: None, ask user)",
      exclusiveOn: ["--show-release-notes"],
    },
    {name: "--install-main-branch", description: "Install the latest version from the main branch"},
    {name: ["--upgrade", "--update"], description: "Upgrade aider to the latest version from PyPI"},
    {name: "--version", description: "Show the version number and exit"},
    {
      name: ["--message", "--msg", "-m"],
      description: "Specify a single message to send the LLM, process reply then exit (disables chat mode)",
      args: {name: "message"},
    },
    {
      name: ["--message-file", "-f"],
      description: "Specify a file containing the message to send the LLM, process reply, then exit (disables chat mode)",
      args: {name: "message_file", template: "filepaths"},
    },
    {
      name: ["--gui", "--browser"],
      description: "Run aider in your browser (default: False)",
      exclusiveOn: ["--no-gui", "--no-browser"],
    },
    {
      name: ["--no-gui", "--no-browser"],
      description: "Run aider in your browser (default: False)",
      exclusiveOn: ["--gui", "--browser"],
    },
    {
      name: "--copy-paste",
      description: "Enable automatic copy/paste of chat between aider and web UI (default: False)",
      exclusiveOn: ["--no-copy-paste"],
    },
    {
      name: "--no-copy-paste",
      description: "Enable automatic copy/paste of chat between aider and web UI (default: False)",
      exclusiveOn: ["--copy-paste"],
    },
    {
      name: "--apply",
      description: "Apply the changes from the given file instead of running the chat (debug)",
      args: {name: "file", template: "filepaths"},
    },
    {
      name: "--apply-clipboard-edits",
      description: "Apply clipboard contents as edits using the main model's editor format",
    },
    {
      name: "--exit",
      description: "Do all startup activities then exit before accepting user input (debug)",
    },
    {name: "--show-repo-map", description: "Print the repo map and exit (debug)"},
    {name: "--show-prompts", description: "Print the system prompts and exit (debug)"},
    {
      name: "--voice-format",
      description: "Audio format for voice recording (default: wav). webm and mp3 require ffmpeg",
      args: {name: "voice_format"},
    },
    {
      name: "--voice-language",
      description: "Specify the language for voice using ISO 639-1 code (default: auto)",
      args: {name: "voice_language"},
    },
    {
      name: "--voice-input-device",
      description: "Specify the input device name for voice recording",
      args: {name: "voice_input_device"},
    },
    {
      name: "--disable-playwright",
      description: "Never prompt for or attempt to install Playwright for web scraping (default: False).",
    },
    {
      name: "--file",
      description: "specify a file to edit (can be used multiple times)",
      args: {name: "file", template: "filepaths"},
      isRepeatable: true,
    },
    {
      name: "--read",
      description: "specify a read-only file (can be used multiple times)",
      args: {name: "file", template: "filepaths"},
      isRepeatable: true,
    },
    {name: "--vim", description: "Use VI editing mode in the terminal (default: False)"},
    {
      name: "--chat-language",
      description: "Specify the language to use in the chat (default: None, uses system settings)",
      args: {name: "chat_language"},
    },
    {
      name: "--commit-language",
      description: "Specify the language to use in the commit message (default: None, user language)",
      args: {name: "commit_language"},
    },
    {name: "--yes-always", description: "Always say yes to every confirmation"},
    {name: ["-v", "--verbose"], description: "Enable verbose output"},
    {
      name: "--load",
      description: "Load and execute /commands from a file on launch",
      args: {name: "load_file", template: "filepaths"},
    },
    {
      name: "--encoding",
      description: "Specify the encoding for input and output (default: utf-8)",
      args: {name: "encoding"},
    },
    {
      name: "--line-endings",
      description: "Line endings to use when writing files (default: platform)",
      args: {name: "line-endings", suggestions: ["platform", "lf", "crlf"]},
    },
    {
      name: ["-c", "--config"],
      description: "Specify the config file (default: search for .aider.conf.yml in git root, cwd or home directory)",
      args: {name: "config_file", template: "filepaths"},
    },
    {
      name: "--env-file",
      description: "Specify the .env file to load (default: .env in git root)",
      args: {name: "env_file", template: "filepaths"},
    },
    {
      name: "--suggest-shell-commands",
      description: "Enable/disable suggesting shell commands (default: True)",
      exclusiveOn: ["--no-suggest-shell-commands"],
    },
    {
      name: "--no-suggest-shell-commands",
      description: "Enable/disable suggesting shell commands (default: True)",
      exclusiveOn: ["--suggest-shell-commands"],
    },
    {
      name: "--fancy-input",
      description: "Enable/disable fancy input with history and completion (default: True)",
      exclusiveOn: ["--no-fancy-input"],
    },
    {
      name: "--no-fancy-input",
      description: "Enable/disable fancy input with history and completion (default: True)",
      exclusiveOn: ["--fancy-input"],
    },
    {
      name: "--multiline",
      description: "Enable/disable multi-line input mode with Meta-Enter to submit (default: False)",
      exclusiveOn: ["--no-multiline"],
    },
    {
      name: "--no-multiline",
      description: "Enable/disable multi-line input mode with Meta-Enter to submit (default: False)",
      exclusiveOn: ["--multiline"],
    },
    {
      name: "--notifications",
      description: "Enable/disable terminal bell notifications when LLM responses are ready (default: False)",
      exclusiveOn: ["--no-notifications"],
    },
    {
      name: "--no-notifications",
      description: "Enable/disable terminal bell notifications when LLM responses are ready (default: False)",
      exclusiveOn: ["--notifications"],
    },
    {
      name: "--notifications-command",
      description: "Specify a command to run for notifications instead of the terminal bell",
      args: {name: "command"},
    },
    {
      name: "--detect-urls",
      description: "Enable/disable detection and offering to add URLs to chat (default: True)",
      exclusiveOn: ["--no-detect-urls"],
    },
    {
      name: "--no-detect-urls",
      description: "Enable/disable detection and offering to add URLs to chat (default: True)",
      exclusiveOn: ["--detect-urls"],
    },
    {
      name: "--editor",
      description: "Specify which editor to use for the /editor command",
      args: {name: "editor"},
    },
    {
      name: "--shell-completions",
      description: "Print shell completion script for the specified SHELL and exit",
      args: {name: "shell", suggestions: ["bash", "tcsh", "zsh"]},
    },
    {
      name: "--opus",
      description: "Use claude-3-opus-20240229 model for the main chat (deprecated, use --model)",
    },
    {
      name: "--sonnet",
      description: "Use anthropic/claude-3-7-sonnet-20250219 model for the main chat (deprecated, use --model)",
    },
    {
      name: "--haiku",
      description: "Use claude-3-5-haiku-20241022 model for the main chat (deprecated, use --model)",
    },
    {
      name: ["--4", "-4"],
      description: "Use gpt-4-0613 model for the main chat (deprecated, use --model)",
    },
    {name: "--4o", description: "Use gpt-4o model for the main chat (deprecated, use --model)"},
    {
      name: "--mini",
      description: "Use gpt-4o-mini model for the main chat (deprecated, use --model)",
    },
    {
      name: "--4-turbo",
      description: "Use gpt-4-1106-preview model for the main chat (deprecated, use --model)",
    },
    {
      name: ["--35turbo", "--35-turbo", "--3", "-3"],
      description: "Use gpt-3.5-turbo model for the main chat (deprecated, use --model)",
    },
    {
      name: "--deepseek",
      description: "Use deepseek/deepseek-chat model for the main chat (deprecated, use --model)",
    },
    {
      name: "--o1-mini",
      description: "Use o1-mini model for the main chat (deprecated, use --model)",
    },
    {
      name: "--o1-preview",
      description: "Use o1-preview model for the main chat (deprecated, use --model)",
    },
  ],
  args: {
    name: "file",
    description: "files to edit with an LLM",
    isOptional: true,
    isVariadic: true,
    template: "filepaths",
  },
};
