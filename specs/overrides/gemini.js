// From gemini --help and the help of each subcommand, Gemini CLI 0.66.0, 2026-11-02.
export default {
  name: "gemini",
  subcommands: [
    {
      name: "mcp",
      description: "Manage MCP servers",
      subcommands: [
        {name: "add", description: "Add a server"},
        {name: "remove", description: "Remove a server"},
        {name: "list", description: "List all configured MCP servers"},
        {
          name: "enable",
          description: "Enable an MCP server",
          options: [{name: "--session", description: "Clear session-only disable"}],
          args: {name: "name", description: "MCP server name to enable"},
        },
        {
          name: "disable",
          description: "Disable an MCP server",
          options: [{name: "--session", description: "Disable for current session only"}],
          args: {name: "name", description: "MCP server name to disable"},
        },
      ],
    },
    {
      name: ["extensions", "extension"],
      description: "Manage Gemini CLI extensions.",
      subcommands: [
        {
          name: "install",
          description: "Installs an extension from a git repository URL or a local path.",
          options: [
            {name: "--ref", description: "The git ref to install from.", args: {name: "ref"}},
            {name: "--auto-update", description: "Enable auto-update for this extension."},
            {name: "--pre-release", description: "Enable pre-release versions for this extension."},
            {
              name: "--consent",
              description: "Acknowledge the security risks of installing an extension and skip the confirmation prompt.",
            },
            {name: "--skip-settings", description: "Skip the configuration on install process."},
          ],
          args: {name: "source", description: "The github URL or local path of the extension to install."},
        },
        {
          name: "uninstall",
          description: "Uninstalls one or more extensions.",
          options: [{name: "--all", description: "Uninstall all installed extensions."}],
          args: {
            name: "names",
            description: "The name(s) or source path(s) of the extension(s) to uninstall.",
          },
        },
        {
          name: "list",
          description: "Lists installed extensions.",
          options: [
            {
              name: ["-o", "--output-format"],
              description: "The format of the CLI output.",
              args: {name: "output-format", suggestions: ["text", "json"]},
            },
          ],
        },
        {
          name: "update",
          description: "Updates all extensions or a named extension to the latest version.",
          options: [{name: "--all", description: "Update all extensions."}],
          args: {name: "name", description: "The name of the extension to update."},
        },
        {
          name: "disable",
          description: "Disables an extension.",
          options: [
            {
              name: "--scope",
              description: "The scope to disable the extension in.",
              args: {name: "scope"},
            },
          ],
          args: {name: "name", description: "The name of the extension to disable."},
        },
        {
          name: "enable",
          description: "Enables an extension.",
          options: [
            {
              name: "--scope",
              description: "The scope to enable the extension in. If not set, will be enabled in all scopes.",
              args: {name: "scope"},
            },
          ],
          args: {name: "name", description: "The name of the extension to enable."},
        },
        {
          name: "link",
          description: "Links an extension from a local path. Updates made to the local path will always be reflected.",
          options: [
            {
              name: "--consent",
              description: "Acknowledge the security risks of installing an extension and skip the confirmation prompt.",
            },
          ],
          args: {name: "path", description: "The name of the extension to link.", template: "filepaths"},
        },
        {
          name: "new",
          description: "Create a new extension from a boilerplate example.",
          args: [
            {
              name: "path",
              description: "The path to create the extension in.",
              template: "filepaths",
            },
            {
              name: "template",
              description: "The boilerplate template to use.",
              suggestions: [
                "custom-commands",
                "exclude-tools",
                "hooks",
                "mcp-server",
                "policies",
                "skills",
                "themes-example",
              ],
            },
          ],
        },
        {
          name: "validate",
          description: "Validates an extension from a local path.",
          args: {
            name: "path",
            description: "The path of the extension to validate.",
            template: "filepaths",
          },
        },
        {
          name: "config",
          description: "Configure extension settings.",
          options: [
            {
              name: "--scope",
              description: "The scope to set the setting in.",
              args: {name: "scope", suggestions: ["user", "workspace"]},
            },
          ],
          args: [
            {name: "name", description: "Name of the extension to configure."},
            {name: "setting", description: "The specific setting to configure (name or env var)."},
          ],
        },
      ],
    },
    {
      name: ["skills", "skill"],
      description: "Manage agent skills.",
      subcommands: [
        {
          name: "list",
          description: "Lists discovered agent skills.",
          options: [{name: "--all", description: "Show all skills, including built-in ones."}],
        },
        {
          name: "enable",
          description: "Enables an agent skill.",
          args: {name: "name", description: "The name of the skill to enable."},
        },
        {
          name: "disable",
          description: "Disables an agent skill.",
          options: [
            {
              name: ["-s", "--scope"],
              description: "The scope to disable the skill in (user or workspace).",
              args: {name: "scope", suggestions: ["user", "workspace"]},
            },
          ],
          args: {name: "name", description: "The name of the skill to disable."},
        },
        {
          name: "install",
          description: "Installs an agent skill from a git repository URL or a local path.",
          options: [
            {
              name: "--scope",
              description: "The scope to install the skill into. Defaults to \"user\" (global).",
            },
            {
              name: "--path",
              description: "Sub-path within the repository to install from (only used for git repository sources).",
              args: {name: "path", template: "filepaths"},
            },
            {
              name: "--consent",
              description: "Acknowledge the security risks of installing a skill and skip the confirmation prompt.",
            },
          ],
          args: {
            name: "source",
            description: "The git repository URL or local path of the skill to install.",
          },
        },
        {
          name: "link",
          description: "Links an agent skill from a local path. Updates to the source will be reflected immediately.",
          options: [
            {
              name: "--scope",
              description: "The scope to link the skill into. Defaults to \"user\" (global).",
            },
            {
              name: "--consent",
              description: "Acknowledge the security risks of linking a skill and skip the confirmation prompt.",
            },
          ],
          args: {name: "path", description: "The local path of the skill to link.", template: "filepaths"},
        },
        {
          name: "uninstall",
          description: "Uninstalls an agent skill by name.",
          options: [
            {
              name: "--scope",
              description: "The scope to uninstall the skill from. Defaults to \"user\" (global).",
            },
          ],
          args: {name: "name", description: "The name of the skill to uninstall."},
        },
      ],
    },
    {
      name: ["hooks", "hook"],
      description: "Manage Gemini CLI hooks.",
      subcommands: [
        {
          name: "migrate",
          description: "Migrate hooks from Claude Code to Gemini CLI",
          options: [{name: "--from-claude", description: "Migrate from Claude Code hooks"}],
        },
      ],
    },
    {
      name: "gemma",
      description: "Manage local Gemma model routing",
      subcommands: [
        {
          name: "setup",
          description: "Download and configure Gemma local model routing",
          options: [
            {name: "--port", description: "Port for the LiteRT server", args: {name: "port"}},
            {name: "--skip-model", description: "Skip model download (binary only)"},
            {name: "--start", description: "Start the server after setup"},
            {name: "--force", description: "Re-download binary and model even if already present"},
            {name: "--consent", description: "Skip interactive consent prompt (implies acceptance)"},
          ],
        },
        {
          name: "start",
          description: "Start the LiteRT-LM server",
          options: [{name: "--port", description: "Port for the LiteRT server", args: {name: "port"}}],
        },
        {
          name: "stop",
          description: "Stop the LiteRT-LM server",
          options: [
            {
              name: "--port",
              description: "Port where the LiteRT server is running",
              args: {name: "port"},
            },
          ],
        },
        {
          name: "status",
          description: "Check Gemma local model routing status",
          options: [
            {
              name: "--port",
              description: "Port to check for the LiteRT server",
              args: {name: "port"},
            },
          ],
        },
        {
          name: "logs",
          description: "View LiteRT-LM server logs",
          options: [
            {
              name: ["-n", "--lines"],
              description: "Show the last N lines and exit (omit to follow live)",
              args: {name: "lines"},
            },
            {
              name: ["-f", "--follow"],
              description: "Follow log output (defaults to true when --lines is omitted)",
            },
          ],
        },
      ],
    },
  ],
  options: [
    {
      name: ["-d", "--debug"],
      description: "Run in debug mode (open debug console with F12)",
      isPersistent: true,
    },
    {name: ["-m", "--model"], description: "Model", args: {name: "model"}},
    {
      name: ["-p", "--prompt"],
      description: "Run in non-interactive (headless) mode with the given prompt. Appended to input on stdin (if any).",
      args: {name: "prompt"},
    },
    {
      name: ["-i", "--prompt-interactive"],
      description: "Execute the provided prompt and continue in interactive mode",
      args: {name: "prompt-interactive"},
    },
    {name: "--skip-trust", description: "Trust the current workspace for this session."},
    {
      name: ["-w", "--worktree"],
      description: "Start Gemini in a new git worktree. If no name is provided, one is generated automatically.",
      args: {name: "worktree"},
    },
    {name: ["-s", "--sandbox"], description: "Run in sandbox?"},
    {name: ["-y", "--yolo"], description: "Automatically accept all actions"},
    {
      name: "--approval-mode",
      description: "Set the approval mode: default",
      args: {name: "approval-mode", suggestions: ["default", "auto_edit", "yolo", "plan"]},
    },
    {
      name: "--policy",
      description: "Additional policy files or directories to load (comma-separated or multiple --policy)",
      args: {name: "policy", template: "filepaths"},
      isRepeatable: true,
    },
    {
      name: "--admin-policy",
      description: "Additional admin policy files or directories to load (comma-separated or multiple --admin-policy)",
      args: {name: "admin-policy", template: "filepaths"},
      isRepeatable: true,
    },
    {name: "--acp", description: "Starts the agent in ACP mode"},
    {
      name: "--experimental-acp",
      description: "Starts the agent in ACP mode (deprecated, use --acp instead)",
    },
    {
      name: "--allowed-mcp-server-names",
      description: "Allowed MCP server names",
      args: {name: "allowed-mcp-server-names"},
    },
    {
      name: "--allowed-tools",
      description: "[DEPRECATED: Use Policy Engine instead See https://geminicli.com/docs/core/policy-engine] Tools that are...",
      args: {name: "allowed-tools"},
    },
    {
      name: ["-e", "--extensions"],
      description: "A list of extensions to use. If not provided, all extensions are used.",
      args: {name: "extensions"},
    },
    {name: ["-l", "--list-extensions"], description: "List all available extensions and exit."},
    {
      name: ["-r", "--resume"],
      description: "Resume a previous session. Use \"latest\" for most recent or index number (e.g. --resume 5)",
      args: {name: "resume"},
    },
    {
      name: "--session-file",
      description: "Load a session from a JSON file",
      args: {name: "session-file", template: "filepaths"},
    },
    {
      name: "--session-id",
      description: "Start a new session with a manually provided UUID.",
      args: {name: "session-id"},
    },
    {
      name: "--list-sessions",
      description: "List available sessions for the current project and exit.",
    },
    {
      name: "--delete-session",
      description: "Delete a session by index number (use --list-sessions to see available sessions).",
      args: {name: "delete-session"},
    },
    {
      name: "--include-directories",
      description: "Additional directories to include in the workspace (comma-separated or multiple --include-directories)",
      args: {name: "include-directories", template: "folders"},
      isRepeatable: true,
    },
    {name: "--screen-reader", description: "Enable screen reader mode for accessibility."},
    {
      name: ["-o", "--output-format"],
      description: "The format of the CLI output.",
      args: {name: "output-format", suggestions: ["text", "json", "stream-json"]},
    },
    {
      name: "--raw-output",
      description: "Disable sanitization of model output (e.g. allow ANSI escape sequences)",
    },
    {
      name: "--accept-raw-output-risk",
      description: "Suppress the security warning when using --raw-output.",
    },
    {name: ["-v", "--version"], description: "Show version number"},
    {name: ["-h", "--help"], description: "Show help", isPersistent: true},
  ],
  args: {
    name: "query",
    description: "Initial prompt. Runs in interactive mode by default; use -p/--prompt for non-interactive.",
  },
};
