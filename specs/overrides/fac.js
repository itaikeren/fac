// From `fac --help` and the argument parsers in src/main.rs and src/init/mod.rs.
// tests/fac_spec.rs fails when this and the help text drift apart.
const terminal = (name, description) => ({name, description});

export default {
  name: "fac",
  description: "Fig-style autocomplete for the macOS terminal",
  subcommands: [
    {
      name: "init",
      description: "Make new windows of your terminals start fac",
      options: [
        {
          name: "--terminals",
          description: "Terminals to set up, without asking: a comma-separated list, or all",
          args: {
            name: "terminals",
            suggestions: [
              terminal("terminal", "Terminal.app"),
              terminal("iterm2", "iTerm2"),
              terminal("ghostty", "Ghostty"),
              terminal("vscode", "VS Code"),
              terminal("zed", "Zed"),
              terminal("all", "Every terminal above"),
            ],
          },
        },
        {
          name: "--shell",
          description: "The shell to set up (default: your $SHELL)",
          args: {
            name: "shell",
            suggestions: [
              {name: "zsh", description: "zsh (~/.zshrc and ~/.zprofile)"},
              {name: "bash", description: "bash (your login file and ~/.bashrc)"},
              {name: "fish", description: "fish (conf.d/00-fac.fish)"},
            ],
          },
        },
      ],
    },
    {
      name: "enable",
      description: "Turn the startup block on; it keeps your choices",
    },
    {
      name: "disable",
      description: "Turn the startup block off; it keeps your choices",
    },
    {
      name: "uninstall",
      description: "Remove the block from every startup file it is in",
    },
    {
      name: "doctor",
      description: "Check fac's setup, say what is wrong, and turn safe mode off when all is well",
    },
    {
      name: "theme",
      description: "Make your own theme",
      subcommands: [
        {
          name: "new",
          description: "Write a theme file with fac's colours to start your own from",
          args: {name: "name"},
          options: [
            {name: "--light", description: "Start from the colours for a light terminal"},
            {name: "--stdout", description: "Print the file instead of writing it"},
          ],
        },
      ],
    },
    {
      name: "spec",
      description: "Your own completion specs",
      subcommands: [
        {
          name: "new",
          description: "Write a starter completion spec for a command",
          args: {name: "name", description: "The command the spec completes"},
          options: [
            {
              name: "--js",
              description: "Write JavaScript instead of TypeScript",
              exclusiveOn: ["--json"],
            },
            {
              name: "--json",
              description: "Write JSON instead of TypeScript",
              exclusiveOn: ["--js"],
            },
          ],
        },
        {
          name: "check",
          description: "Load a spec as fac would and say what is in it or what is wrong",
          args: {
            name: "name or path",
            description: "A spec name or a file; with none, every spec you have",
            isOptional: true,
            template: "filepaths",
            generators: {
              // `fac spec list` prints "<name>.<ext>  <what it does>" per spec,
              // or "No specs in <dir>. ..." when there are none.
              script: ["fac", "spec", "list"],
              postProcess: (out) =>
                out
                  .split("\n")
                  .map((line) => /^(\S+)\.(?:ts|js|json)  /.exec(line))
                  .filter(Boolean)
                  .map((m) => ({name: m[1], description: "Your spec", icon: "fig://icon?type=file"})),
            },
          },
        },
        {name: "list", description: "List your specs and the spec of fac's each one replaces"},
      ],
    },
  ],
  options: [
    {name: ["--help", "-h"], description: "Show the help"},
    {name: ["--version", "-V"], description: "Show the version"},
  ],
};
