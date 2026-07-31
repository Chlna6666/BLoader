# Changelog

本项目遵循语义化版本。每次 BLoader 行为、ABI、安全协议或发布格式发生变化时，必须更新版本号或本文件。

## [0.2.5] - 2026-07-31

### Changed

- 未检测到当前 Minecraft PID 对应的 BMCBL 安全一次性管道时，明确记录“不安装 `QueryApiImpl` Hook，继续使用微软官方 XUser 登录”。
- BMCBL 安全会话验证失败或 Hook 安装失败时，明确记录官方登录回退状态，避免用户误判为账号注入失败后仍在接管官方 Runtime。
- 安全会话接收并成功安装 Hook 后，仅记录经过控制字符过滤和 64 字符长度限制的公开 Xbox Gamertag。
- BLoader 版本更新为 `0.2.5`。

### Security

- 无有效 BMCBL 会话时仍保持零 Hook、零自定义 XUser 状态，不修改微软官方 `QueryApiImpl`。
- 日志继续禁止输出 XUID、Token、私钥、Authorization、Signature、完整请求正文和原始管道载荷。

## [0.2.4] - 2026-07-31

### Added

- 在 BLoader 内部实现按需 XUser 登录桥，不再依赖独立 `xgameruntime.dll` 代理。
- 仅在收到 BMCBL 为当前 Minecraft PID 创建的一次性认证管道后，Hook Microsoft 官方 `xgameruntime.dll!QueryApiImpl`。
- 内置 XUser 50 槽 ABI、Gamertag 接口、XUserAdd、用户句柄、XUID、年龄组和权限检查。
- 内置 XAsync Provider，异步任务继续调用 Microsoft 官方 `IXThreadingImpl`。
- 内置 URL 到 Xbox Live、PlayFab、Multiplayer、Realms 和 Licensing relying-party 的 Token 路由。
- 为 ANSI 和 UTF-16 `XUserGetTokenAndSignatureAsync` 实现完整结果缓冲区。
- 使用 BMCBL 提供的 P-256 BCRYPT 私钥 Blob，在进程内导入不可导出的 BCrypt 句柄。
- 按 Xbox Proof-of-Possession 规则生成 SHA-256 + ECDSA P-256 `Signature`，修复 Presence、好友状态和依赖签名的 Xbox Live 请求。
- 添加管道魔数、协议版本、目标 PID、父进程 PID、有效期、长度和 SHA-256 校验。
- 添加 Windows Release 构建、测试、导出检查、打包和标签发布工作流。
- 发布 ZIP 新增 `manifest.json`、`BLoader.dll.sha256`，并在 ZIP 外生成独立 `.zip.sha256` 文件。
- README 新增完整架构、安全边界、GDK 支持范围、Signature、CI 和发布产物说明。
- XUser Bridge 成功日志记录经过控制字符过滤和长度限制的公开 Gamertag，便于确认启动账号。

### Changed

- 第三方 native/preload Mod 不再在 `DllMain` loader lock 中加载。
- BLoader bootstrap 先处理一次性登录会话，再加载任何第三方 Mod。
- 除 `QueryApiImpl` 外，所有 XGameRuntime API 继续由系统官方 `xgameruntime.dll` 实现。
- 未检测到有效会话时，BLoader 不创建 MinHook、不修改官方函数，并保持原版 Xbox 登录行为。
- Windows CI 使用 `Swatinem/rust-cache` 缓存 Cargo registry、Git 依赖和可复用编译产物；缓存键包含 Rust 环境与 Cargo 清单变化。
- `dumpbin.exe` 不再硬编码 Visual Studio 2022 路径，优先使用 PATH，再通过 `vswhere.exe` 定位当前 Runner 的 MSVC 工具链。
- GitHub 标签发布同时上传 ZIP 与 SHA-256 文件，CHANGELOG 不再作为二进制附件上传。

### Fixed

- 修复 `windows-2025` Runner 使用新版 Visual Studio 时找不到 `dumpbin.exe` 导致的发布验证失败。
- 修复 PowerShell 兜底搜索 `dumpbin.exe` 时的空管道语法错误。
- 修复发布阶段只有 Rust 编译成功、但导出验证和 Artifact 打包失败的问题。
- 修复 PR Artifact 清单记录临时 merge commit 而非真实源码 head SHA 的问题。

### Security

- 登录 Token、设备私钥和启用开关不再通过 Minecraft 环境变量、命令行、注册表或临时 JSON 文件传递。
- 管道拒绝远程客户端，使用受限 DACL，并双向校验 BMCBL 父进程 PID 与 Minecraft 客户端 PID。
- 传输载荷、私钥 Blob、请求正文、Authorization 和 Signature 临时缓冲区在使用后清零。
- 日志只允许输出经过清理的公开 Gamertag；不记录 XUID、Token、私钥、Authorization、Signature、完整请求正文或原始管道载荷。
- 发布清单记录 DLL 大小、SHA-256、源码提交、工作流提交和 XUser Bridge 安全能力，便于 BMCBL 部署前校验。

## [0.2.3] - 2026-07-31

- 首个公开轻量化版本。
- 默认不编译 ArcUI 面板和 Minecraft 符号包子系统。
- 保留原生预加载、热加载、崩溃捕获、stdio 捕获和 Mod 诊断框架。
