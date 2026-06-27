# Example templates

Four self-contained templates, each highlighting a different part of
`openeis-generate`. Run any of them with `--path`:

```sh
# from the repo root, build the binary first
cargo build --release

# then generate from an example into a fresh directory:
./target/release/openeis-generate \
    --path example-templates/minimal \
    --name my-project \
    --silent \
    --destination /tmp/out
```

Drop `--silent` to answer the prompts interactively, or pass
`-D key=value` (repeatable) / `--values-file values.kdl` to supply variables
non-interactively. `--dry-run` prints the plan without writing anything.

| Example | What it shows |
| --- | --- |
| [`minimal`](minimal) | string + bool placeholders, built-in variables, case-conversion filters in content & filenames, `.genignore` |
| [`rust-cli`](rust-cli) | a practical Cargo binary scaffold (`crate_name`, `authors`), Liquid `{% if %}` control flow, `vcs Git` |
| [`feature-toggles`](feature-toggles) | `array` placeholder (multi-select) + `conditional` blocks + Liquid `contains` |
| [`hooks-demo`](hooks-demo) | rhai `pre`/`post` hooks: `variable::set` feeding rendering, the `file`/`env` modules, case functions |
| [`package-demo`](package-demo) | a template meant to be **packaged**: nested dirs, a `.genignore` keeping `*.env`/`*.log` secrets out of the archive |

## Packaging

The `package` subcommand bundles a template into a distributable archive (default
`.tar.zst`); the [`package-demo`](package-demo) template exists to exercise it. Run the
end-to-end check (packages → all three formats, asserts the `.genignore` secrets are
dropped while `template.kdl` is kept, then round-trips the `.tar.zst` back through
`--archive` and confirms the project renders):

```sh
bash example-templates/verify-package.sh
```

Or by hand:

```sh
cargo build
./target/debug/openeis-generate package example-templates/package-demo -o /tmp/demo.tar.zst
./target/debug/openeis-generate --archive /tmp/demo.tar.zst --name demo-proj --silent --destination /tmp/out
```


## Gotchas worth knowing

- **The template config file is `template.kdl`** (in the template root). It is
  never copied into the output.
- **Booleans are `#true`/`#false`** in KDL, not bare `true`/`false`: kdl 6.7.1's
  v2 parser treats bare `true`/`false`/`null` as identifiers. Write
  `default #true` / `default #false` (works inline or on its own line).
- **Lists are multi-argument nodes:** `choices "a" "b" "c"`, not repeated
  `choices` nodes.
- **Hyphenated variables in rhai:** rhai identifiers can't contain `-`, so
  `os-arch` / `project-name` must be read with `variable::get("os-arch")` inside
  hooks, and can't appear in `conditional` expressions at all (use a
  non-hyphenated placeholder instead). Liquid has no such restriction —
  `{{ os-arch }}` works fine in templates.
- **Bool variables are strings:** a `bool` value is stored as `"true"`/`"false"`,
  and Liquid treats any non-empty string (including `"false"`) as truthy. So
  `{% if use_clap %}` is *always* true — compare explicitly:
  `{% if use_clap == "true" %}`. (See `rust-cli`.)
- **Hook scripts ship unless ignored:** `.rhai` files are regular template files
  and would be copied into the output. Exclude them with
  `template { ignore "*.rhai" }`. (See `hooks-demo`.)
- **Conditional keys with quotes:** a `conditional` key is a rhai expression,
  which usually holds its own quoted literals. Use a KDL **hash-string**
  `#"…"#` so the inner quotes need no escaping: `#"database != "sqlite""#`
  instead of `"database != \"sqlite\""`. (See `feature-toggles`.)

