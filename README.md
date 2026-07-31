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

### Minecraft 符号加载：无法实现（请勿尝试）

请明确：**BLoader 无法以通用方式加载 Minecraft 内部符号**，启用 `mc-symbols` feature
在主分支上不会得到任何支持，请不要把它当作可用的功能开关。这不是配置选项，而是一个**实现上不可能通用化**的子系统。

**为什么做不到：**

- Minecraft Bedrock 没有公开符号表或导出表，每个版本的可执行文件结构、内联位移、字节模式（pattern）都不同。要解析任何内部地址，都必须**为每一个具体游戏版本**单独逆向工程，并通过 `tools/pack_symbol_pack.py` 编译出一份精确匹配的 `.blsym` 包（详见 `src/core/symbols.rs` 的 SHA-256 + RVA/pattern 校验链路）。
- .blsym 包是基于版本特征的产物，**BLoader 项目本身不会也无法替你预先生成**这些包。在没有任何匹配包的运行时上，符号系统只会报告
  `no external symbol pack matches this game build`，不会暴露任何游戏内部地址给 MOD。
- 即便开启 `mc-symbols`（或 `panel-ui`，会连带启用 `mc-symbols`），代码也只是被编译进来；**真正能不能工作，完全取决于你是否手动维护了对应版本的 `.blsym`**，并自行承担相应的法律与维护责任。
- 对游戏可执行文件做特征码扫描、地址校验乃至远端 hook，在很多司法管辖区存在法律风险。BLoader 项目不会替任何人维护这类产物。

**因此主分支上的策略：**

- 默认的 `default = []` 构建把 `mc-symbols` 子系统整体排除。`bootstrap()` 里只写一行提示
  `Minecraft symbol subsystem: disabled (not compiled in lightweight build).`，不在运行期寻找符号包，也不读 `.blsym` 文件。
- BL Mod 的 `requires_symbol_pack` / `required_symbols` 字段一律判定为不满足，相关 Mod 会被跳过并写日志告知原因。
- 仓库中保留的 `src/core/symbols.rs`、`src/core/sig_scan.rs`、`src/core/native_hud_discovery.rs`、
  `src/core/symbol_diagnostics.rs`、`src/core/symbols_tests.rs`、`src/mc/*`、
  `templates/symbol-packs/*`、`tools/pack_symbol_pack.py` 等**仅作为代码保留**，让你理解历史设计或自行分叉扩展 —— 但绝不应该期待它们在主分支上能直接工作。

如果你只是想加载 MOD，不需要任何符号能力，使用默认 `default = []` 构建即可，所有 BL Mod 编程接口都会照常工作。

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

BLoader 以 **GNU 通用公共许可证第 3 版或更高版本（GPL-3.0-or-later）** 发布。完整文本见仓库根目录的
`LICENSE` 文件。

- 所有提交到本仓库的代码自动适用 GPL-3.0-or-later。在 `Cargo.toml` 中已声明 `license = "GPL-3.0-or-later"`。
- 因为 `bl-sdk`（编写 BL Mod 引用）以同样许可证发布，所有**静态或动态链接** `bl-sdk` 的 BL Mod
  产物**必须**遵循兼容的开源许可证（推荐同样使用 `GPL-3.0-or-later`，模板 `templates/bl_mod/Cargo.toml.tpl` 中已默认填入）。
- 仓库内 `vendor/` 下的第三方代码（如 `imgui-windows-d3d12-renderer`）保留其各自的原始许可证，BLoader 不对其主张版权。
- **无任何担保**：在适用法律允许的最大范围内，本程序按 "AS IS" 提供，著作权人不承担任何明示或默示的担保与责任。