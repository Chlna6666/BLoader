# BLoader

[![Windows build and release](https://github.com/Chlna6666/BLoader/actions/workflows/windows-release.yml/badge.svg)](https://github.com/Chlna6666/BLoader/actions/workflows/windows-release.yml)

BLoader 是面向 Minecraft Bedrock 的开源 Rust Mod 加载器，以 `BLoader.dll` 进入游戏进程，负责原生 Mod/BL Mod 装载、日志捕获、崩溃诊断、文件重定向，以及与 BMCBL 配套的按需 GDK XUser 登录桥。

- 目标平台：Windows 10/11 x64
- 编译目标：`x86_64-pc-windows-msvc`
- crate 类型：`cdylib`
- 当前版本：以 `Cargo.toml` 的 `[package].version` 为唯一来源
- 许可证：GPL-3.0

## XUser 登录架构

BLoader 0.2.4 起不再依赖独立代理 `xgameruntime.dll` 或 `xgameruntime_o.dll`。

```text
BMCBL
  └─ 仅在存在有效 Win32 GDK 会话时创建一次性安全通道
       └─ BLoader.dll
            ├─ 校验目标 PID、父进程 PID、时效和载荷摘要
            ├─ 内置 Rust XUser / XAsync Provider
            ├─ 只 Hook Microsoft xgameruntime.dll!QueryApiImpl
            ├─ 为 XUserGetTokenAndSignatureAsync 生成 Token + Xbox PoP Signature
            └─ 其他 XGameRuntime 接口继续由系统官方 xgameruntime.dll 实现
```

### 无登录会话时

```text
BMCBL 不创建安全会话
→ BLoader 检测不到当前 Minecraft PID 的会话管道
→ 不加载自定义 XUser Provider
→ 不创建 MinHook
→ 不修改 QueryApiImpl
→ Microsoft 原版 Xbox 登录链路保持不变
```

因此普通启动不会承担额外 Hook 成本，也不会因为 BMCBL 登录状态缺失或失效而破坏游戏原生登录。

### 支持范围

XUser Bridge 当前只支持：

- `Minecraft.Windows.exe` Win32 GDK 版本；
- 由 BMCBL 使用 `CreateProcessW(CREATE_SUSPENDED)` 启动；
- BMCBL 已取得有效的 Xbox/GDK 预认证会话。

不支持：

- UWP/AppContainer 版账号注入；
- 通过环境变量、命令行、注册表或普通临时 JSON 传递登录凭证；
- 把 BLoader 当作完整 `xgameruntime.dll` 替代品。

## Token 与 Signature

`XUserGetTokenAndSignatureAsync` 会根据请求 URL 选择对应 relying party：

| 服务 | 默认 relying party |
| --- | --- |
| Xbox Live / Presence | `http://xboxlive.com` |
| Minecraft Multiplayer | `https://multiplayer.minecraft.net/` |
| Realms | `https://pocket.realms.minecraft.net/` |
| PlayFab | `https://b980a380.minecraft.playfabapi.com/` |
| Xbox Licensing | `http://licensing.xboxlive.com` |

BLoader 构造：

```text
Authorization: XBL3.0 x=<uhs>;<token>
Signature: Base64(version + FILETIME timestamp + ECDSA-P256 signature)
```

签名摘要覆盖 HTTP Method、Path/Query、Authorization、签名策略要求的 Header 和请求 Body，使用 SHA-256 与 ECDSA P-256 生成 Xbox Proof-of-Possession Signature。

## XUser 通信日志

BLoader 会把 XUser/BMCBL 通信状态输出到结构化控制台和正常日志链路，包括：

- BMCBL 当前 Minecraft PID 的 named-pipe 探测和连接状态；
- Minecraft PID、父进程 PID、pipe server PID 校验；
- transport protocol、时效窗口、payload 长度、SHA-256 校验结果；
- 会话解析状态、公开 Gamertag、XUID、Token 路由数量、Privilege 数量；
- System32 `xgameruntime.dll` 路径和 `QueryApiImpl` Hook 状态；
- `QueryApiImpl` 路由到内置 XUser 或微软官方 Runtime；
- 每次 Token/Signature 请求的 encoding、HTTP method、host、无 query 的 path、relying party、Header 名称、Body 大小、Token 剩余寿命；
- PoP Signature 生成阶段、XAsync Begin/DoWork/GetResult/Cleanup、HRESULT 和结果 buffer 大小。

日志不会记录 Token 内容、UHS、Authorization 正文、Signature 正文、ECC 私钥、完整请求 Body 或原始 pipe payload。URL query 也不会写入通信日志。

## 安全模型

登录材料不通过以下渠道传递：

- Minecraft 环境变量；
- 命令行参数；
- 注册表；
- 普通临时凭证文件；
- BLoader Mod API。

安全会话使用当前 Minecraft PID 派生的一次性本地命名管道，并执行：

- `PIPE_REJECT_REMOTE_CLIENTS`；
- 受限 DACL；
- BLoader 校验管道服务端 PID 必须是 Minecraft 父进程；
- BMCBL 校验客户端 PID 必须是目标 Minecraft 进程；
- 协议魔数、版本、目标 PID、启动器 PID、签发时间、失效时间、载荷长度和 SHA-256 校验；
- 会话只消费一次，随后关闭并销毁管道；
- 私钥 Blob 导入 BCrypt 后立即清零；
- Token、Authorization、Signature、请求 Body 和传输缓冲区按生命周期清零。

第三方 Mod 在安全会话消费并完成 Hook 初始化后才开始加载，不能通过正常 BLoader API 获得登录载荷。

同进程边界仍需明确：具有任意内存扫描或 Hook 能力的恶意原生 Mod、管理员、SYSTEM 或内核程序，理论上仍能读取游戏进程中的短期数据。BLoader 的目标是消除可避免的环境变量、命令行、文件、注册表和 IPC 泄漏面，而不是在同一地址空间内提供硬件级隔离。

## 主要功能

- 原生 `native` / `preload-native` / `hot-native` / `hot-inject` Mod 装载；
- BL Mod ABI、事件总线、资源和 UI 注册；
- 进程 stdio 捕获，支持 `puts`、`printf`、`fprintf`、`std::cout`、Rust stdout/stderr 与 Win32 stdout；
- 每个 Mod 的加载生命周期、stdout/stderr、BL Host `log()` 和崩溃归属统一输出到控制台；
- VEH/SEH/顶层异常过滤器和原生预加载崩溃归属；
- 内置中英文 i18n；
- 文件重定向；
- 默认关闭的 WinSock2/NetherNet 网络 Hook；
- 可选 ArcUI 和 Minecraft 外部符号子系统。

默认构建为轻量模式：

```toml
[features]
default = []
```

`panel-ui`、`mc-symbols` 和 `blgen` 不会进入默认发布 DLL。

## 结构化运行控制台

BLoader 控制台使用固定列输出：

```text
TIME         LEVEL  GROUP    SOURCE             THREAD     MESSAGE
12:44:10.125 INFO   SYS      identity           t18452     BLoader v0.2.32 ...
12:44:10.180 INFO   XUSER    bridge             t18452     BMCBL XUser pipe 已连接 ...
12:44:11.004 INFO   MOD      ExampleMod          t18452     stdout> initialized
12:44:11.040 DEBUG  MOD      ExampleMod          t18452     lifecycle#8 ...
12:44:12.300 INFO   STDIO    game-stdio          t19200     stdout> ...
```

分组包括：

- `SYS`：构建、宿主、日志、路径和兼容层；
- `XUSER`：BMCBL/XUser/Token/Signature 通信；
- `MOD`：Mod 加载、生命周期、BL Host 日志和已归属 stdout/stderr；
- `STDIO`：未能归属到特定 Mod 的 Minecraft/进程输出；
- `NET`：网络 Hook；
- `BLOADER`：其他 BLoader 内部模块。

控制台支持 ANSI 时会按等级和分组着色，不支持 ANSI 时保持相同列布局的纯文本输出。QuickEdit 被关闭，避免选择文本时暂停 Minecraft 进程。

### UWP 1.17–1.19

Minecraft 1.17.x、1.18.x、1.19.x 下 BLoader 进入 `legacy-uwp-no-file-write`：

- 不创建或修改 BLoader 日志、配置、崩溃文件和缓存；
- 不安装会预创建目标目录的文件重定向；
- Mod/进程 stdout 与 stderr 改用匿名内存管道实时捕获；
- 预加载阶段的 Mod 输出在日志系统就绪前暂存于内存，随后回放到控制台；
- 不依赖磁盘即可看到 `puts/printf/std::cout/Rust print/Win32 stdout`。

其他版本继续保留磁盘 raw stdio capture，并同步实时回放到结构化控制台。

## 构建

依赖：

- Stable Rust，支持 Edition 2024；
- Visual Studio/MSVC x64 工具链；
- Windows SDK。

```powershell
cargo fmt --all
cargo test --lib --target x86_64-pc-windows-msvc
cargo build --release --lib --target x86_64-pc-windows-msvc
```

Release DLL：

```text
target\x86_64-pc-windows-msvc\release\BLoader.dll
```

本地打包：

```powershell
New-Item -ItemType Directory target\release -Force | Out-Null
Copy-Item target\x86_64-pc-windows-msvc\release\BLoader.dll target\release\BLoader.dll -Force
.\scripts\package-release.ps1
```

## GitHub Actions 与发布产物

`.github/workflows/windows-release.yml` 在 Windows Runner 上执行格式检查、单元测试、Release DLL 构建、导出验证、打包、SHA-256 和 Artifact/Release 发布。

发布 ZIP 内包含：

```text
BLoader.dll
BLoader.dll.sha256
README.md
CHANGELOG.md
LICENSE
manifest.json
```

`manifest.json` 包含版本、目标平台、构建配置、提交 SHA、DLL SHA-256 和 XUser Bridge 安全能力声明。BMCBL 下载或部署产物时应校验 ZIP 或 DLL SHA-256。

## 运行时日志

允许文件写入的版本主要使用：

```text
logs/latest.log
logs/archive/bloader-<timestamp>.log
logs/native-load-status.json
logs/mods/<name>-<id>.log
logs/mod-registry.json
```

`latest.log` 和 archive tracing sink 包含 target、线程 ID、线程名、源文件和行号。完整 DEBUG 为默认有效等级；只有显式 `log_level=trace` 才启用 TRACE。

1.17–1.19 的 legacy UWP 模式不创建上述文件，诊断改走控制台、`OutputDebugStringW` 和匿名内存管道。

### XUser Bridge 判定顺序

有效 BMCBL 会话下，日志应依次出现类似：

```text
XUser Bridge 入口已执行
BMCBL XUser pipe 已连接
BMCBL XUser transport verified
已从 BMCBL 安全一次性管道接收并验证 Xbox 会话
系统原生 xgameruntime.dll 已就绪
已定位系统原生 QueryApiImpl
XUser Bridge 已启用；仅接管官方 QueryApiImpl
QueryApiImpl Hook 已首次命中
QueryApiImpl 已请求 CLSID_XUserImpl；返回 BLoader 内置 Rust XUser
XUser token/signature request | ...
XUser request signature prepared | ...
XUser token async provider | op=GetResult | result=S_OK | ...
```

若没有安全会话，日志会明确说明不主动加载系统 Runtime、不安装 Hook，并继续使用微软官方 XUser。

## 配置

BLoader 在 DLL 所在目录读取 `config.json`。常用配置：

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `enable_debug_console` | `false` | 创建调试控制台 |
| `disable_mod_loading` | `false` | 禁止第三方 Mod 装载 |
| `enable_redirection` | `false` | 启用文件重定向 |
| `default_locale` | `zh_CN` | 日志/界面语言 |
| `log_level` | `debug` | 日志级别；`trace` 可启用最高详细度 |
| `enable_network_hooks` | `false` | 网络 Hook 总开关 |
| `network_verbose` | `false` | 逐包详细日志 |
| `enable_p2p_redirection` | `false` | P2P 重定向开关 |
| `enable_mc_bug_fix` | `true` | 内置游戏问题修复 |

XUser Bridge 不使用 `config.json` 开关。它只由 BMCBL 为当前 Minecraft PID 创建并认证的有效一次性会话激活。

## 部署

BMCBL 集成模式下，仅部署：

```text
BLoader.dll
```

不再部署：

```text
xgameruntime.dll
xgameruntime_o.dll
xgameruntime Mod manifest
登录环境变量配置
```

BLoader 只 Hook 系统官方 `xgameruntime.dll!QueryApiImpl`；其他导出与 Runtime Class 始终由微软官方实现。

## 许可证

BLoader 以 GPL-3.0 发布；Cargo 使用与其等价的精确 SPDX 标识 `GPL-3.0-only`。完整条款见 `LICENSE`。仓库中的第三方代码保留各自许可证。本软件按“AS IS”提供，不附带任何明示或默示担保。
