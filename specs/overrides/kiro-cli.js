// From kiro-cli --help and the help of each subcommand, kiro-cli 2.37.1, 2026-11-02.
export default {
  name: "kiro-cli",
  subcommands: [
    {
      name: "agent",
      description: "Manage agents",
      subcommands: [
        {name: "list", description: "List the available agents"},
        {
          name: "create",
          description: "Create an agent config",
          options: [
            {
              name: ["-d", "--directory"],
              description: "The directory where the agent will be saved",
              args: {name: "directory", template: "folders"},
            },
            {
              name: ["-f", "--from"],
              description: "The name of an agent that shall be used as the starting point for the agent creation",
              args: {name: "from"},
            },
          ],
          args: {
            name: "name",
            description: "Name of the agent to be created (can also be provided via --name for backwards compatibility)",
            isOptional: true,
          },
        },
        {
          name: "edit",
          description: "Edit an existing agent config",
          options: [
            {
              name: "--path",
              description: "Path to the agent config file to edit",
              args: {name: "path", template: "filepaths"},
            },
          ],
          args: {
            name: "name",
            description: "Name of the agent to edit (can also be provided via --name for backwards compatibility)",
            isOptional: true,
          },
        },
        {
          name: "validate",
          description: "Validate a config with the given path",
          options: [{name: ["-p", "--path"], args: {name: "path", template: "filepaths"}}],
        },
        {
          name: "migrate",
          description: "Migrate profiles to agent Note that doing this is potentially destructive to agents that are...",
          options: [{name: "--force"}],
        },
        {
          name: "upgrade",
          description: "Upgrade agent configs in a directory to the universal format both engines load",
          options: [
            {
              name: "--no-backup",
              description: "Don't save each original as <name>.json.bak before rewriting it",
            },
            {name: "--dry-run", description: "Report what would change without writing any files"},
          ],
          args: {
            name: "dir",
            description: "Directory containing the agent *.json files",
            template: "folders",
          },
        },
        {
          name: "set-default",
          description: "Define a default agent to use when kiro-cli chat launches",
          args: {
            name: "name",
            description: "Name of the agent to set as default (can also be provided via --name for backwards compatibility)",
            isOptional: true,
          },
        },
      ],
    },
    {
      name: "chat",
      description: "AI assistant in your terminal",
      options: [
        {
          name: ["-r", "--resume"],
          description: "Resume the most recent conversation from this directory",
        },
        {
          name: "--resume-id",
          description: "Resume a specific conversation by session ID",
          args: {name: "session_id"},
        },
        {
          name: "--resume-picker",
          description: "Interactively select a conversation to resume from this directory",
        },
        {
          name: "--sessions",
          description: "Launch straight into the session dashboard (V3/KAS only); closing it exits rather than dropping into a chat",
        },
        {name: "--agent", description: "Context profile to use", args: {name: "agent"}},
        {name: "--model", description: "Current model to use", args: {name: "model"}},
        {
          name: "--effort",
          description: "Initial effort level (e.g. low, medium, high, xhigh, max)",
          args: {name: "effort"},
        },
        {
          name: ["-a", "--trust-all-tools"],
          description: "Allows the model to use any tool to run commands without asking for confirmation",
        },
        {
          name: "--trust-tools",
          description: "Trust only this set of tools",
          args: {name: "tool_names"},
        },
        {
          name: "--no-interactive",
          description: "Whether the command should run without expecting user input",
        },
        {
          name: "--trust-workspace",
          description: "Skip the \"Trust this workspace?\" prompt for this launch only (V3 only)",
        },
        {
          name: ["-l", "--list-sessions"],
          description: "List all saved chat sessions for the current directory",
        },
        {
          name: "--all-cwds",
          description: "With --list-sessions: span every workspace (local stores only), emitting one envelope per cwd in JSON output",
        },
        {name: "--list-models", description: "List available models and exit"},
        {
          name: ["-f", "--format"],
          description: "Output format for list commands (used with --list-models and --list-sessions)",
          args: {
            name: "format",
            suggestions: [
              {name: "plain", description: "Outputs the results as markdown"},
              {name: "json", description: "Outputs the results as JSON"},
              {name: "json-pretty", description: "Outputs the results as pretty print JSON"},
            ],
          },
        },
        {
          name: "--output-format",
          description: "Response stream format: text (default), or stream-json for JSON Lines on stdout. stream-json implies --no-interactive",
          args: {
            name: "output_format",
            suggestions: [
              {name: "text", description: "Human-readable text"},
              {
                name: "stream-json",
                description: "The run's ACP events as JSON Lines on stdout, one self-describing event per line",
              },
            ],
          },
        },
        {
          name: ["-d", "--delete-session"],
          description: "Delete a saved chat session by ID",
          args: {name: "session_id"},
        },
        {
          name: "--session-source",
          description: "Target only v1 or v2 store for --delete-session (default: both)",
          args: {name: "session-source", suggestions: ["v1", "v2"]},
        },
        {
          name: ["-w", "--wrap"],
          description: "Control line wrapping behavior (default: auto-detect)",
          args: {
            name: "wrap",
            suggestions: [
              {name: "always", description: "Always wrap at terminal width"},
              {name: "never", description: "Never wrap (raw output)"},
              {name: "auto", description: "Auto-detect based on output target (default)"},
            ],
          },
        },
        {
          name: "--require-mcp-startup",
          description: "Require all enabled MCP servers to start successfully; exit with code 3 if any fail",
        },
        {name: "--tui", description: "Use the new terminal UI"},
        {name: ["--legacy-ui", "--classic"], description: "Use the legacy harness"},
        {
          name: "--agent-engine",
          description: "Agent engine to use: \"v1\", \"v2\" (default), or \"v3\"",
          args: {name: "engine", suggestions: ["v2", "v1", "v3"]},
        },
        {name: "--v3", description: "Launch the next generation Kiro agent"},
        {name: "--v2", description: "Use the V2 agent engine (the pre-3.0 default)"},
        {
          name: "--mode",
          description: "Mode to use with the V3 agent: \"default\" or \"spec\"",
          args: {name: "mode", suggestions: ["default", "spec"]},
        },
        {
          name: "--cloud",
          description: "Run this session in a cloud sandbox (V3/KAS only), interactively or with --no-interactive",
        },
        {
          name: "--repo",
          description: "Repository to open in a cloud session, as name or owner/name (comma-separated for multiple)",
          args: {name: "repo"},
        },
      ],
      args: {name: "input", description: "The first question to ask", isOptional: true},
    },
    {
      name: "login",
      description: "Log in to Kiro",
      options: [
        {
          name: "--license",
          description: "License type (pro for Identity Center, free for Builder ID)",
          args: {
            name: "license",
            suggestions: [
              {name: "free", description: "Free license with Builder ID"},
              {name: "pro", description: "Pro license with Identity Center"},
            ],
          },
        },
        {
          name: "--identity-provider",
          description: "Identity provider URL (for Identity Center)",
          args: {name: "identity_provider"},
        },
        {name: "--region", description: "Region (for Identity Center)", args: {name: "region"}},
        {
          name: "--social",
          description: "Social provider (google or github)",
          args: {name: "social", suggestions: ["google", "github"]},
        },
        {
          name: "--use-device-flow",
          description: "Always use the OAuth device flow for authentication",
        },
      ],
    },
    {name: "logout", description: "Log out of Kiro"},
    {
      name: "whoami",
      description: "Print info about the current login session",
      options: [
        {
          name: ["-f", "--format"],
          description: "Output format to use",
          args: {
            name: "format",
            suggestions: [
              {name: "plain", description: "Outputs the results as markdown"},
              {name: "json", description: "Outputs the results as JSON"},
              {name: "json-pretty", description: "Outputs the results as pretty print JSON"},
            ],
          },
        },
      ],
    },
    {name: "profile", description: "Show the profile associated with this idc user"},
    {
      name: "settings",
      description: "Customize appearance & behavior",
      subcommands: [
        {
          name: "open",
          description: "Open the settings file",
          options: [
            {name: "--global", description: "Open global settings file (default)"},
            {name: "--workspace", description: "Open workspace settings file"},
          ],
        },
        {
          name: "list",
          description: "List settings",
          options: [
            {name: "--all", description: "Show all available settings"},
            {
              name: ["-f", "--format"],
              description: "Format of the output",
              args: {
                name: "format",
                suggestions: [
                  {name: "plain", description: "Outputs the results as markdown"},
                  {name: "json", description: "Outputs the results as JSON"},
                  {name: "json-pretty", description: "Outputs the results as pretty print JSON"},
                ],
              },
            },
          ],
        },
      ],
      options: [
        {name: ["-d", "--delete"], description: "Delete a key (No value needed)"},
        {name: "--global", description: "Set or delete in global settings (default)"},
        {name: "--workspace", description: "Set or delete in workspace settings"},
        {
          name: ["-f", "--format"],
          description: "Format of the output",
          args: {
            name: "format",
            suggestions: [
              {name: "plain", description: "Outputs the results as markdown"},
              {name: "json", description: "Outputs the results as JSON"},
              {name: "json-pretty", description: "Outputs the results as pretty print JSON"},
            ],
          },
        },
      ],
      args: [
        {name: "key", description: "key", isOptional: true},
        {name: "value", description: "value", isOptional: true},
      ],
    },
    {
      name: "diagnostic",
      description: "Run diagnostic tests",
      options: [
        {
          name: ["-f", "--format"],
          description: "The format of the output",
          args: {
            name: "format",
            suggestions: [
              {name: "plain", description: "Outputs the results as markdown"},
              {name: "json", description: "Outputs the results as JSON"},
              {name: "json-pretty", description: "Outputs the results as pretty print JSON"},
            ],
          },
        },
        {name: "--force", description: "Force limited diagnostic output"},
      ],
    },
    {
      name: "issue",
      description: "Create a new Github issue",
      options: [{name: ["-f", "--force"], description: "Force issue creation"}],
      args: {name: "description", description: "Issue description", isOptional: true, isVariadic: true},
    },
    {
      name: "crew",
      description: "Launch Kiro Crew, installing it if it is not already installed",
      options: [
        {
          name: ["-y", "--yes"],
          description: "Install Kiro Crew without prompting if it is not already installed",
        },
      ],
      args: {
        name: "args",
        description: "Arguments forwarded to the Kiro Crew CLI",
        isOptional: true,
        isVariadic: true,
      },
    },
    {
      name: "mcp",
      description: "Model Context Protocol (MCP)",
      subcommands: [
        {
          name: "add",
          description: "Add or replace a configured server",
          options: [
            {
              name: "--name",
              description: "Name for the server (optional when registry is configured - shows interactive menu)",
              args: {name: "name"},
            },
            {
              name: "--scope",
              description: "Scope",
              args: {name: "scope", suggestions: ["default", "workspace", "global"]},
            },
            {
              name: "--command",
              description: "The command used to launch the server (required for stdio servers, not needed for registry or HTTP servers)",
              args: {name: "command"},
            },
            {
              name: "--url",
              description: "The URL for HTTP-based MCP server communication (required for HTTP servers)",
              args: {name: "url"},
            },
            {name: "--args", description: "Arguments to pass to the command", args: {name: "args"}},
            {name: "--agent", description: "Where to add the server to", args: {name: "agent"}},
            {
              name: "--env",
              description: "Environment variables to use when launching the server",
              args: {name: "env"},
            },
            {
              name: "--timeout",
              description: "Server launch timeout, in milliseconds",
              args: {name: "timeout"},
            },
            {name: "--disabled", description: "Whether the server should be disabled (not loaded)"},
            {name: "--force", description: "Overwrite an existing server with the same name"},
          ],
        },
        {
          name: "remove",
          description: "Remove a server from the MCP configuration",
          options: [
            {name: "--name", args: {name: "name"}},
            {
              name: "--scope",
              description: "Scope",
              args: {name: "scope", suggestions: ["default", "workspace", "global"]},
            },
            {name: "--agent", args: {name: "agent"}},
          ],
        },
        {
          name: "list",
          description: "List configured servers",
          args: {name: "scope", isOptional: true, suggestions: ["default", "workspace", "global"]},
        },
        {
          name: "import",
          description: "Import a server configuration from another file",
          options: [
            {name: "--file", args: {name: "file", template: "filepaths"}},
            {name: "--force", description: "Overwrite an existing server with the same name"},
          ],
          args: {name: "scope", isOptional: true, suggestions: ["default", "workspace", "global"]},
        },
        {
          name: "status",
          description: "Get the status of a configured server",
          options: [{name: "--name", args: {name: "name"}}],
        },
      ],
    },
    {
      name: "powers",
      description: "Manage Powers (install and uninstall plugins)",
      subcommands: [
        {
          name: "install",
          description: "Install a power by name or local path",
          args: {
            name: "name_or_path",
            description: "Catalog name or local filesystem path",
            template: "filepaths",
          },
        },
        {
          name: "uninstall",
          description: "Uninstall a power by name",
          args: {name: "name", description: "Power name to uninstall"},
        },
      ],
    },
    {
      name: "update",
      description: "Check for and install updates",
      options: [
        {name: "--check", description: "Only check for updates without installing"},
        {name: "--force", description: "Force installation even if already on latest version"},
      ],
    },
    {
      name: "integrations",
      description: "Manage system integrations",
      subcommands: [
        {
          name: "install",
          subcommands: [{name: "kiro-command-router"}],
          options: [{name: ["-s", "--silent"], description: "Suppress status messages"}],
        },
        {
          name: "uninstall",
          subcommands: [{name: "kiro-command-router"}],
          options: [{name: ["-s", "--silent"], description: "Suppress status messages"}],
        },
        {
          name: "reinstall",
          subcommands: [{name: "kiro-command-router"}],
          options: [{name: ["-s", "--silent"], description: "Suppress status messages"}],
        },
        {
          name: "status",
          subcommands: [{name: "kiro-command-router"}],
          options: [
            {
              name: ["-f", "--format"],
              args: {
                name: "format",
                suggestions: [
                  {name: "plain", description: "Outputs the results as markdown"},
                  {name: "json", description: "Outputs the results as JSON"},
                  {name: "json-pretty", description: "Outputs the results as pretty print JSON"},
                ],
              },
            },
          ],
        },
      ],
    },
    {
      name: "serve",
      description: "Start a persistent WebSocket server for the v3 engine",
      options: [{name: "--port", description: "Port to listen on", args: {name: "port"}}],
    },
  ],
  options: [
    {
      name: ["-v", "--verbose"],
      description: "Increase logging verbosity",
      isRepeatable: true,
      isPersistent: true,
    },
    {
      name: "--agent",
      description: "Launch chat with the given agent, shorthand for chat --agent",
      args: {name: "agent"},
    },
    {name: "--tui", description: "Launch chat in TUI mode"},
    {name: ["--legacy-ui", "--classic"], description: "Launch chat in legacy UI mode"},
    {name: "--v3", description: "Launch the next generation Kiro agent"},
    {name: "--v2", description: "Use the V2 agent engine (the pre-3.0 default)"},
    {
      name: ["-r", "--resume"],
      description: "Resume the most recent conversation from this directory",
    },
    {
      name: "--resume-id",
      description: "Resume a specific conversation by session ID",
      args: {name: "session_id"},
    },
    {
      name: ["--resume-picker", "--list"],
      description: "Interactively select a conversation to resume from this directory",
    },
    {
      name: "--cloud",
      description: "Run this session in a cloud sandbox, shorthand for chat --cloud",
    },
    {
      name: "--repo",
      description: "Repository to open in a cloud session, shorthand for chat --repo",
      args: {name: "repo"},
    },
    {name: ["-h", "--help"], description: "Print help", isPersistent: true},
    {name: ["-V", "--version"], description: "Print version"},
  ],
};
