// From opencode --help and the help of each subcommand, opencode 1.19.5, 2026-11-02.
export default {
  name: "opencode",
  subcommands: [
    {name: "completion", description: "generate shell completion script"},
    {
      name: "acp",
      description: "start ACP (Agent Client Protocol) server",
      options: [
        {name: "--port", description: "port to listen on", args: {name: "port"}},
        {name: "--hostname", description: "hostname to listen on", args: {name: "hostname"}},
        {
          name: "--mdns",
          description: "enable mDNS service discovery (defaults hostname to 0.0.0.0)",
        },
        {
          name: "--mdns-domain",
          description: "custom domain name for mDNS service (default: opencode.local)",
          args: {name: "mdns-domain"},
        },
        {name: "--cors", description: "additional domains to allow for CORS", args: {name: "cors"}},
        {name: "--cwd", description: "working directory", args: {name: "cwd", template: "folders"}},
      ],
    },
    {
      name: "mcp",
      description: "manage MCP (Model Context Protocol) servers",
      subcommands: [
        {
          name: "add",
          description: "add an MCP server",
          options: [
            {name: "--url", description: "URL for a remote MCP server", args: {name: "url"}},
            {
              name: "--env",
              description: "environment variable for a local MCP server (KEY=VALUE)",
              args: {name: "env"},
            },
            {
              name: "--header",
              description: "HTTP header for a remote MCP server (KEY=VALUE)",
              args: {name: "header"},
            },
          ],
          args: {name: "name", description: "name of the MCP server"},
        },
        {name: ["list", "ls"], description: "list MCP servers and their status"},
        {
          name: "auth",
          description: "authenticate with an OAuth-enabled MCP server",
          subcommands: [
            {
              name: ["list", "ls"],
              description: "list OAuth-capable MCP servers and their auth status",
              args: {name: "name", description: "name of the MCP server"},
            },
          ],
          args: {name: "name", description: "name of the MCP server"},
        },
        {
          name: "logout",
          description: "remove OAuth credentials for an MCP server",
          args: {name: "name", description: "name of the MCP server"},
        },
        {
          name: "debug",
          description: "debug OAuth connection for an MCP server",
          args: {name: "name", description: "name of the MCP server"},
        },
      ],
    },
    {
      name: "attach",
      description: "attach to a running opencode server",
      options: [
        {
          name: "--dir",
          description: "directory to run in",
          args: {name: "dir", template: "folders"},
        },
        {name: ["-c", "--continue"], description: "continue the last session"},
        {name: ["-s", "--session"], description: "session id to continue", args: {name: "session"}},
        {
          name: "--fork",
          description: "fork the session when continuing (use with --continue or --session)",
        },
        {
          name: ["-p", "--password"],
          description: "basic auth password (defaults to OPENCODE_SERVER_PASSWORD)",
          args: {name: "password"},
        },
        {
          name: ["-u", "--username"],
          description: "basic auth username (defaults to OPENCODE_SERVER_USERNAME or 'opencode')",
          args: {name: "username"},
        },
        {name: "--mini", description: "start the minimal interactive interface"},
        {
          name: "--no-replay",
          description: "disable mini session history replay on resume and after resize",
        },
        {
          name: "--replay-limit",
          description: "cap visible mini replay to the newest N messages",
          args: {name: "replay-limit"},
        },
      ],
      args: {name: "url", description: "http://localhost:4096"},
    },
    {
      name: "run",
      description: "run opencode with a message",
      options: [
        {
          name: "--command",
          description: "the command to run, use message for args",
          args: {name: "command"},
        },
        {name: ["-c", "--continue"], description: "continue the last session"},
        {name: ["-s", "--session"], description: "session id to continue", args: {name: "session"}},
        {
          name: "--fork",
          description: "fork the session before continuing (requires --continue or --session)",
        },
        {
          name: ["-m", "--model"],
          description: "model to use in the format of provider/model",
          args: {name: "model"},
        },
        {name: "--agent", description: "agent to use", args: {name: "agent"}},
        {
          name: "--format",
          description: "format: default (formatted) or json (raw JSON events)",
          args: {name: "format", suggestions: ["default", "json"]},
        },
        {
          name: ["-f", "--file"],
          description: "file(s) to attach to message",
          args: {name: "file", template: "filepaths"},
        },
        {
          name: "--title",
          description: "title for the session (uses truncated prompt if no value provided)",
          args: {name: "title"},
        },
        {
          name: "--attach",
          description: "attach to a running opencode server (e.g., http://localhost:4096)",
          args: {name: "attach"},
        },
        {
          name: ["-p", "--password"],
          description: "basic auth password (defaults to OPENCODE_SERVER_PASSWORD)",
          args: {name: "password"},
        },
        {
          name: ["-u", "--username"],
          description: "basic auth username (defaults to OPENCODE_SERVER_USERNAME or 'opencode')",
          args: {name: "username"},
        },
        {
          name: "--dir",
          description: "directory to run in, path on remote server if attaching",
          args: {name: "dir", template: "folders"},
        },
        {
          name: "--port",
          description: "port for the local server (defaults to random port if no value provided)",
          args: {name: "port"},
        },
        {
          name: "--variant",
          description: "model variant (provider-specific reasoning effort, e.g., high, max, minimal)",
          args: {name: "variant"},
        },
        {name: "--thinking", description: "show thinking blocks"},
        {name: ["-i", "--interactive"], description: "run in direct interactive split-footer mode"},
        {
          name: "--auto",
          description: "auto-approve permissions that are not explicitly denied (dangerous!)",
        },
      ],
      args: {name: "message", description: "message to send"},
    },
    {
      name: "debug",
      description: "debugging and troubleshooting tools",
      subcommands: [
        {name: "config", description: "show resolved configuration"},
        {
          name: "lsp",
          description: "LSP debugging utilities",
          subcommands: [
            {
              name: "diagnostics",
              description: "get diagnostics for a file",
              args: {name: "file", template: "filepaths"},
            },
            {name: "symbols", description: "search workspace symbols", args: {name: "query"}},
            {
              name: "document-symbols",
              description: "get symbols from a document",
              args: {name: "uri"},
            },
          ],
        },
        {
          name: "rg",
          description: "ripgrep debugging utilities",
          subcommands: [
            {
              name: "files",
              description: "list files using ripgrep",
              options: [
                {name: "--query", description: "Filter files by query", args: {name: "query"}},
                {name: "--glob", description: "Glob pattern to match files", args: {name: "glob"}},
                {name: "--limit", description: "Limit number of results", args: {name: "limit"}},
              ],
            },
            {
              name: "search",
              description: "search file contents using ripgrep",
              options: [
                {name: "--glob", description: "File glob patterns", args: {name: "glob"}},
                {name: "--limit", description: "Limit number of results", args: {name: "limit"}},
              ],
              args: {name: "pattern", description: "Search pattern"},
            },
          ],
        },
        {
          name: "file",
          description: "file system debugging utilities",
          subcommands: [
            {
              name: "read",
              description: "read file contents as JSON",
              args: {name: "path", description: "File path to read", template: "filepaths"},
            },
            {
              name: "list",
              description: "list files in a directory",
              args: {name: "path", description: "File path to list", template: "filepaths"},
            },
            {
              name: "search",
              description: "search files by query",
              args: {name: "query", description: "Search query"},
            },
          ],
        },
        {name: "scrap", description: "list all known projects"},
        {name: "skill", description: "list all available skills"},
        {
          name: "snapshot",
          description: "snapshot debugging utilities",
          subcommands: [
            {name: "track", description: "track current snapshot state"},
            {
              name: "patch",
              description: "show patch for a snapshot hash",
              args: {name: "hash", description: "hash"},
            },
            {
              name: "diff",
              description: "show diff for a snapshot hash",
              args: {name: "hash", description: "hash"},
            },
          ],
        },
        {name: "startup", description: "print startup timing"},
        {
          name: "agent",
          description: "show agent configuration details",
          options: [
            {name: "--tool", description: "Tool id to execute", args: {name: "tool"}},
            {
              name: "--params",
              description: "Tool params as JSON or a JS object literal",
              args: {name: "params"},
            },
          ],
          args: {name: "name", description: "Agent name"},
        },
        {name: "v2", description: "debug v2 catalog and built-in plugins"},
        {name: "info", description: "show debug information"},
        {name: "paths", description: "show global paths (data, config, cache, state)"},
        {name: "wait", description: "wait indefinitely (for debugging)"},
      ],
    },
    {
      name: ["providers", "auth"],
      description: "manage AI providers and credentials",
      subcommands: [
        {name: ["list", "ls"], description: "list providers and credentials"},
        {
          name: "login",
          description: "log in to a provider",
          options: [
            {
              name: ["-p", "--provider"],
              description: "provider id or name to log in to (skips provider selection)",
              args: {name: "provider"},
            },
            {
              name: ["-m", "--method"],
              description: "login method label (skips method selection)",
              args: {name: "method"},
            },
          ],
          args: {name: "url", description: "opencode auth provider"},
        },
        {
          name: "logout",
          description: "log out from a configured provider",
          args: {name: "provider", description: "provider id or name to log out from"},
        },
      ],
    },
    {
      name: "agent",
      description: "manage agents",
      subcommands: [
        {
          name: "create",
          description: "create a new agent",
          options: [
            {
              name: "--path",
              description: "directory path to generate the agent file",
              args: {name: "path", template: "filepaths"},
            },
            {
              name: "--description",
              description: "what the agent should do",
              args: {name: "description"},
            },
            {
              name: "--mode",
              description: "agent mode",
              args: {name: "mode", suggestions: ["all", "primary", "subagent"]},
            },
            {
              name: ["--permissions", "--tools"],
              description: "comma-separated list of permissions to allow (default: all).",
              args: {name: "permissions"},
            },
            {
              name: ["-m", "--model"],
              description: "model to use in the format of provider/model",
              args: {name: "model"},
            },
          ],
        },
        {name: "list", description: "list all available agents"},
      ],
    },
    {
      name: "upgrade",
      description: "upgrade opencode to the latest or a specific version",
      options: [
        {
          name: ["-m", "--method"],
          description: "installation method to use",
          args: {name: "method", suggestions: ["curl", "npm", "pnpm", "bun", "brew", "choco", "scoop"]},
        },
        {name: "--major", description: "upgrade to OpenCode 2 and remove OpenCode 1"},
      ],
      args: {name: "target", description: "version to upgrade to, for ex '0.1.48' or 'v0.1.48'"},
    },
    {
      name: "uninstall",
      description: "uninstall opencode and remove all related files",
      options: [
        {name: ["-c", "--keep-config"], description: "keep configuration files"},
        {name: ["-d", "--keep-data"], description: "keep session data and snapshots"},
        {name: "--dry-run", description: "show what would be removed without removing"},
        {name: ["-f", "--force"], description: "skip confirmation prompts"},
      ],
    },
    {
      name: "serve",
      description: "starts a headless opencode server",
      options: [
        {name: "--port", description: "port to listen on", args: {name: "port"}},
        {name: "--hostname", description: "hostname to listen on", args: {name: "hostname"}},
        {
          name: "--mdns",
          description: "enable mDNS service discovery (defaults hostname to 0.0.0.0)",
        },
        {
          name: "--mdns-domain",
          description: "custom domain name for mDNS service (default: opencode.local)",
          args: {name: "mdns-domain"},
        },
        {name: "--cors", description: "additional domains to allow for CORS", args: {name: "cors"}},
      ],
    },
    {
      name: "web",
      description: "start opencode server and open web interface",
      options: [
        {name: "--port", description: "port to listen on", args: {name: "port"}},
        {name: "--hostname", description: "hostname to listen on", args: {name: "hostname"}},
        {
          name: "--mdns",
          description: "enable mDNS service discovery (defaults hostname to 0.0.0.0)",
        },
        {
          name: "--mdns-domain",
          description: "custom domain name for mDNS service (default: opencode.local)",
          args: {name: "mdns-domain"},
        },
        {name: "--cors", description: "additional domains to allow for CORS", args: {name: "cors"}},
      ],
    },
    {
      name: "models",
      description: "list all available models",
      options: [
        {
          name: "--verbose",
          description: "use more verbose model output (includes metadata like costs)",
        },
        {name: "--refresh", description: "refresh the models cache from models.dev"},
      ],
      args: {name: "provider", description: "provider ID to filter models by"},
    },
    {
      name: "stats",
      description: "show token usage and cost statistics",
      options: [
        {
          name: "--days",
          description: "show stats for the last N days (default: all time)",
          args: {name: "days"},
        },
        {
          name: "--tools",
          description: "number of tools to show (default: all)",
          args: {name: "tools"},
        },
        {
          name: "--models",
          description: "show model statistics (default: hidden). Pass a number to show top N, otherwise shows all",
        },
        {
          name: "--project",
          description: "filter by project (default: all projects, empty string: current project)",
          args: {name: "project"},
        },
      ],
    },
    {
      name: "export",
      description: "export session data as JSON",
      options: [{name: "--sanitize", description: "redact sensitive transcript and file data"}],
      args: {name: "sessionID", description: "session id to export"},
    },
    {
      name: "import",
      description: "import session data from JSON file",
      args: {name: "file", description: "path to JSON file", template: "filepaths"},
    },
    {
      name: "github",
      description: "manage GitHub agent",
      subcommands: [
        {name: "install", description: "install the GitHub agent"},
        {
          name: "run",
          description: "run the GitHub agent",
          options: [
            {
              name: "--event",
              description: "GitHub mock event to run the agent for",
              args: {name: "event"},
            },
            {
              name: "--token",
              description: "GitHub personal access token (github_pat_********)",
              args: {name: "token"},
            },
          ],
        },
      ],
    },
    {
      name: "pr",
      description: "fetch and checkout a GitHub PR branch, then run opencode",
      args: {name: "number", description: "PR number to checkout"},
    },
    {
      name: "session",
      description: "manage sessions",
      subcommands: [
        {
          name: "list",
          description: "list sessions",
          options: [
            {
              name: ["-n", "--max-count"],
              description: "limit to N most recent sessions",
              args: {name: "max-count"},
            },
            {
              name: "--format",
              description: "output format",
              args: {name: "format", suggestions: ["table", "json"]},
            },
          ],
        },
        {
          name: "delete",
          description: "delete a session",
          args: {name: "sessionID", description: "session ID to delete"},
        },
      ],
    },
    {
      name: ["plugin", "plug"],
      description: "install plugin and update config",
      options: [
        {name: ["-g", "--global"], description: "install in global config"},
        {name: ["-f", "--force"], description: "replace existing plugin version"},
      ],
      args: {name: "module", description: "npm module name"},
    },
    {
      name: "db",
      description: "database tools",
      subcommands: [{name: "path", description: "print the database path"}],
      options: [
        {
          name: "--format",
          description: "Output format",
          args: {name: "format", suggestions: ["json", "tsv"]},
        },
      ],
      args: {name: "query", description: "SQL query to execute"},
    },
  ],
  options: [
    {name: ["-h", "--help"], description: "show help", isPersistent: true},
    {name: ["-v", "--version"], description: "show version number", isPersistent: true},
    {name: "--print-logs", description: "print logs to stderr", isPersistent: true},
    {
      name: "--log-level",
      description: "log level",
      args: {name: "log-level", suggestions: ["DEBUG", "INFO", "WARN", "ERROR"]},
      isPersistent: true,
    },
    {name: "--pure", description: "run without external plugins", isPersistent: true},
    {name: "--port", description: "port to listen on", args: {name: "port"}},
    {name: "--hostname", description: "hostname to listen on", args: {name: "hostname"}},
    {name: "--mdns", description: "enable mDNS service discovery (defaults hostname to 0.0.0.0)"},
    {
      name: "--mdns-domain",
      description: "custom domain name for mDNS service (default: opencode.local)",
      args: {name: "mdns-domain"},
    },
    {name: "--cors", description: "additional domains to allow for CORS", args: {name: "cors"}},
    {
      name: ["-m", "--model"],
      description: "model to use in the format of provider/model",
      args: {name: "model"},
    },
    {name: ["-c", "--continue"], description: "continue the last session"},
    {name: ["-s", "--session"], description: "session id to continue", args: {name: "session"}},
    {
      name: "--fork",
      description: "fork the session when continuing (use with --continue or --session)",
    },
    {name: "--prompt", description: "prompt to use", args: {name: "prompt"}},
    {name: "--agent", description: "agent to use", args: {name: "agent"}},
    {
      name: "--auto",
      description: "auto-approve permissions that are not explicitly denied (dangerous!)",
    },
    {name: "--mini", description: "start the minimal interactive interface"},
    {
      name: "--no-replay",
      description: "disable mini session history replay on resume and after resize",
    },
    {
      name: "--replay-limit",
      description: "cap visible mini replay to the newest N messages",
      args: {name: "replay-limit"},
    },
  ],
  args: {name: "project", description: "path to start opencode in"},
};
