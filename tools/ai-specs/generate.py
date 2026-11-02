#!/usr/bin/env python3
"""Build specs/overrides/<name>.js from a tool's own --help output.

Called by refresh.sh, one tool at a time. Standard library only.

    generate.py --tool claude --label "Claude Code" --format commander \
        --out specs/overrides/claude.js --date 2026-11-02 \
        --env HOME=/scratch/home --env PATH=/usr/bin:/bin -- /scratch/bin/claude

Only `--version`, `--help` and `<sub> --help` are ever run, under `env -i`.
The output is written to a temporary file and moved into place at the end,
so a failure never leaves a half-made spec. If the fetched version is the one
in the existing file's header the file is left alone (unless --force).
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor

WIDTH = 101
HUG_WIDTH = 111
ARGS_WIDTH = 107
DESC_LIMIT = 120
CUT_AT = 100
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07")


# --------------------------------------------------------------------------
# Running the tool


class Runner:
    def __init__(self, cmd, env):
        self.cmd = cmd
        self.env = env
        self.cache = {}

    def run(self, words):
        key = tuple(words)
        if key in self.cache:
            return self.cache[key]
        if os.environ.get("AI_SPECS_TRACE"):
            print("run", " ".join(words), file=sys.stderr)
        try:
            p = subprocess.run(
                ["env", "-i"] + self.env + self.cmd + list(words),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=30,
            )
            # help goes to stdout; stderr is only a fallback (uvx chatters there)
            out = p.stdout.decode("utf-8", "replace") or p.stderr.decode("utf-8", "replace")
        except subprocess.TimeoutExpired:
            out = ""
        out = ANSI.sub("", out).replace("\r", "")
        for item in self.env:
            if item.startswith("HOME=") and len(item) > 8:
                # defaults printed from the scratch HOME are the user's ~ in real life
                out = out.replace(item[5:] + "/", "~/")
        self.cache[key] = out
        return out


# --------------------------------------------------------------------------
# Splitting help text into sections and entries


HEADER = re.compile(r"^([A-Za-z][^:]*):\s*$")


def sections(text):
    """[(title, [lines])] for every column-0 'Title:' line."""
    found = []
    cur = None
    for line in text.split("\n"):
        m = HEADER.match(line)
        if m and not line.startswith(" "):
            cur = (m.group(1).strip(), [])
            found.append(cur)
        elif cur is not None:
            cur[1].append(line)
    return found


def kind_of(title, fmt=None):
    t = title.lower().strip()
    if t in ("commands", "command", "subcommands", "available commands"):
        return "commands"
    if t in ("arguments", "positionals", "positional arguments", "positional"):
        return "args"
    if fmt == "argparse":
        return "options"
    if t.endswith("options") or t in ("flags", "option"):
        return "options"
    return "ignore"


OPTION_START = re.compile(r"^ {2,6}-{1,2}[A-Za-z0-9]")


def entries(lines, kind):
    """[(spec, description-text)] for a section body."""
    body = [l for l in lines if l.strip()]
    if not body:
        return []
    base = min(len(l) - len(l.lstrip()) for l in body)
    out = []
    cur = None
    for line in lines:
        if not line.strip():
            continue
        indent = len(line) - len(line.lstrip())
        if indent == 0:
            continue
        if kind == "options":
            starts = bool(OPTION_START.match(line)) and indent <= 6 and (indent == base or indent <= base + 4)
        else:
            starts = indent == base
        if starts:
            m = re.match(r"^\s*(\S.*?)(\s{2,})(\S.*)$", line)
            if m:
                spec, desc = m.group(1), m.group(3)
                col = line.index(desc, indent)
            else:
                spec, desc, col = line.strip(), "", None
            cur = {"spec": spec, "parts": [desc] if desc else [], "col": col}
            out.append(cur)
        elif cur is not None:
            if kind != "commands" or cur["col"] is None or (indent >= cur["col"] - 1 and not cur.get("nested")):
                cur["parts"].append(line.strip())
            else:
                # a nested command listed under its parent (found by recursion), and
                # whatever wraps below it, is not part of this description
                cur["nested"] = True
    return [(e["spec"], "\n".join(e["parts"]).strip()) for e in out]


# --------------------------------------------------------------------------
# Text helpers


def squash(s):
    return re.sub(r"\s+", " ", s).strip()


def first_sentence(s):
    depth = 0
    for i, ch in enumerate(s):
        if ch in "([":
            depth += 1
        elif ch in ")]":
            depth = max(0, depth - 1)
        elif ch == "." and depth == 0 and i + 2 < len(s) and s[i + 1] == " " and (s[i + 2].isupper() or s[i + 2] in "`'\""):
            return s[:i], True
    return s, False


def shorten(desc, first_only=False):
    """One short description line.

    first_only (clap tools): the first sentence, no final period. Otherwise
    the whole text, cut to its first sentence or to DESC_LIMIT when longer.
    """
    desc = re.sub(r"Possible values:.*", "", desc, flags=re.S)
    desc = squash(desc)
    desc = desc.replace("~/ ", "~/")
    desc = re.sub(r"\s*\[env var: [^\]]*\]", "", desc)
    desc = re.sub(r"\s*\[aliases?: [^\]]*\]", "", desc)
    desc = squash(desc)
    sentence, cut = first_sentence(desc)
    if first_only:
        desc = sentence.rstrip(".")
    elif len(desc) > DESC_LIMIT and cut:
        desc = sentence + "."
    if len(desc) > DESC_LIMIT:
        cut_at = desc.rfind(" ", 0, CUT_AT)
        desc = desc[: cut_at if cut_at > 0 else CUT_AT].rstrip(" ,;:") + "..."
    return desc


def alias_names(desc):
    m = re.search(r"\[aliases?: ([^\]]*)\]", desc)
    if not m:
        return []
    return [a.strip() for a in m.group(1).split(",") if a.strip()]


YARGS_TAG = re.compile(r"\s*\[(boolean|string|number|array|count|required|deprecated)\]\s*$")
YARGS_META = re.compile(r"\s*\[(default|choices|aliases?|deprecated): ?((?:\[\]|[^\]])*)\]\s*$")


def split_yargs_tags(desc):
    """(description, types, choices, default-removed) from a yargs-style tail."""
    types = []
    choices = None
    while True:
        m = YARGS_TAG.search(desc)
        if m:
            types.append(m.group(1))
            desc = desc[: m.start()]
            continue
        m = YARGS_META.search(desc)
        if m:
            if m.group(1) == "choices":
                choices = re.findall(r'"([^"]*)"|([^\s,]+)', m.group(2))
                choices = [a or b for a, b in choices]
            elif m.group(1).startswith("alias"):
                desc += " [aliases: " + m.group(2) + "]"
                break
            desc = desc[: m.start()]
            continue
        break
    return desc.strip(), types, choices


# --------------------------------------------------------------------------
# Arguments and options


CLICK_TYPES = {"text", "integer", "float", "boolean", "path", "file", "directory", "filename", "uuid"}
NESTED_TOKEN = r"<[^>]+>(?:\.\.\.)?|\[(?:[^\[\]]|\[[^\]]*\])*\](?:\.\.\.)?|\{[^}]+\}|[A-Za-z_][^\s]*(?:\.\.\.)?"
UPPER = re.compile(r"^[A-Z][A-Z0-9_]*$")
VALUE_TOKEN = re.compile(r"(<[^>]+>|\[[^\]]+\]|\{[^}]+\}|[A-Za-z_][\w=:.\-]*)(\.\.\.|\.\.)?")


def clean_name(raw):
    name = raw.strip("<>[]{}").rstrip(".")
    if UPPER.match(name):
        name = name.lower()
    return name


def template_for(name, opt_name=""):
    if re.search(r"(^|-)(dir|dirs|directory|directories|folder|folders)$", opt_name.lower()):
        return "folders"
    n = name.lower()
    if re.search(r"(^|[-_])(dir|dirs|directory|directories|folder|folders|cwd)$", n):
        return "folders"
    if re.search(r"(^|[-_])(file|files|path|paths|filepath)$", n):
        return "filepaths"
    return None


def make_arg(token, variadic=False, force_optional=None, desc=None, opt_name=""):
    optional = token.startswith("[")
    if force_optional is not None:
        optional = force_optional
    raw = token
    if raw.startswith("{"):
        choices = [c.strip() for c in raw.strip("{}").split(",") if c.strip()]
        name = opt_name.lstrip("-") or "value"
        arg = {"name": name}
        if desc:
            arg["description"] = desc
        arg["suggestions"] = choices
        return arg
    name = clean_name(raw)
    pipe = None
    if "|" in name:
        parts = name.split("|")
        if opt_name:
            name = opt_name.lstrip("-")
            pipe = [x.lower() for x in parts]
    arg = {"name": name}
    if desc:
        arg["description"] = desc
    if optional:
        arg["isOptional"] = True
    if variadic or raw.endswith("..."):
        arg["isVariadic"] = True
    if pipe:
        arg["suggestions"] = pipe
    t = template_for(name, opt_name)
    if t:
        arg["template"] = t
    return arg


def finalize_arg(arg):
    order = ["name", "description", "isOptional", "isVariadic", "suggestions", "template"]
    return {k: arg[k] for k in order if k in arg}


def list_choices(desc):
    """'(local, user, or project)' in a description: a fixed list of plain words."""
    for m in re.finditer(r"\(([^()]+)\)", squash(desc)):
        body = m.group(1)
        if body.lower().startswith(("e.g", "default", "i.e", "for ", "such as")):
            continue
        items = [x.strip() for x in re.split(r",\s*(?:or\s+)?|\s+or\s+", body) if x.strip()]
        if 2 <= len(items) <= 8 and all(re.fullmatch(r"[a-z][\w-]{0,20}", x) for x in items):
            return items
    return None


def pipe_choices(desc):
    m = re.search(r"(?<![\w-])([\w-]+(?: \| [\w-]+)+)", desc)
    if m:
        return m.group(1).split(" | ")
    return None


def paren_choices(desc):
    desc = squash(desc)
    m = re.search(r'\(choices: ((?:"[^"]*"(?:, )?)+)', desc)
    if m:
        return re.findall(r'"([^"]*)"', m.group(1))
    return None


def parse_option(spec, desc, fmt):
    """List of option dicts (usually one) for a spec line and its description."""
    raw_desc = desc
    types = []
    choices = None
    if fmt == "yargs":
        desc, types, choices = split_yargs_tags(desc)
    if fmt == "clap":
        m = re.search(r"\[possible values: ([^\]]*)\]", desc)
        if m:
            choices = [v.split(":")[0].strip() for v in m.group(1).split(",") if v.strip()]
        m = re.search(r"Possible values:\s*\n(.*)", desc, re.S)
        if m:
            found = []
            for line in m.group(1).split("\n"):
                lm = re.match(r"\s*-\s+([^\s:]+)(?::\s*(.*))?$", line)
                if lm:
                    d0 = re.sub(r"\s*\[default: [^\]]*\]", "", lm.group(2) or "").strip()
                    found.append({"name": lm.group(1), "description": d0} if d0 else lm.group(1))
                elif found and line.strip() and not line.strip().startswith("["):
                    if isinstance(found[-1], dict):
                        found[-1]["description"] += " " + line.strip()
            if found:
                choices = found
        desc = re.sub(r"\s*\[(possible values|default): [^\]]*\]", "", desc)
    aliases = alias_names(desc)
    pieces = [p.strip() for p in re.split(r",\s+(?=-)", spec)]
    names = []
    value_tokens = []
    repeatable = False
    for piece in pieces:
        if piece.endswith("..."):
            piece = piece[:-3]
            repeatable = True
        m = re.match(r"^(-{1,2}[A-Za-z0-9][\w-]*)(?:[ =](.*))?$", piece)
        if not m:
            continue
        names.append(m.group(1))
        rest = (m.group(2) or "").strip()
        if rest and not value_tokens:
            value_tokens = re.findall(NESTED_TOKEN, rest)
    for a in aliases:
        for part in re.split(r",\s*", a):
            part = part.strip()
            if part.startswith("-") and part not in names:
                names.append(part)
    if not names:
        return []
    # --foo / --no-foo written together (argparse BooleanOptionalAction)
    if (
        len(names) == 2
        and names[1].startswith("--no-")
        and names[0].startswith("--")
        and names[1] == "--no-" + names[0][2:]
        and not value_tokens
    ):
        d = shorten(desc, fmt == "clap")
        out = []
        for me, other in ((names[0], names[1]), (names[1], names[0])):
            o = {"name": me}
            if d:
                o["description"] = d
            o["exclusiveOn"] = [other]
            out.append(o)
        return out
    opt = {"name": names[0] if len(names) == 1 else names}
    d = shorten(desc, fmt == "clap")
    if d:
        opt["description"] = d
    long_name = next((n for n in names if n.startswith("--")), names[0])
    args = []
    variadic_any = False
    expanded = []
    for tok in value_tokens:
        if tok.startswith("<") and " " in tok.strip("<>."):
            expanded += ["<%s>" % w for w in tok.strip("<>.").split()]  # click: <TEXT TEXT>
        else:
            expanded.append(tok)
    value_tokens = expanded
    for tok in value_tokens:
        variadic = "..." in tok
        variadic_any = variadic_any or variadic
        a = make_arg(tok, variadic=variadic, opt_name=long_name)
        if fmt == "click" and len(value_tokens) == 1 and a["name"] in CLICK_TYPES:
            # click prints the type, not a name: use the option's own name
            a["name"] = long_name.lstrip("-") if a["name"] != "file" else "file"
            if a["name"] != "file" and not (a.get("template")):
                t = template_for(a["name"])
                if t:
                    a["template"] = t
        args.append(a)
    if fmt == "yargs" and not args:
        scalar = [t for t in types if t in ("string", "number", "array")]
        if scalar:
            a = {"name": long_name.lstrip("-")}
            t = template_for(a["name"])
            if t:
                a["template"] = t
            args.append(a)
    ch = choices or paren_choices(raw_desc)
    if args and ch:
        args[0]["suggestions"] = ch
    elif args and fmt != "yargs":
        pc = pipe_choices(raw_desc) or list_choices(raw_desc)
        if pc:
            args[0]["suggestions"] = pc
    if args:
        opt["args"] = [finalize_arg(a) for a in args] if len(args) > 1 else finalize_arg(args[0])
    if re.search(r"\(repeatable|repeatable\)|can be used multiple times|or multiple\b", raw_desc, re.I) or repeatable:
        opt["isRepeatable"] = True
    return [opt]


def option_key(opt):
    n = opt["name"]
    return tuple(n) if isinstance(n, list) else (n,)


def parse_arg_entry(spec, desc, fmt=None):
    token = spec.strip().split()[0] if spec.strip() else ""
    variadic = spec.strip().endswith("...") or spec.strip().endswith("..")
    base = token.rstrip(".")
    if not base.startswith(("<", "[")):
        base = "<" + base + ">"
    choices = None
    if fmt == "clap":
        m = re.search(r"\[possible values: ([^\]]*)\]", desc)
        if m:
            choices = [v.split(":")[0].strip() for v in m.group(1).split(",") if v.strip()]
        desc = re.sub(r"\s*\[(possible values|default): [^\]]*\]", "", desc)
    d = shorten(desc)
    if d.endswith("[string]"):
        d = d[: -len("[string]")].strip()
    arg = make_arg(base, variadic=variadic, desc=d or None)
    choices = choices or list_choices(desc)
    if choices:
        arg["suggestions"] = choices
    return arg


# --------------------------------------------------------------------------
# One command's help text


SKIP_TOKENS = {"[options]", "[command]", "<command>", "[args]", "[ARGS]...", "[COMMAND]", "<COMMAND>", "[default]"}


def parse_command_help(text, fmt, path, binname):
    """Parse one help page: subcommand entries, options, positional args."""
    subs = []
    options = []
    args = []
    foreign = []
    for title, lines in sections(text):
        kind = kind_of(title, fmt)
        if kind == "ignore":
            continue
        for spec, desc in entries(lines, kind):
            if kind == "commands":
                if fmt == "yargs" and path and spec.split()[: 1 + len(path)] != [binname] + list(path):
                    foreign.append(spec)
                s = parse_listing(spec, desc, fmt, path, binname)
                if s:
                    subs.append(s)
            elif kind == "args":
                if fmt == "yargs":
                    d, types, _ = split_yargs_tags(desc)
                    desc = d
                args.append((spec, desc))
            else:
                options.extend(parse_option(spec, desc, fmt))
    if foreign:
        # a yargs leaf that prints the root help: none of it belongs to this command
        return [], [], []
    return subs, options, args


def parse_listing(spec, desc, fmt, path, binname):
    words = spec.split()
    if fmt == "yargs" and path and words[: 1 + len(path)] != [binname] + list(path):
        return None  # a yargs leaf prints its parent's commands: not ours
    drop = [binname] + list(path)
    while words and words and drop and words[0] == drop[0]:
        words.pop(0)
        drop.pop(0)
    if not words:
        return None
    first = words[0]
    if first.startswith(("[", "<")):
        return None  # yargs default command
    default = first.endswith("*")
    first = first.rstrip("*")
    if first == "help":
        return None
    names = [n for n in first.split("|") if n]
    if not all(re.match(r"^[a-z0-9][\w.:-]*$", n) for n in names):
        return None
    desc2, _, _ = split_yargs_tags(desc) if fmt == "yargs" else (desc, None, None)
    for a in alias_names(desc2):
        if a not in names and not a.startswith("-"):
            names.append(a)
    tokens = [w for w in words[1:] if w not in SKIP_TOKENS]
    return {"names": names, "desc": desc2, "tokens": tokens, "default": default}


def usage_tokens(text, path, binname, fmt):
    """Positional tokens from the Usage line (click/argparse style)."""
    m = re.search(r"^[Uu]sage:\s+(.*(?:\n {2,}.*)*)", text, re.M)
    if not m:
        return []
    usage = squash(m.group(1))
    words = usage.split()
    drop = [binname] + list(path)
    while words and drop and words[0] == drop[0]:
        words.pop(0)
        drop.pop(0)
    out = []
    for w in words:
        if w in ("[OPTIONS]", "[options]", "COMMAND", "[COMMAND]", "COMMAND...", "[ARGS]...", "[ARGS]"):
            continue
        out.append(w)
    return out


# --------------------------------------------------------------------------
# Tree


class Builder:
    def __init__(self, runner, fmt, binname, tool):
        self.runner = runner
        self.fmt = fmt
        self.binname = binname
        self.tool = tool

    def help_words(self, path):
        return list(path) + ["--help"]

    def build(self, path):
        text = self.runner.run(self.help_words(path))
        subs, options, arg_entries = parse_command_help(text, self.fmt, path, self.binname)
        node = {"names": [path[-1]] if path else [self.tool], "options": options, "subs": [], "args": []}
        arg_desc = {}
        for spec, desc in arg_entries:
            arg_desc[clean_name(spec.strip().split()[0].rstrip("."))] = (spec, desc)

        # positional arguments
        if self.fmt in ("click",):
            toks = usage_tokens(text, path, self.binname, self.fmt)
            node["args"] = [self.token_arg(t, arg_desc) for t in toks if not t.startswith("-")]
        else:
            node["args"] = [parse_arg_entry(spec, desc, self.fmt) for spec, desc in arg_entries]
        node["_text"] = text
        node["_subs"] = subs
        return node

    def token_arg(self, tok, arg_desc):
        variadic = tok.endswith("...") or tok.endswith("..")
        base = tok.rstrip(".")
        optional = base.startswith("[")
        name = clean_name(base)
        a = {"name": name}
        if name in arg_desc and arg_desc[name][1]:
            a["description"] = shorten(arg_desc[name][1])
        if optional:
            a["isOptional"] = True
        if variadic:
            a["isVariadic"] = True
        t = template_for(name)
        if t:
            a["template"] = t
        return finalize_arg(a)


def build_tree(builder, path, listing=None, parent_names=None):
    node = builder.build(path)
    subs = node.pop("_subs")
    names_here = [x["names"][0] for x in subs]
    if len(path) > 6 or (parent_names is not None and names_here == parent_names):
        subs = []  # a leaf whose help just repeats its parent's command list
        node["options"] = []
        node["args"] = []
    text = node.pop("_text")
    # arguments given only in the listing line, e.g. "login <identifier>"
    if listing and not node["args"] and builder.fmt in ("commander", "clap", "yargs"):
        node["args"] = [
            make_arg(t.replace("..", "...") if t.endswith("..") else t, variadic=t.endswith(".."))
            for t in listing["tokens"]
        ]
    if listing and builder.fmt in ("commander", "clap", "yargs"):
        # fill in descriptions of listing-line args from an Arguments section
        pass
    seen = set()
    todo = []
    for s in subs:
        if s["names"][0] in seen:
            continue
        seen.add(s["names"][0])
        todo.append(s)
    results = []
    # build in parallel one level at a time; deeper levels run in their own call
    with ThreadPoolExecutor(max_workers=6) as pool:
        futures = [pool.submit(build_tree, builder, path + [s["names"][0]], s, names_here) for s in todo]
        for s, f in zip(todo, futures):
            child = f.result()
            child["names"] = s["names"]
            child["desc"] = s["desc"]
            child["default"] = s["default"]
            results.append(child)
    node["subs"] = results
    return node


# --------------------------------------------------------------------------
# Post-processing


def merge_default(root):
    """click: the default command's options and arguments also work on the root."""
    for sub in root["subs"]:
        if sub.get("default"):
            have = {k for o in root["options"] for k in option_key(o)}
            for o in sub["options"]:
                if not (set(option_key(o)) & have):
                    root["options"].append(o)
            if not root["args"]:
                root["args"] = sub["args"]


def walk(node):
    yield node
    for c in node["subs"]:
        yield from walk(c)


def okey(o):
    return (tuple(sorted(option_key(o))), o.get("description"))


def drop_globals(root):
    """Root options that half the subcommands (at least three) repeat are global flags: keep them on the root only."""
    count = {}
    for n in walk(root):
        if n is root:
            continue
        for o in n["options"]:
            count[okey(o)] = count.get(okey(o), 0) + 1
    need = max(3, (len(root["subs"]) + 1) // 2)
    glob = {okey(o) for o in root["options"] if count.get(okey(o), 0) >= need}
    for n in walk(root):
        if n is not root:
            n["options"] = [o for o in n["options"] if okey(o) not in glob]


def drop_help(root):
    """-h/--help on every subcommand is noise; the root's is persistent."""
    for n in walk(root):
        if n is root:
            continue
        n["options"] = [o for o in n["options"] if "--help" not in option_key(o)]
    if root["subs"]:
        for o in root["options"]:
            if "--help" in option_key(o):
                o["isPersistent"] = True


def mark_persistent(node):
    """An option every subcommand of a command repeats is persistent: say so once, on the parent."""
    subs = node["subs"]
    if subs:
        keys = []
        for o in node["options"]:
            key = set(option_key(o))
            with_options = [s for s in subs if s["options"]]
            if with_options and all(any(key & set(option_key(x)) for x in s["options"]) for s in with_options):
                o["isPersistent"] = True
                keys.append(key)
        for s in subs:
            s["options"] = [x for x in s["options"] if not any(set(option_key(x)) & k for k in keys)]
    for s in subs:
        mark_persistent(s)


def apply_patches(root, patches):
    for p in patches:
        node = root
        for step in p.get("path", []):
            node = next(s for s in node["subs"] if step in s["names"])
        if "option" in p:
            opt = next(o for o in node["options"] if p["option"] in option_key(o))
            opt.update(p["set"])
        elif "add_subcommand" in p:
            node["subs"].append(p["add_subcommand"])
        else:
            node.update(p["set"])


# --------------------------------------------------------------------------
# JavaScript output


IDENT = re.compile(r"^[A-Za-z_$][\w$]*$")


def js_str(s):
    return json.dumps(s, ensure_ascii=False)


def inline(v):
    if isinstance(v, dict):
        if not v:
            return "{}"
        return "{" + ", ".join(f"{k if IDENT.match(k) else js_str(k)}: {inline(x)}" for k, x in v.items()) + "}"
    if isinstance(v, list):
        return "[" + ", ".join(inline(x) for x in v) + "]"
    if isinstance(v, bool):
        return "true" if v else "false"
    return js_str(v)


def emit(v, indent, prefix_len, hug=False):
    """Prettier-like: one line when it fits in WIDTH, else one item per line."""
    one = inline(v)
    limit = WIDTH
    if isinstance(v, list) and len(v) == 1 and isinstance(v[0], dict):
        limit = HUG_WIDTH  # a lone object hugs its brackets
    if hug:
        limit = ARGS_WIDTH
    if not isinstance(v, (dict, list)) or indent + prefix_len + len(one) + 1 <= limit:
        return one
    pad = " " * (indent + 2)
    if isinstance(v, dict):
        lines = []
        for k, x in v.items():
            key = (k if IDENT.match(k) else js_str(k)) + ": "
            lines.append(pad + key + emit(x, indent + 2, len(key), k == "args" and isinstance(x, dict)) + ",")
        return "{\n" + "\n".join(lines) + "\n" + " " * indent + "}"
    lines = [pad + emit(x, indent + 2, 0) + "," for x in v]
    return "[\n" + "\n".join(lines) + "\n" + " " * indent + "]"


def to_spec(node, root=False, tool=None, fmt=None):
    out = {}
    names = node["names"]
    out["name"] = tool if root else (names[0] if len(names) == 1 else names)
    d = shorten(node.get("desc", ""), fmt == "clap") if not root else ""
    if d:
        out["description"] = d
    if node["subs"]:
        out["subcommands"] = [to_spec(c, fmt=fmt) for c in node["subs"]]
    if node["options"]:
        out["options"] = [order_option(o) for o in node["options"]]
    args = node["args"]
    if args:
        out["args"] = args[0] if len(args) == 1 else args
    return out


def order_option(o):
    order = ["name", "description", "args", "isRepeatable", "exclusiveOn", "isPersistent"]
    return {k: o[k] for k in order if k in o}


# --------------------------------------------------------------------------


def header_version(path):
    try:
        with open(path, encoding="utf-8") as f:
            first = f.readline()
    except OSError:
        return None
    m = re.search(r", (.+?), \d{4}-\d{2}-\d{2}\.\s*$", first)
    return m.group(1) if m else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tool", required=True)
    ap.add_argument("--label", required=True)
    ap.add_argument("--format", required=True, choices=["commander", "clap", "yargs", "argparse", "click"])
    ap.add_argument("--out", required=True)
    ap.add_argument("--date", required=True)
    ap.add_argument("--env", action="append", default=[])
    ap.add_argument("--patches", default=None)
    ap.add_argument("--force", action="store_true")
    ap.add_argument("cmd", nargs="+")
    a = ap.parse_args()

    runner = Runner(a.cmd, a.env)
    vtext = runner.run(["--version"]).strip()
    m = re.search(r"\b\d+(?:\.\d+)+[\w.\-]*", vtext)
    if not m:
        print(f"{a.tool}: no version in --version output", file=sys.stderr)
        return 3
    version = f"{a.label} {m.group(0)}"
    if not a.force and header_version(a.out) == version:
        print(f"{a.tool}: {version} (unchanged)")
        return 0

    builder = Builder(runner, a.format, a.tool, a.tool)
    root = build_tree(builder, [])
    root["names"] = [a.tool]
    if not root["options"] and not root["subs"]:
        print(f"{a.tool}: the help output had no options or subcommands", file=sys.stderr)
        return 4
    if a.format == "click":
        merge_default(root)
    drop_help(root)
    mark_persistent(root)
    drop_globals(root)
    if a.patches and os.path.exists(a.patches):
        with open(a.patches, encoding="utf-8") as f:
            apply_patches(root, json.load(f).get(a.tool, []))

    spec = to_spec(root, root=True, tool=a.tool, fmt=a.format)
    body = f"// From {a.tool} --help and the help of each subcommand, {version}, {a.date}.\n"
    body += "export default " + emit(spec, 0, len("export default ")) + ";\n"

    fd, tmp = tempfile.mkstemp(prefix=".spec-", suffix=".tmp", dir=os.path.dirname(os.path.abspath(a.out)))
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write(body)
    os.replace(tmp, a.out)
    print(f"{a.tool}: {version} written")
    return 0


if __name__ == "__main__":
    sys.exit(main())
