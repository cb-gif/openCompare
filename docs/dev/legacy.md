# 旧原型开发说明

当前可执行代码为 Rust + egui 原型，用于复用和回归参照。产品目标采用 Tauri/React；旧原型能运行不代表新桌面已完成。历史结果见 [validation.md](validation.md)。

## 启动与比较

已安装 Rust 和平台编译依赖时，在仓库根目录执行：

```bash
cargo run -p compare-gui
cargo run -p compare-cli -- diff fixtures/left/example.rs fixtures/right/example.rs --json
cargo run -p compare-cli -- dir fixtures/left fixtures/right --differences-only
```

CLI 子命令返回 0 表示当前规则下相同，1 表示有差异，2 表示错误。diff 的 JSON 输出包含规则、编码、hunks 与 coarse_alignment；dir 提供逐项状态。详细参数以 `cargo run -p compare-cli -- --help` 及子命令帮助为准。

文本参数包括 --ignore-trailing-whitespace、--ignore-case、--ignore-line-endings、--left-encoding、--right-encoding；编码值为 utf8、utf16le、utf16be、gbk。规则影响比较，不自动转换原文件。

```bash
cargo build --release -p compare-cli
node scripts/benchmark.mjs release
```

基准脚本在 .tools/benchmark 生成 10 万行、25 处变化的固定样本，仅测 CLI，不覆盖桌面输入、目录扫描或峰值内存。

## 工具链与产物

scripts/bootstrap.mjs 是 Windows x64 专用可选安装辅助，会从 Rust 官方下载工具链到 .tools；使用前确认网络、MSVC 依赖和安装范围。它不代表当前机器已安装工具链，也不适用于 macOS。

若已经使用该脚本安装，本机 Git Bash 可显式选择工具链：

```bash
export CARGO_HOME="$PWD/.tools/cargo"
export RUSTUP_HOME="$PWD/.tools/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
cargo --version
```

历史 scripts/dev.ps1 仍保留，agent 按当前 Shell 规范直接使用 Git Bash。Debug 产物为 target/debug/rust-compare 与 target/debug/bcompare，Windows 另有 .exe 后缀。它们不是目标产品的已发布安装包。

## 功能边界

- 文本差异、行内高亮、块接收、撤销/重做、查找、编码和保存保护已有实现。
- 编辑使用独立弹窗，非目标产品的双栏直接编辑；自由编辑有 512 KiB 限制，文件读取上限为 16 MiB。
- 目录目前是完整结果返回后的列表，进度计数不等于渐进目录树。
- 复制包含预览、变化检查、取消和逐项报告，不提供整批回滚或完整特殊元数据保留。
- 会话配置默认在可执行文件旁 .rust-compare/sessions.json，也可通过 RUST_COMPARE_STATE_DIR 指定；正式安装版将迁移到平台应用数据目录。
- 未实现三方合并、目标多标签、Git 桌面集成、Tauri 前端及正式安装发布。
- Windows 有部分历史原生验收；macOS 与新桌面须重新验证。
