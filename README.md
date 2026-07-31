# BLoader

BLoader 是一个开源的 **我的世界基岩版（Minecraft Bedrock）** Mod 加载器，以
`BLoader.dll` 的形式被宿主进程加载，负责原生预加载、BL Mod 装载、崩溃诊断、
国际化、文件重定向和可选的网络 Hook。

-宿主：`Minecraft.Windows.exe`（Windows 10/11，UWP / Steam / 桌面版均可）
- 编译目标：`cdylib`，默认产出 `BLoader.dll`
- 语言：Rust（Edition 2024）

> **维护提示**：`[package].version`（在 `Cargo.toml` 中）是版本号唯一来源。运行时常量
> `src/runtime/foundation/build_info.rs::VERSION` 透过 `env!("CARGO_PKG_VERSION")`
> 自动取值，不要在源码里硬编码版本字符串。

## 特性

- **原生预加载**：在 `DllMain` 同步阶段扫描 `mods/` 下 `preload-native` / `native`
  类型的包，校验导出表与依赖模块，并在加载后立即恢复顶层异常过滤器归属。
- **BL Mod**：通过 `bl-sdk` 暴露 ABI、事件总线、UI 注册、资源提供者等，使用
  `bl_export_mod!` 导出统一入口。
- **崩溃诊断**：在 DllMain 早期安装 VEH/SEH/顶层异常过滤器，把第三方预加载 DLL 的
  崩溃正确归属到 BLoader；进程崩溃单独落盘到 `logs/`。
- **i18n**：内置简体中文（`zh_CN`）与英文（`en_US`），文案以 `include_str!` 嵌入 DLL，
  不在运行时读取文件。
- **文件重定向**：按 `config.json` 把指定游戏目录重新映射到外部内容根。
- **网络 Hook（默认关闭）**：可选地拦截 WinSock2 API（`connect` / `bind` / `sendto` /
  `WSARecvFrom` 等）并解析 NetherNet 7551 / Xbox P2P 信令包；支持把 P2P 流量重定向
  到 EasyTier 之类 VPN 节点。该项**默认不启用**，也不会输出逐包详情，需要 `config.json`
  显式开启 `enable_network_hooks` / `network_verbose`。

## 默认发行（轻量化构建）

`default = []` 的默认构建刻意保持极小，**不会**把以下子系统链接进 DLL：

| 子系统 | Feature | 说明 |
| --- | --- | --- |
| ArcUI 面板渲染 / 输入捕获 / Bedrock UI | `panel-ui` | 源码完整保留，但默认不参与编译。需要时启用 `cargo build --features panel-ui`。 |
| Minecraft 外部符号包加载器 | `mc-symbols` | 下文详述。 |
| `blgen` 命令行工具 | `blgen` | 打包 `.blsym` 用；默认不构建 `[[bin]]`。 |

### Minecraft 符号加载：当前不支持

**BLoader 默认不加载任何 Minecraft 内部符号。** 与符号相关的 Rust 源码（
`src/core/symbols.rs`、`src/core/sig_scan.rs`、`src/core/native_hud_discovery.rs`、
`src/core/symbol_diagnostics.rs`、`src/core/symbols_tests.rs`、`src/mc/*`、
`templates/symbol-packs/*`、`tools/pack_symbol_pack.py` 等）**仍保留在仓库中**，但其模块
声明被 `#[cfg(feature = "mc-symbols")]` 包裹（见 `src/core/mod.rs`）。这意味着：

- 在默认 `default = []` 构建中，符号子系统**完全不进入产物**，没有解除 hook、没有
  解析游戏内部地址、没有把任何地址暴露给 MOD，也不会触发 `.blsym` 文件 IO。
- `bootstrap()` 里仅写一行提示
  `Minecraft symbol subsystem: disabled (not compiled in lightweight build).`，
  不在运行期寻找符号包。
- BL Mod 的 `requires_symbol_pack` / `required_symbols` 字段在轻量化构建里一律判定为
  不满足，相关 Mod 会被跳过。

如果你希望恢复符号加载能力，需要自行开启 feature：

```powershell
cargo build --release --features panel-ui   # 同时启用 mc-symbols
# 或
cargo build --release --features mc-symbols # 仅开启符号子系统，不开 ArcUI 面板
```

代价：DLL 体积明显增大，并对每个游戏版本维护一份 `.blsym` 包（见
`templates/symbol-packs/README.md`）。

## 目录结构

```
BLoader/
├─ src/
│  ├─ lib.rs            # DllMain + bootstrap 线程
│  ├─ core/
│  │  ├─ loader.rs      # 原生预加载、BL Mod 装载、热注入
│  │  ├─ network_hook.rs# 可选 WinSock2 Hook（默认关闭）
│  │  ├─ render/        # 极小的 Present 信号观察
│  │  └─ *.rs           # mc-symbols / panel-ui 的可选模块
│  ├─ bl/               # BL Mod ABI 与宿主回调
│  ├─ runtime/foundation/# 日志、i18n、崩溃、构建信息
│  ├─ fixes/            # 游戏 bug 修复（如 mcpe_228407）
│  ├─ config.rs         # config.json 读写 + 热更新
│  └─ utils.rs
├─ crates/              # ArcUI / 渲染器子 crate（默认不编译）
├─ bl-sdk/              # BL Mod Rust 辅助库
├─ templates/
│  ├─ bl_mod/           # 新建 BL Mod 的模板
│  └─ symbol-packs/     # .blsym 源包示例
├─ resources/lang/      # 内置 i18n 文案
├─ tools/
│  ├─ pack_symbol_pack.py     # 编译 .blsym
│  └─ bloader_crash_logger/
├─ examples/
│  ├─ bl_f3/            # ArcUI 面板示例（需 panel-ui）
│  └─ bl_motion_blur/   # BL Mod 示例
└─ build.rs             # 嵌入版本资源
```

## 构建

依赖：稳定版 Rust（支持 Edition 2024，建议 ≥ 1.85），MSVC 工具链。

```powershell
# 默认轻量构建
cargo build --release

# 如需面板与符号加载
cargo build --release --features panel-ui

# 打包 .blsym（可选）
cargo build --release --features blgen --bin blgen
python tools/pack_symbol_pack.py templates/symbol-packs/minecraft.windows.26.21.example.json <BLoader 目录>/minecraft.windows.26.21.blsym
```

产物位于 `target/release/BLoader.dll`。

## 配置

BLoader 在 `BLoader.dll` 所在目录查找 `config.json`；不存在时自动写入默认值。
常用字段（未列出者均有默认值）：

| 字段 | 默认 | 含义 |
| --- | --- | --- |
| `enable_debug_console` | `false` | 启用调试控制台与命令行输入 |
| `disable_mod_loading` | `false` | 跳过所有 Mod 加载 |
| `enable_redirection` | `false` | 启用文件重定向 |
| `redirection_root` | `"Minecraft Bedrock"` | 重定向根目录 |
| `file_redirections` | `[]` | `{source,target,kind}` 重定向条目 |
| `mods` | `[]` | 额外模组清单 |
| `default_locale` | `"zh_CN"` | 界面语言 |
| `log_level` | `"info"` | tracing 日志级别 |
| `enable_network_hooks` | `false` | 总开关，默认关闭 |
| `network_verbose` | `false` | 逐包详细日志，默认关闭 |
| `network_listen_port` | `19132` | 标注监听端口的日志标签 |
| `network_log_hex_bytes` | `0` | HEX dump 字节数 |
| `network_ignore_ports` | `[7897]` | 日志忽略端口列表 |
| `enable_p2p_redirection` | `false` | 把 P2P 流量重写到 `p2p_target_ip` |
| `p2p_target_ip` | `""` | P2P 重定向目标（EasyTier 等） |
| `enable_mc_bug_fix` | `true` | 内置游戏 bug 修复（见 `src/fixes/`） |

`config.json` 支持热更新；进程内静态值由 `Config::apply_update` 同步，网络 Hook 的
`update_config` 也在热更新路径中。

## 运行时日志

- `logs/latest.log`：当前会话滚动日志
- `logs/<时间戳>.log`：归档日志
- `logs/native-load-status.json`：原生预加载结果机读状态
- `logs/mods/<name>-<id>.log`：各 BL Mod 标准输出捕获
- `logs/bootstrap.marker.log`：DllMain 之前的极简启动标记

## 部署

1. 把 `BLoader.dll` 放到游戏进程所在目录（或装一个 `PreLoadCpp`-风格启动器把它注入）。
2. 在同目录创建 `mods/`，按 `templates/bl_mod/manifest.json.tpl` 的结构放预制包。
3. 若有原生预加载需求，按 BL SDK 文档准备 `manifest.json`；BLoader 会按 `native` /
   `preload-native` / `hot-native` / `hot-inject` / `BL` 分类装载。

## 许可证

本仓库目前未附带 LICENSE 文件，著作权归贡献者所有。如需二次分发或商用，请自行联系维护者
或在你的 fork 中补充许可证声明。