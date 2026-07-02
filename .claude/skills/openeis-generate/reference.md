# openeis-generate — reference

Detailed reference. `SKILL.md` covers daily use and the KDL gotchas; read this
when you need the full flag list, the complete hooks API, or packaging detail.
All facts here were verified against the source as of 2026-06.

## Top-level commands

```
openeis-generate [OPTIONS] [AUTO_PATH]       # default = generate flow
openeis-generate package [OPTIONS] [PATH]    # bundle a template into an archive
```

`arg_required_else_help(true)`: no args prints help. A bare positional `AUTO_PATH`
is a favorite name (when no `--git`/`--path`/`--archive`/`--favorite` is given) or a
subfolder (when one of those is given).

## CLI reference (generate)

**Template Selection** (mutually exclusive group `SpecificPath`, except `--subfolder`):

| Flag | Notes |
|------|-------|
| `--git <url\|owner/repo\|path>` | clone via system `git` |
| `--path <dir>` | local directory |
| `--archive <file\|url>` | `.zip`/`.tar.gz`/`.tgz`/`.tar.zst`/`.tzst`, local or HTTP(S) |
| `--favorite <name>` | from app config |
| `[AUTO_PATH]` | favorite name (no source flag) |
| `--subfolder <dir>` | subdir of the template, composes with any source |

**Git Parameters** (mutually exclusive with each other): `--branch`/`-b`, `--tag`/`-t`,
`--revision`/`-r`.

**Output Parameters:**

| Flag | Effect |
|------|--------|
| `-n, --name <NAME>` | project name / output dir; kebab-cased unless `--force` |
| `-f, --force` | keep name as-is (don't kebab-case) |
| `-D, --define <KEY=VALUE>` | set a variable; repeatable; beats `--values-file` |
| `--values-file <FILE>` | KDL `values { key "val" }`, arrays comma-joined |
| `--vcs <Git\|None>` | overrides the template's `vcs` |
| `--init` | generate in place (current dir, no subfolder, **no VCS**) |
| `--destination <PATH>` | output directory (default: `<name>`) |
| `--overwrite` | overwrite existing files in destination |
| `--force-git-init` | fresh `git init` even if destination is already a repo |
| `-s, --allow-commands` | **dangerous**: lets hooks run `system::run(cmd)` |

**Other:**

| Flag | Effect |
|------|--------|
| `-c, --config <FILE>` | app config (default `~/.config/openeis/openeis.kdl`) |
| `--list-favorites` | print favorites, then exit |
| `--dry-run` | print the plan + variable resolution, write nothing, non-interactive |
| `--silent` | **requires `--name`**; never prompts; unresolved placeholder w/o default = error |
| `-v, --verbose` / `-q, --quiet` | mutually exclusive |
| `--continue-on-error` | keep going on template errors |
| `--allow-commands` | (see Output Parameters — repeated for visibility) |

### `run()` flow (why things happen in this order)

```
load app config → (list-favorites? exit) → prepare_source (clone/extract/copy)
→ merged_defines  (--define ∪ --values-file, --define wins)
→ [dry-run? print plan + variable summary, exit]
→ resolve name (prompt if omitted) → resolve destination
→ init hooks      (template dir, before variables)
→ conditional::collect  (seed built-ins, resolve base+conditional placeholders, merge filters)
→ pre hooks       (template dir; variable::set flows into rendering)
→ generate::expand (Liquid render names+contents; filters; verbatim binaries)
→ post hooks      (output dir, before VCS init)
→ init VCS        (skip for --init)
```

`--dry-run` branches **before** name resolution, so it's non-interactive and
name-optional. `--init` skips VCS entirely.

## App config & favorites

`~/.config/openeis/openeis.kdl` (override with `-c`). Favorites:

```kdl
favorites {
    my-tmpl {
        description "my template"
        git "https://example.com/t.git"
        branch "main"
        vcs "Git"
        init #true
    }
}
```

Then `openeis-generate my-tmpl --name app` (or `--favorite my-tmpl`). A favorite
needs `git` **or** `path`. `--list-favorites` prints what's defined.

## `template.kdl` sections in full

### `template`

```kdl
template {
    generator_version ">=0.1.0"   # optional semver requirement
    include "a" "b"               # whitelist (multi-arg list)
    exclude "target"              # matched file copied VERBATIM (not rendered)
    ignore "*.key"                # additional ignores (not copied at all)
    vcs "Git"                     # "Git" | "None" (default None)
    init #false                   # bool: git init + initial commit
}
```

Semantics of the three filter lists:

- `include` — **whitelist**. If present, only matched files are processed.
- `exclude` — matched files are **copied verbatim, NOT rendered** (Liquid left
  untouched). This is cargo-generate semantics, not "drop".
- `ignore` — matched files are **dropped entirely**.
- `.genignore` (template root, one glob per line, `#` comments / blanks skipped)
  is merged into the ignore set, **and** is itself never copied.

Binary files are always copied verbatim (content sniff). `template.kdl`,
`.genignore`, and `.git` are never copied.

### `placeholders`

```kdl
placeholders {
    edition {
        type "string"                       # "string" | "bool" | "array"
        prompt "Edition?"
        choices "2021" "2024"               # multi-arg (string/array)
        default "2024"
    }
    semver_tag {
        type "string"
        prompt "Tag?"
        regex "^v[0-9]+\\.[0-9]+\\.[0-9]+$"  # validated against the provided value
    }
    features {
        type "array"                        # multi-select
        prompt "Modules?"
        choices "auth" "logging" "graphql"
        default "auth" "logging"            # array default = multi-arg
    }
    use_ci {
        type "bool"
        prompt "CI?"
        default #true
    }
}
```

- `type "array"` = multi-select; value reaches templates as a **comma-joined
  string** (`"auth,logging"`). `--define features=auth,logging` does the same.
- `default` for an array is **multi-arg** (`default "a" "b"`); for a scalar it's a
  single value. Every default must be among `choices` when `choices` constrains it.
- Reserved names (cannot be placeholders): `authors`, `os-arch`, `project-name`,
  `crate_name`, `crate_type`, `within_cargo_project`, `is_init`, `username`.

### `hooks`

```kdl
hooks {
    init "setup.rhai"      # path relative to template root, OR inline script
    pre  "pre.rhai"
    post "post.rhai"
}
```

### `conditional`

```kdl
conditional {
    #"lang == \"rust\"" {              # prefer hash-string: #"lang == "rust""#
        include "rust-only/*"
        placeholders {
            edition { type "string"; prompt "Edition?"; default "2024" }
        }
    }
}
```

Re-evaluated as new placeholders appear (chaining). A block merges its
`include`/`exclude`/`ignore`/`placeholders` when its rhai expression is true.

## Hooks API (rhai) — complete

| Capability | Usage |
|------------|-------|
| Read a variable | bare name: `author`, `crate_name` |
| `variable::get(name)` | read (works for hyphenated names) |
| `variable::set(name, value)` | set/override (**flows into rendering in `pre`**) |
| `variable::is_set(name)` | presence check |
| `file::exists / read / write / delete / rename` | sandboxed to the working dir |
| `env::working_dir`, `env::destination` | directory constants |
| `to_kebab_case`, `to_snake_case`, `to_upper_camel_case`, `to_lower_camel_case`, `to_pascal_case`, `to_title_case`, `to_shouty_snake_case`, `to_shouty_kebab_case` | case conversion (heck) |
| `system::run(cmd)` | run `sh -c <cmd>` — **only with `--allow-commands`** |

Working dir = template dir for `init`/`pre`, **output dir** for `post`. `.rhai`
scripts resolve relative to the template root. Multi-statement scripts need `;`
between statements.

### `pre` hook deriving names (canonical pattern)

```rhai
variable::set("crate_name", to_snake_case(display_name));
variable::set("pkg_name", to_kebab_case(display_name));
variable::set("struct_name", to_upper_camel_case(display_name));
```

## Packaging (`package` subcommand)

Bundle a template directory into a distributable archive. **Raw** — no Liquid
rendering, `template.kdl` kept, the generation-time `include`/`exclude`/`ignore`
filters are **not** applied. Fixed behavior: `.git` always excluded; template-root
`.genignore` globs are honored (matched files dropped, matched directories not
descended into — keeps secrets out); `.genignore` itself stays in the archive.

```sh
openeis-generate package ./my-template                     # → my-template.tar.zst
openeis-generate package ./my-template -o dist.zip          # format from extension
openeis-generate package --format tar-gz ./tpl -o dist.tgz  # force a format
openeis-generate package ./tpl --level 19                   # zstd 1–22 / gzip 0–9
```

| Flag | Notes |
|------|-------|
| `[PATH]` | template dir (defaults to `.`) |
| `-o, --output <file>` | output path; extension selects format |
| `--format <fmt>` | force: `zip` / `tar-gz`(`tgz`) / `tar-zst`(`tzst`) |
| `--level <n>` | compression level; ignored for zip |
| `-f, --force` | overwrite an existing output file |

Round-trip check: feed the archive back through `--archive` to confirm the
distribution regenerates.

## Liquid quick reference

- Parser = **stdlib + 8 case filters**. `{% if %}`, `{% elsif %}`, `{% else %}`,
  `{% for %}`, `{% unless %}`, `{% case %}`, `contains`, `default`, etc. all work.
- Renders in **file names and contents**. Errors in a path component are treated
  as literal (a stray `{{` won't abort generation).
- `.liquid` suffix stripped from the output path.
- **Bool values are the strings `"true"`/`"false"`** — non-empty strings are
  truthy, so compare with `== "true"`.
- **Array values are comma-joined strings** — test with `contains`.

## End-to-end examples

```sh
# generate from an example, non-interactively
./target/release/openeis-generate \
    --path example-templates/rust-cli --name my-cli --silent \
    -D use_clap=true -D description="A CLI tool" --destination /tmp/out

# multi-select array, non-interactively
./target/release/openeis-generate \
    --path example-templates/feature-toggles --name feat-demo --silent \
    -D features=auth,graphql --destination /tmp/out

# package then round-trip
./target/release/openeis-generate package example-templates/package-demo -o /tmp/demo.tar.zst
./target/release/openeis-generate --archive /tmp/demo.tar.zst --name demo-proj --silent --destination /tmp/out
```

The bundled check `bash example-templates/verify-package.sh` exercises all three
package formats, asserts `.genignore` secrets are dropped, and round-trips
`.tar.zst` through `--archive`.
