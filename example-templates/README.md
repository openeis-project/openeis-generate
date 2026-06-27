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

## Gotchas worth knowing

- **The template config file is `template.kdl`** (in the template root). It is
  never copied into the output.
- **Bare booleans go on their own line** in KDL: write
  `default true` on its own line, not `placeholder { default true }` inline.
  (kdl 6.7.1's v2 parser rejects a bool directly after `{`.)
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

