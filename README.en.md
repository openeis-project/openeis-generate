# openeis-generate

English | **[中文](README.md)**

A project/template generator whose configuration is written in [KDL](https://kdl.dev)
instead of TOML. The configuration model is a focused port of
[`cargo-generate`](https://github.com/cargo-generate/cargo-generate), adapted to
KDL and streamlined.

```sh
# generate from a local template, interactively
openeis-generate --path ./my-template --name my-app

# or from a git repo / archive (zip / tar.gz / tar.zst) / URL
openeis-generate --git https://example.com/t.git --name my-app
openeis-generate --archive https://example.com/t.zip --name my-app

# package a template into a single distributable archive
openeis-generate package ./my-template -o dist.tar.zst
```

## Status

Working end-to-end. Implemented:

- **KDL config** (`openeis.kdl`) — template filters, placeholders, hooks, conditionals.
- **Four template sources** — local `--path`, `--git <url>` (clone), `--archive <file|url>`
  (zip / tar.gz / tar.zst), and `--favorite` (from app config).
- **Packaging** — the `package` subcommand bundles a template directory into a
  `zip` / `tar.gz` / `tar.zst` archive (keeps `template.kdl` raw, skips `.git`, honors
  `.genignore`), for distribution and as a `publish` precursor.
- **Interactive variables** — `bool` / `string` placeholders with defaults, choices,
  regex validation; `--define key=value` and `--silent` supported.
- **Liquid rendering** — `{{ var }}` in file names and contents; `.liquid` suffix
  convention; include / exclude / ignore filters; binary files copied verbatim.
- **Git** — clone for `--git`, and `git init` + initial commit in the output
  (via the system `git`).
- **Rhai hooks** — `init` / `pre` / `post` phases, with variable read/write, a
  sandboxed `file` module, an `env` module, case-conversion helpers, and an
  opt-in `system` module.
- **Conditionals** — merge extra placeholders / filters when a rhai expression
  evaluates true against the collected variables.

52 unit tests, clippy-clean.

## Build

```sh
cargo build --release
# binary: target/release/openeis-generate
```

`openeis-generate` shells out to the system `git` for cloning and repo init, so
`git` must be on `PATH`. Private-repo auth relies on your normal git credential
helper / SSH config.

## Quick start

A template is just a directory with an `openeis.kdl` and the files to render.

```
my-template/
├── openeis.kdl
├── README.md
├── Cargo.toml.liquid
└── src/
    └── main.rs
```

`openeis.kdl`:

```kdl
template {
    include "README.md" "Cargo.toml.liquid" "src/*"
    exclude "src/unused.rs"
    vcs "Git"
    init false
}

placeholders {
    author {
        type "string"
        prompt "Author name?"
        default "Alice"
    }
    license {
        type "string"
        prompt "License?"
        choices "MIT" "Apache-2.0"
    }
    use_ci {
        type "bool"
        prompt "Set up CI?"
        default true
    }
}
```

Generate:

```sh
openeis-generate --path ./my-template --name my-app
# → ./my-app/  (rendered), with a fresh git repo + initial commit
```

## Template sources

All mutually exclusive (pass exactly one):

| Flag | Source |
|------|--------|
| `--path <dir>` | Local directory |
| `--git <url>` | Clone a git repo (URL, or `owner/repo`) |
| `--archive <file\|url>` | Extract a local `.zip`/`.tar.gz`/`.tgz`/`.tar.zst`/`.tzst`, or download one over HTTP(S) |
| `--favorite <name>` | A favorite defined in the app config |
| _(positional)_ | A favorite name (when no `--git`/`--path`/`--archive`) |

Git ref flags: `--branch`, `--tag`, `--revision` (mutually exclusive).
`--subfolder` selects a subdirectory of the template.

## Packaging (`package` subcommand)

Bundle a template directory into a distributable archive (`.tar.zst` by default).
Packaging is **raw** — no Liquid rendering, `template.kdl` is kept; the `include`/`exclude`/
`ignore` filters from `template.kdl` are *generation-time* concerns and are **not** applied.
Fixed behavior:

- `.git` is always excluded;
- a template-root `.genignore` (one glob per line, `#` comments / blank lines skipped) is read
  and matched files are dropped — including a matched directory (it isn't descended into), so
  secrets can be kept out of a published package;
- the `.genignore` file itself stays in the archive.

```sh
openeis-generate package ./my-template                  # → my-template.tar.zst
openeis-generate package ./my-template -o dist.zip       # format inferred from extension
openeis-generate package --format tar-gz ./tpl -o dist.tgz
openeis-generate package ./tpl --level 19                # compression level (zstd 1–22 / gzip 0–9)
```

| Flag | Description |
|------|-------------|
| _(positional)_ | Template directory to package (defaults to the current directory) |
| `-o, --output <file>` | Output archive path; its extension selects the format |
| `--format <fmt>` | Force a format: `zip` / `tar-gz`(`tgz`) / `tar-zst`(`tzst`) |
| `--level <n>` | Compression level; ignored for zip |
| `-f, --force` | Overwrite an existing output file |

The produced archive can be fed straight back through `--archive`
(`openeis-generate --archive dist.tar.zst …`) to verify the distribution round-trip.

## Configuration reference (`openeis.kdl`)

### `template`

```kdl
template {
    generator_version ">=0.1.0"   # optional semver requirement
    include "a" "b"               # whitelist (multi-argument list)
    exclude "target"              # remove
    ignore "*.key"                # additional ignores
    vcs "Git"                     # "Git" | "None" (default None)
    init false                    # bool
}
```

### `placeholders`

Each entry is `string` or `bool`, with `prompt`, optional `default`, `choices`,
and `regex`:

```kdl
placeholders {
    author {
        type "string"
        prompt "Author?"
        default "Alice"
    }
    edition {
        type "string"
        prompt "Edition?"
        choices "2021" "2024"
        default "2024"
    }
    use_ci {
        type "bool"
        prompt "CI?"
        default true
    }
    semver_tag {
        type "string"
        prompt "Tag?"
        regex "^v[0-9]+\\.[0-9]+\\.[0-9]+$"
    }
}
```

### `hooks`

Rhai scripts run at three phases (each entry is a `.rhai` path relative to the
template, or an inline script):

```kdl
hooks {
    init "setup.rhai"
    pre  "pre.rhai"
    post "post.rhai"
}
```

| Phase | When | Working dir |
|-------|------|-------------|
| `init` | before variable collection | template dir |
| `pre`  | after variables, before render | template dir |
| `post` | after render, before git init | output dir |

`pre`-hook `variable::set` calls flow into the rendering.

### `conditional`

Merge a block's placeholders and filters when the rhai expression is true:

```kdl
conditional {
    "lang == \"rust\"" {
        include "rust-only/*"
        placeholders {
            edition { type "string"; prompt "Edition?"; default "2024" }
        }
    }
}
```

Conditions are re-evaluated as new placeholders appear, so they can chain.

## Hooks API (rhai)

| | |
|---|---|
| Variables | read by name (`author`, `project-name`, …) |
| `variable::get(name)` | read a variable |
| `variable::set(name, value)` | set/override (flows into rendering in `pre`) |
| `variable::is_set(name)` | check presence |
| `file::exists / read / write / delete / rename` | sandboxed to the working dir |
| `env::working_dir`, `env::destination` | directory constants |
| `to_kebab_case`, `to_snake_case`, `to_upper_camel_case`, `to_lower_camel_case`, `to_pascal_case`, `to_title_case`, `to_shouty_snake_case`, `to_shouty_kebab_case` | case conversion (`heck`) |
| `system::run(cmd)` | run `sh -c <cmd>` — **only** with `--allow-commands` |

> Multi-statement rhai scripts need `;` between statements.

Example `pre` hook deriving names:

```rhai
variable::set("crate_name", to_snake_case(display_name));
variable::set("pkg_name", to_kebab_case(display_name));
variable::set("struct_name", to_upper_camel_case(display_name));
```

## Built-in variables

`name` and `project-name` are seeded from `--name` and available to templates,
hooks, and conditionals. (`author`, `os-arch`, `crate_name`, `crate_type`,
`within_cargo_project`, `is_init` are reserved and can't be placeholders.)

## CLI reference

```
openeis-generate [OPTIONS] [AUTO_PATH]
openeis-generate package [OPTIONS] [PATH]      # bundle a template into an archive (see above)

Template Selection:
  --git <GIT>              --path <PATH>              --archive <file|url>
  --favorite <FAVORITE>    [AUTO_PATH]                --subfolder <SUBFOLDER>

Git Parameters:   --branch / --tag / --revision   (mutually exclusive)

Output Parameters:
  -n, --name <NAME>        -f, --force               --vcs <Git|None>
  -D, --define <KEY=VALUE> --init                    --destination <PATH>
  --overwrite              --force-git-init          -s, --allow-commands

Other:
  -c, --config <FILE>      --list-favorites          --dry-run
  -v, --verbose            -q, --quiet               --silent
```

- `--silent` — don't prompt; every placeholder must resolve from `--define` or its
  default.
- `--dry-run` — resolve and print the plan without writing files.
- `--config <FILE>` — app config (default `~/.config/openeis/openeis.kdl`),
  where favorites live.

## App config (favorites)

`~/.config/openeis/openeis.kdl`:

```kdl
favorites {
    my-tmpl {
        description "my template"
        git "https://example.com/t.git"
        branch "main"
        vcs "Git"
        init true
    }
}
```

Then `openeis-generate my-tmpl --name app` (or `--favorite my-tmpl`).
`--list-favorites` prints what's defined.

## KDL authoring notes

The `kdl` 6.7.1 parser has a few quirks — all avoided by the idiomatic style:

- **Bare `true`/`false`/`null` arguments go on their own line** —
  `template { init false }` (single line) fails to parse; use
  ```
  template {
      init false
  }
  ```
- **Lists are multi-argument nodes** — `include "a" "b"`, not repeated
  `include "a"` lines.
- Strings with special characters (the conditional keys) are quoted:
  `"lang == \"rust\""`.

## Tests

```sh
cargo test          # 52 tests
cargo clippy --all-targets
```
