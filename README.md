# openeis-generate

**[English](README.en.md)** | 中文

一个用 [KDL](https://kdl.dev)（而非 TOML）编写配置的项目/模板生成器。配置模型移植自
[`cargo-generate`](https://github.com/cargo-generate/cargo-generate)，并针对 KDL 做了适配与精简。

```sh
# 从本地模板生成（交互式）
openeis-generate --path ./my-template --name my-app

# 或从 git 仓库 / 归档（zip / tar.gz / tar.zst）/ URL 生成
openeis-generate --git https://example.com/t.git --name my-app
openeis-generate --archive https://example.com/t.zip --name my-app

# 把模板打包成单个归档文件用于分发
openeis-generate package ./my-template -o dist.tar.zst
```

## 现状

端到端可用。已实现：

- **KDL 配置**（`template.kdl`）—— 模板过滤、占位符、钩子、条件配置。
- **四种模板源** —— 本地 `--path`、`--git <url>`（clone）、`--archive <file|url>`
  （zip / tar.gz / tar.zst）、`--favorite`（来自 app 配置）。
- **打包分发** —— `package` 子命令把模板目录打成 `zip` / `tar.gz` / `tar.zst`
  归档（原样保留 `template.kdl`、跳过 `.git`、遵守 `.genignore`），为分发与 `publish` 做准备。
- **交互式变量** —— `bool` / `string` 占位符，支持默认值、choices、regex 校验；
  支持 `--define key=value` 与 `--silent`。
- **Liquid 渲染** —— 文件名与内容里的 `{{ var }}`；`.liquid` 后缀约定；
  include / exclude / ignore 过滤；二进制文件按字节拷贝。
- **Git** —— `--git` 时 clone；输出目录里 `git init` + 初始提交
  （均通过系统 `git`）。
- **Rhai 钩子** —— `init` / `pre` / `post` 三阶段，支持变量读写、沙箱化的
  `file` 模块、`env` 模块、大小写转换函数，以及可选的 `system` 模块。
- **条件配置** —— 当 rhai 表达式对已收集变量求值为真时，合并额外的占位符/过滤规则。

77 个单元测试，clippy 零警告。

## 构建

```sh
cargo build --release
# 二进制：target/release/openeis-generate
```

`openeis-generate` clone 与仓库初始化都 shell out 到系统 `git`，因此 `git` 需在
`PATH` 中。私有仓库认证走你本机的 git 凭证助手 / SSH 配置。

## 快速上手

模板就是一个包含 `template.kdl` 和待渲染文件的目录。

```
my-template/
├── template.kdl
├── README.md
├── Cargo.toml.liquid
└── src/
    └── main.rs
```

`template.kdl`：

```kdl
template {
    include "README.md" "Cargo.toml.liquid" "src/*"
    exclude "src/unused.rs"
    vcs "Git"
    init #false
}

placeholders {
    author {
        type "string"
        prompt "作者？"
        default "Alice"
    }
    license {
        type "string"
        prompt "许可证？"
        choices "MIT" "Apache-2.0"
    }
    use_ci {
        type "bool"
        prompt "配置 CI？"
        default #true
    }
}
```

生成：

```sh
openeis-generate --path ./my-template --name my-app
# → ./my-app/（已渲染），并带有新的 git 仓库与初始提交
```

## 模板源

以下互斥（只传一个）：

| 旗标 | 来源 |
|------|------|
| `--path <dir>` | 本地目录 |
| `--git <url>` | clone 一个 git 仓库（URL，或 `owner/repo`） |
| `--archive <file\|url>` | 解压本地 `.zip`/`.tar.gz`/`.tgz`/`.tar.zst`/`.tzst`，或经 HTTP(S) 下载后解压 |
| `--favorite <name>` | app 配置里定义的收藏 |
| _（位置参数）_ | 收藏名（未给 `--git`/`--path`/`--archive` 时） |

git ref 旗标：`--branch`、`--tag`、`--revision`（互斥）。
`--subfolder` 选择模板的子目录。

## 打包（`package` 子命令）

把模板目录打成一个可分发的归档（默认 `.tar.zst`）。打包是**原样**的——不渲染 Liquid，
保留 `template.kdl`；`include`/`exclude`/`ignore` 等 `template.kdl` 过滤规则是 **生成时**
才用的，打包时一律不应用。打包固定行为：

- 总是排除 `.git`；
- 读取模板根目录的 `.genignore`（每行一个 glob，`#` 注释/空行跳过），按其丢弃文件——
  连同匹配的目录一起跳过（不进入），便于发布前剔除密钥等本地文件；
- `.genignore` 文件本身会保留在归档里。

```sh
openeis-generate package ./my-template                  # → my-template.tar.zst
openeis-generate package ./my-template -o dist.zip       # 由扩展名推断格式
openeis-generate package --format tar-gz ./tpl -o dist.tgz
openeis-generate package ./tpl --level 19                # 压缩级别（zstd 1–22 / gzip 0–9）
```

| 旗标 | 说明 |
|------|------|
| _（位置参数）_ | 待打包的模板目录（默认当前目录） |
| `-o, --output <file>` | 输出归档路径；扩展名决定格式 |
| `--format <fmt>` | 强制格式：`zip` / `tar-gz`(`tgz`) / `tar-zst`(`tzst`) |
| `--level <n>` | 压缩级别；zip 忽略 |
| `-f, --force` | 覆盖已存在的输出文件 |

打出来的归档可直接用 `--archive` 喂回生成器（`openeis-generate --archive dist.tar.zst …`），
验证分发链路。

## 配置参考（`template.kdl`）

### `template`

```kdl
template {
    generator_version ">=0.1.0"   # 可选的 semver 版本要求
    include "a" "b"               # 白名单（多参数列表）
    exclude "target"              # 排除
    ignore "*.key"                # 额外忽略
    vcs "Git"                     # "Git" | "None"（默认 None）
    init #false                   # 布尔（写 #true/#false，非裸 true/false）
}
```

### `placeholders`

每个条目是 `string` 或 `bool`，带 `prompt`，可选 `default`、`choices`、`regex`：

```kdl
placeholders {
    author {
        type "string"
        prompt "作者？"
        default "Alice"
    }
    edition {
        type "string"
        prompt "Edition？"
        choices "2021" "2024"
        default "2024"
    }
    use_ci {
        type "bool"
        prompt "CI？"
        default #true
    }
    semver_tag {
        type "string"
        prompt "Tag？"
        regex "^v[0-9]+\\.[0-9]+\\.[0-9]+$"
    }
}
```

### `hooks`

在三阶段执行的 rhai 脚本（每条是相对模板目录的 `.rhai` 路径，或内联脚本）：

```kdl
hooks {
    init "setup.rhai"
    pre  "pre.rhai"
    post "post.rhai"
}
```

| 阶段 | 时机 | 工作目录 |
|------|------|----------|
| `init` | 收集变量之前 | 模板目录 |
| `pre`  | 变量收齐后、渲染前 | 模板目录 |
| `post` | 渲染后、git init 前 | 输出目录 |

`pre` 钩子里的 `variable::set` 会流进后续渲染。

### `conditional`

当 rhai 表达式为真时，合并该块的占位符与过滤规则：

```kdl
conditional {
    "lang == \"rust\"" {
        include "rust-only/*"
        placeholders {
            edition { type "string"; prompt "Edition？"; default "2024" }
        }
    }
}
```

随着新占位符出现，条件会重新求值，因此可以链式触发。

## 钩子 API（rhai）

| | |
|---|---|
| 变量 | 直接用名字读取（`author`、`project-name`…） |
| `variable::get(name)` | 读变量 |
| `variable::set(name, value)` | 设置/覆盖（在 `pre` 阶段会流入渲染） |
| `variable::is_set(name)` | 是否存在 |
| `file::exists / read / write / delete / rename` | 沙箱化到工作目录 |
| `env::working_dir`、`env::destination` | 目录常量 |
| `to_kebab_case`、`to_snake_case`、`to_upper_camel_case`、`to_lower_camel_case`、`to_pascal_case`、`to_title_case`、`to_shouty_snake_case`、`to_shouty_kebab_case` | 大小写转换（`heck`） |
| `system::run(cmd)` | 执行 `sh -c <cmd>` —— **仅** `--allow-commands` 时可用 |

> 多语句的 rhai 脚本，语句之间需要 `;`。

派生命名的 `pre` 钩子示例：

```rhai
variable::set("crate_name", to_snake_case(display_name));
variable::set("pkg_name", to_kebab_case(display_name));
variable::set("struct_name", to_upper_camel_case(display_name));
```

## 内置变量

`name` 与 `project-name` 由 `--name` 注入，可在模板、钩子、条件中使用。
（`authors`、`username`、`os-arch`、`project-name`、`crate_name`、`crate_type`、
`within_cargo_project`、`is_init` 为保留名，不能用作占位符。）

## CLI 参考

```
openeis-generate [OPTIONS] [AUTO_PATH]
openeis-generate package [OPTIONS] [PATH]      # 打包模板为归档（见上文）

Template Selection:
  --git <GIT>              --path <PATH>              --archive <file|url>
  --favorite <FAVORITE>    [AUTO_PATH]                --subfolder <SUBFOLDER>

Git Parameters:   --branch / --tag / --revision   （互斥）

Output Parameters:
  -n, --name <NAME>        -f, --force               --vcs <Git|None>
  -D, --define <KEY=VALUE> --init                    --destination <PATH>
  --overwrite              --force-git-init          -s, --allow-commands

Other:
  -c, --config <FILE>      --list-favorites          --dry-run
  -v, --verbose            -q, --quiet               --silent
```

- `--silent` —— 不交互；每个占位符必须能从 `--define` 或自身默认值解析。
- `--dry-run` —— 解析并打印计划，不写文件。
- `--config <FILE>` —— app 配置文件（默认 `~/.config/openeis/openeis.kdl`），
  收藏（favorites）定义于此。

## App 配置（收藏）

`~/.config/openeis/openeis.kdl`：

```kdl
favorites {
    my-tmpl {
        description "我的模板"
        git "https://example.com/t.git"
        branch "main"
        vcs "Git"
        init #true
    }
}
```

之后 `openeis-generate my-tmpl --name app`（或 `--favorite my-tmpl`）。
`--list-favorites` 打印已定义的收藏。

## KDL 写法注意

`kdl` 6.7.1 的 v2 解析器有几个坑 —— 用惯用写法都能避开：

- **布尔值写 `#true`/`#false`，不要写裸 `true`/`false`** —— v2 解析器把裸
  `true`/`false`/`null` 当作标识符而非值，因此 `init false`、`default true` 会
  解析失败；写成 `init #false`、`default #true`（单行或独占行均可）。项目故意
  不启用 `v1-fallback`（它虽能接受裸 bool，却会禁用下文的 hash-string）。
- **列表用多参数节点** —— `include "a" "b"`，而不是写两行 `include "a"`。
- **含引号的条件键用 KDL hash-string `#"…"#`** —— 条件键是 rhai 表达式，常含自身
  的引号字面量；用 `#"database != "sqlite""#` 而非转义形式 `"database != \"sqlite\""`，
  内层引号无需转义。

## 测试

```sh
cargo test          # 77 个测试
cargo clippy --all-targets
```
