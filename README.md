# Rust Compare

使用 Rust 开发的本地文件与目录比较工具。当前原型支持文本比较与搜索、大文件只读分页、目录渐进扫描、会话恢复和目录文件复制，采用独立比较核心、文件系统层、会话层、CLI 和 egui 桌面界面。

代码仓库：[cb-gif/openCompare](https://github.com/cb-gif/openCompare)。

## 运行

安装 Rust stable 与 Windows 的 Visual Studio C++ Build Tools 后：

```powershell
cargo run -p compare-gui
```

也可以直接传入两份文件或两个目录：

```powershell
cargo run -p compare-gui -- fixtures/left/example.rs fixtures/right/example.rs
cargo run -p compare-gui -- fixtures/left fixtures/right
```

本项目已在 `.tools/` 安装隔离的 Rust 工具链，不修改系统 PATH。当前机器可使用：

```powershell
.\scripts\dev.ps1 run -p compare-gui
```

在其他 Windows 机器需要隔离安装时，可执行 `node scripts/bootstrap.mjs`。它从 Rust 官方站点下载 rustup，安装位置为 `.tools/cargo` 和 `.tools/rustup`；仍需已安装的 C++ 链接工具。

## 当前功能

- 双栏对齐、行级与行内高亮、差异块导航、同步纵向滚动。
- 忽略行尾空白、大小写及换行符；比较规则不修改原文。
- 将选中差异块接收到另一侧，撤销 / 重做，基础编辑及显式保存。
- `Ctrl+F` 查找两侧文本，高亮匹配；`F3` / `Shift+F3` 跳转下一处 / 上一处。支持区分大小写，最多显示 10,000 个匹配。
- UTF-8、UTF-16 LE / BE、GBK。BOM 自动识别；无 BOM 的 GBK / UTF-16 在打开窗口手动选择，解码不做静默字符替换。
- 保存前检查磁盘内容变化，同目录临时文件写入后替换原文件。只读文件与最终路径为符号链接的文件拒绝保存。
- 按相对路径递归比较目录，逐字节比较同大小文件，显示单侧文件、内容差异、类型冲突与读取错误。
- 默认不跟随目录内的符号链接；链接比较的是链接目标路径。
- 后台比较、扫描进度、取消与过期结果检查。
- 大文件自动进入只读分页，每页最多 1 MiB 原始字节或 2,000 行；支持上下翻页、当前页差异和查找，以及独立的完整文件字节相等判断。
- 目录扫描分批显示待比较和已确认的结果；扫描期间可筛选和滚动，取消后保留不完整结果并暂停复制，可直接重新扫描。
- 文件选择、路径输入、拖入两个文件或目录，未保存修改的关闭 / 会话切换提示。
- 最近会话自动记录，启动时重新打开上次的文件或目录；保留编码、比较规则、目录筛选与窗口大小。
- 目录文件可单选、`Ctrl+单击` 多选，或选择可见文件；支持复制预览、新建父目录、覆盖、取消、逐项结果及自动刷新。

原型的限制：普通文本会话上限为每侧 16 MiB、250,000 行；任一侧超过上限，两侧进入只读分页。普通会话自由编辑上限 512 KiB，范围内更大的文本仍可按块合并。长行通常显示前 4,000 个 UTF-8 字节，查找更后的匹配时切换到匹配附近。查找支持单行字面文本，不含正则、多行匹配或替换；不区分大小写时按 Unicode 小写比较。暂未实现三方合并、语法高亮和 Git 工具集成。目录按相对路径的原始大小写精确配对，暂未提供大小写不敏感的配对规则。

分页模式仅加载当前页，超长行会跨页并显示“续”标记；左右页分别按行数和字节上限切分，当前页差异不等于完整文件的差异清单。完整字节比较独立分块读取文件，打开或重新比较时执行；分页中暂停忽略规则、编辑、合并与保存。翻页检测大小和修改时间变化，发现变化时保留当前页并报错，可点击“重新比较”重载。此检查不保证发现修改时间和大小均被刻意保留的外部变化，也不保证未访问页的解码有效性。

Windows 上保存使用平台文件替换行为；需要保留 ACL、硬链接、备用数据流等特殊元数据的文件暂不建议通过本原型修改。跨进程同时写入存在常规检查与替换之间的竞争窗口，当前版本不提供文件锁或整批事务。

中文字体从系统读取，不分发字体文件。Windows 优先使用微软雅黑 / 黑体，macOS / Linux 提供常见 CJK 字体位置作为回退。

## 目录文件复制

打开两份目录并选中文件后，点击“复制到右侧…”或“复制到左侧…”，检查预览中的来源、目标、新建 / 覆盖清单，再执行复制。筛选变化时会清除选择，避免复制已隐藏的选中项。

预览使用 BLAKE3 内容指纹。执行前检查整批来源和目标，提交每个文件前再次检查；若预览后的内容变化，即使大小与修改时间相同，也会拒绝覆盖。每个文件先写同目录临时文件，再替换目标；新增目标使用不覆盖已有文件的提交方式。

取消保留已经复制成功的文件，结果窗口列出成功、失败、取消及未执行项。复制完成后自动刷新目录。如果覆盖了正在比较的文件，请从目录重新打开最新内容。存在未保存文本修改时，复制按钮暂不可用。

当前复制限普通文件，按需创建父目录；不复制目录项本身、符号链接、junction 或其他 reparse point。复制侧重文件内容，不保留原修改时间、ACL、备用数据流等特殊元数据。执行仍存在文件系统检查与提交之间的常规竞争窗口，不提供整批回滚。

## 会话配置

默认以便携模式保存在可执行文件旁的 `.rust-compare/sessions.json`；本机开发构建对应 `target/debug/.rust-compare/sessions.json`。也可以通过 `RUST_COMPARE_STATE_DIR` 指定配置目录。

配置只保存路径、编码和偏好，不保存未落盘的文档内容。最近记录保留 10 条，侧栏显示最近 5 条；点击“打开示例会话”会让下次启动回到示例，最近记录仍保留。相对目录输入会转成绝对路径保存，恢复不依赖之后的工作目录。

无法读取、损坏或版本不兼容的配置会显示错误并保留原文件，本次运行不会覆盖它。

## 命令行

```powershell
cargo run -p compare-cli -- diff fixtures/left/example.rs fixtures/right/example.rs
cargo run -p compare-cli -- diff fixtures/left/example.rs fixtures/right/example.rs --json
cargo run -p compare-cli -- diff LEFT RIGHT --ignore-case --ignore-trailing-whitespace --ignore-line-endings
cargo run -p compare-cli -- diff LEFT RIGHT --left-encoding gbk --right-encoding utf8
cargo run -p compare-cli -- dir fixtures/left fixtures/right --differences-only
```

返回码：`0` 表示按选定规则相同，`1` 表示有差异，`2` 表示读取、解码或比较错误。JSON 的行区间为从零开始、左闭右开的区间。默认文本输出为不带上下文的差异块；开启忽略规则后，输出仅描述被视为重要的变化，不是字节级完整补丁。

超过普通文本上限时，CLI 返回码及 JSON `equal` 依据完整文件字节比较，`mode` 为 `read_only_preview`、`comparison_scope` 为 `full_file_bytes`；`hunks` 仅覆盖首页，`hunk_scope` 为 `first_page`，两侧 `preview` 字段说明字节范围和是否还有内容。此模式不支持忽略规则，传入对应选项返回 `2`。目录 JSON 的 `complete` 说明整个扫描是否完成。

在本机使用隔离工具链时，将上面的 `cargo` 换成 `.\scripts\dev.ps1`。
PowerShell 脚本转发 `--` 时需加引号，例如：

```powershell
.\scripts\dev.ps1 run -p compare-cli '--' diff fixtures/left/example.rs fixtures/right/example.rs
.\scripts\dev.ps1 clippy --workspace --all-targets '--' -D warnings
```

## 开发检查

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p compare-gui -p compare-cli
node scripts/benchmark-preview.mjs
```

```text
crates/compare-core   原文快照、行索引、差异、合并
crates/compare-fs     编码、目录、受保护的保存
crates/compare-app    会话、撤销 / 重做、后台任务
crates/compare-gui    egui 桌面应用
crates/compare-cli    bcompare 命令行程序
fixtures/            可直接打开的示例
```

后续范围和验收标准见 [PLAN.md](PLAN.md)。

