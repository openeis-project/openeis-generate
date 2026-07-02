---
name: openeis-generate
description: Scaffold new projects and author reusable KDL project templates with openeis-generate. Use when generating a project from a local/git/archive template, OR when writing, debugging, or explaining a template.kdl, rhai hooks, conditional blocks, or Liquid placeholders.
---

# openeis-generate

A project/template generator whose configuration is **KDL** (`template.kdl`), a
focused port of `cargo-generate`. This skill covers the two things a coding agent
does with it: **generate a project from a template**, and **author a template**.

## The binary

It is **not on `PATH`** by default. Resolve it before running anything:

```sh
# if a release build exists:
BIN=target/release/openeis-generate
# otherwise build first:
cargo build --release && BIN=target/release/openeis-generate
# (or fall back to a system install on PATH: BIN=openeis-generate)
```

`openeis-generate` shells out to the system `git` for cloning and `git init`, so
`git` must be on `PATH`.

---

## Job 1 — Generate a project

Pick **exactly one** template source (they are mutually exclusive):

| Flag | Source |
|------|--------|
| `--path <dir>` | local directory |
| `--git <url\|owner/repo>` | clone a git repo (`--branch`/`--tag`/`--revision` to pin; mutually exclusive) |
| `--archive <file\|https-url>` | extract a local/remote `.zip`/`.tar.gz`/`.tgz`/`.tar.zst`/`.tzst` |
| `--favorite <name>` | a favorite defined in app config |
| _(positional)_ | a favorite name when no other source flag is given |
| `--subfolder <dir>` | use a subdirectory of the template (works with any source) |

**Always dry-run first** to see the plan and how variables resolve, with no side
effects:

```sh
"$BIN" --path example-templates/rust-cli --name my-cli --dry-run
```

**Non-interactive generation** (the normal agent path — never blocks on a prompt):

```sh
"$BIN" --path example-templates/rust-cli \
       --name my-cli \
       --silent \
       -D use_clap=true -D description="A CLI tool" \
       --destination /tmp/out
```

- `--silent` **requires `--name`** (it won't prompt). Every placeholder must then
  resolve from `-D key=value` (repeatable) or its `default`.
- `--values-file values.kdl` loads many variables at once; CLI `-D` wins on conflict.
- `--init` generates **in place into the current dir** with no subfolder and no VCS.
- `--force` keeps a non-kebab-case name as-is (otherwise `--name` is kebab-cased).
- `--allow-commands` (`-s`) is the **dangerous** switch that lets template rhai hooks
  run shell commands via `system::run`. Only set it for templates you have reviewed.
- `--dry-run` stays non-interactive and works without `--name`.

Interactive mode (no `--silent`) prompts for any unresolved placeholder using
dialoguer — fine for humans, **avoid it in agent runs**.

---

## Job 2 — Author a template

A template is a directory with a `template.kdl` and the files to render:

```
my-template/
├── template.kdl          # config (NEVER copied to output)
├── README.md.liquid      # .liquid suffix stripped on render → README.md
├── src/main.rs.liquid
└── .genignore            # optional; one glob per line (NEVER copied to output)
```

`template.kdl` has four optional sections — `template`, `placeholders`, `hooks`,
`conditional`. See `reference.md` for full detail; the **KDL rules below are
non-negotiable** (they cause parse/runtime errors).

---

## CRITICAL — KDL authoring rules

These come from the `kdl` 6.7.1 v2 parser. Violating them produces parse errors
or silent wrong behavior:

1. **Booleans are `#true` / `#false`, never bare `true`/`false`.** The v2 parser
   treats bare `true`/`false`/`null` as identifiers, not values. Write
   `default #true`, `init #false` (inline or own line). No `v1-fallback`.
2. **Lists are multi-argument nodes:** `include "a" "b" "c"`, `choices "x" "y"`.
   Repeated node names (`include "a"` then `include "b"`) do **not** deserialize.
3. **Conditional keys with inner quotes → use a KDL hash-string `#"…"#`.** A
   conditional key holds a rhai expression that usually contains its own string
   literals. Use `#"database != "sqlite""#` instead of the escaped
   `"database != \"sqlite\""`. Inner quotes need no escaping inside `#"…"#`.
4. **The config file is named `template.kdl`** (in the template root). It is never
   copied into the output. `.genignore` and `.git` are also never copied.

Minimal real config:

```kdl
template {
    include "README.md" "src/*"      # whitelist (multi-arg)
    exclude "src/unused.rs"          # remove (copied verbatim, NOT rendered)
    ignore "*.key" "*.env"           # extra ignores
    vcs "Git"                        # "Git" | "None" (default None)
    init #false                      # bool: run `git init` + initial commit
}

placeholders {
    author {
        type "string"                # "string" | "bool" | "array"
        prompt "Author name?"
        default "Alice"
    }
    license {
        type "string"
        prompt "License?"
        choices "MIT" "Apache-2.0"   # multi-arg list
    }
    use_ci {
        type "bool"
        prompt "Set up CI?"
        default #true                # NOTE: #true, not true
    }
}
```

---

## Built-in variables (reserved — cannot be placeholders)

These are seeded by the generator and available in templates, hooks, and
conditionals. **Do not declare a placeholder with any of these names.**

| Variable | Value |
|----------|-------|
| `name`, `project-name` | the `--name` value |
| `crate_name` | `--name` in `snake_case` |
| `crate_type` | always `"bin"` (no `--lib`/`--bin` flag) |
| `os-arch` | `"<os>-<arch>"` (e.g. `linux-x86_64`) |
| `is_init` | `"true"` if `--init`, else `"false"` |
| `within_cargo_project` | `"true"`/`"false"` |
| `authors`, `username` | discovered from git config / environment; **may be absent** |

> `name`/`project-name` are only seeded when `--name` is given. `authors`/
> `username` are best-effort — always provide a fallback (`{{ authors | default: "you" }}`).

---

## Liquid rendering

Parser = **full stdlib** (`{% if %}`, `{% for %}`, `contains`, `default`, …) **plus
8 case filters**. Renders in **file names and contents**. `.liquid` suffix is
stripped from the output path.

Case filters: `kebab_case`, `snake_case`, `pascal_case`, `lower_camel_case`,
`upper_camel_case`, `shouty_kebab_case`, `shouty_snake_case`, `title_case`.

```liquid
{{ name | kebab_case }}          {{ crate_name | shouty_snake_case }}
```

**Two traps:**

- **Bool values are the strings `"true"`/`"false"`.** Liquid treats every non-empty
  string (including `"false"`) as truthy, so `{% if use_clap %}` is **always** true.
  Compare explicitly: `{% if use_clap == "true" %}`.
- **Array placeholders arrive as a comma-joined string** (e.g. `"auth,logging"`).
  Test membership with Liquid `contains` (`{% if features contains "auth" %}`).

---

## Hooks (rhai)

Three phases, each entry a `.rhai` path relative to the template root or an inline
script:

| Phase | When | Working dir |
|-------|------|-------------|
| `init` | before variable collection | template dir |
| `pre` | after variables, before render | template dir |
| `post` | after render, before `git init` | **output dir** |

```kdl
hooks {
    init "setup.rhai"
    pre  "pre.rhai"
    post "post.rhai"
}
```

`pre`-hook `variable::set(...)` calls **flow into the rendering** — that's how a
hook derives extra variables. Variables are read by bare name in rhai.
`variable::get(name)` / `variable::set(name, value)` / `variable::is_set(name)`;
`file::exists|read|write|delete|rename` (sandboxed to working dir); `env::working_dir`,
`env::destination`; case-conversion functions (`to_snake_case`, `to_kebab_case`, …).

**Two traps:**

- **rhai identifiers cannot contain `-`.** So `os-arch`/`project-name` must be read
  as `variable::get("os-arch")` in hooks, and **cannot appear in `conditional`
  expressions at all** (use a non-hyphenated placeholder). Liquid has no such limit —
  `{{ os-arch }}` works.
- **`.rhai` scripts are regular template files and would be copied to the output.**
  Exclude them: `template { ignore "*.rhai" }`.

Multi-statement rhai scripts need `;` between statements.

---

## Conditionals

Merge a block's `placeholders`/`include`/`exclude`/`ignore` when the rhai
expression is true. Keys are re-evaluated as new placeholders appear, so they
chain. **Use a hash-string key** when the expression contains quotes:

```kdl
conditional {
    #"!("auth" in features)"# {
        ignore "src/modules/auth.rs"
    }
}
```

(`features` is an array → comma-joined string; rhai `in` works on it because the
name has no hyphen.)

---

## Validation checklist (run before declaring a template done)

1. `cargo build --release` — the binary builds.
2. `"$BIN" --path ./my-template --name demo --dry-run` — config parses; each
   placeholder shows `defined` / `default` / `would prompt` / `INVALID`.
3. `"$BIN" --path ./my-template --name demo --silent -D <every-var>=<val> --destination /tmp/out` — renders non-interactively.
4. Inspect `/tmp/out`: `template.kdl`, `.genignore`, `.git` are absent; `.liquid`
   suffixes stripped; bool-gated content correct.
5. For packaging, also run `"$BIN" package ./my-template -o /tmp/t.tar.zst` then
   round-trip it back through `--archive`.

**Canonical references:** the templates under `example-templates/` (`minimal`,
`rust-cli`, `feature-toggles`, `hooks-demo`, `package-demo`) each demonstrate one
feature and are copy-paste-accurate for KDL syntax. See `reference.md` for the
full CLI, hooks API, app-config/favorites, and packaging detail.
