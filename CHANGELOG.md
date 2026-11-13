## Unreleased

- 修复 BLoader 本地披风包作者信息丢失：`manifest.json metadata.authors`、`cape.json` 根级 `author` 与每披风 `author`、本地化 `.by` 署名正确映射至商品及目录 `creatorName`；无作者时才回退 `Player`。
- 合成 Persona manifest 写入 `metadata.authors`；内容寻址版本纳入作者，防止改作者后旧资源缓存残留。新增文件夹、ZIP、单披风及本地化作者验证，并更新文档。

## 0.2.110 (2026-11-07)

- 预加载阶段只做**描述符级** stdio 重定向（`_dup` / `_open_osfhandle` / `_dup2`），不再调用 `setvbuf` 与 `_flushall`，也不碰 Win32 标准句柄：原生加载器在加载期间打印的横幅因此能实时进入捕获管道，不再只能事后从日志文件补。0.2.104/105 的失败来自 CRT 全局 stdio 重配置，不是描述符替换本身。
- 若该重定向仍导致加载器 DllMain 失败，这条路径会整体回退。

## 0.2.109 (2026-11-07)

- 修复同一行仍重复显示：去重日志从“10 秒窗口”改为“本次会话内一直有效”（上限 16384 条，超出按先进先出淘汰）。实测三条拷贝（控制台 / stdout 管道 / 日志中继）在慢速预加载时能相隔数十秒到达，10 秒窗口拦不住后到的中继副本。
- 去重键继续做通道装饰归一化（行首同名 `[Name]` 标签、行尾填充空白），三条通道映射到同一条记录。

## 0.2.108 (2026-11-07)

- 修复同一日志块显示三遍：控制台捕获只接受以换行结尾的完整写入，分次 `WriteConsole` 产生的半截片段直接丢弃（完整行由 stdout 管道与日志中继提供），片段不再以原始文本漏到控制台。
- 去重键归一化：去掉行首与来源同名的 `[Name]` 标签和行尾空白。同一行经控制台/管道/文件三条通道到达时文本装饰不同，此前键不一致导致重复显示。
- 来源标签不再写死任何 Mod 名：`[Name]` 直接取该行自己打印的标签（跳过级别词与时间戳），新 Mod 无需改代码即可正确归属。

## 0.2.107 (2026-11-07)

- 控制台写入改为**逐次转发**，去掉跨调用的行缓冲：原生加载器分多次 `WriteConsole` 打印一行（彩色前缀 + 正文）时，缓冲会把整行压到换行符到达才输出，表现为整块迟到。现在与上游实现一致，每次写入立即记录，行顺序即发生顺序；重复由去重日志拦掉。
- 回退 0.2.104 的预加载阶段 stdio 捕获（0.2.105/106 已回退）：仅重定向 CRT 描述符同样会让 PreLoader 的 DllMain 初始化失败（0x8007045A）。

## 0.2.106 (2026-11-07)

- 回退“预加载阶段安装 stdio 捕获”：实测即使只重定向 CRT 描述符（不碰 Win32 标准句柄），PreLoader 的 DllMain 仍会初始化失败（`LoadLibraryW` 0x8007045A）。捕获恢复为 OEP 之后安装，原生加载器行为不受影响。
- 0.2.104 的其余修复保留：三通道统一去重（行尾空白不参与比较）、披风 `{"name","author"}` 描述符格式支持。

## 0.2.105 (2026-11-07)

- 修复 0.2.104 引入的加载失败（`LoadLibraryW` 0x8007045A，PreLoader/LeviLamina 的 DllMain 初始化失败）：提前安装的捕获在控制台尚未就绪时会替换进程 Win32 标准句柄，原生加载器因此初始化失败。现在预加载阶段只重定向 CRT 描述符，Win32 标准句柄保持宿主原样；控制台就绪后的安装路径行为不变。

## 0.2.104 (2026-11-07)

- 原生 Mod 控制台输出改为**真正的实时捕获**：CRT stdout/stderr 重定向提前到预加载链之前安装。此前重定向发生在 OEP 之后，PreLoader / LeviLamina 在加载期间打印的横幅只能事后从 `latest.log` 中继读到，表现为整块迟到、时间戳乱序。现在这些输出在写出时就进入 BLoader 的管道，按发生顺序显示。
- 所有捕获通道（控制台钩子、CRT 管道、日志中继）统一走同一个记录与去重路径，去重键忽略行尾填充差异：同一行只显示一次（此前横幅会显示两遍）。
- 披风文件夹支持 `{"name","author"}` 形式的 `cape.json`（`共享披风` / `我的披风1` 这类手写单披风目录）：包 UUID 由文件夹名派生，显示名取 `cape.json` 的 `name`。

## 0.2.103 (2026-11-07)

- 移除 0.2.102 的 `BedrockLogOut` 抢占：其入口与原始函数取自加载器内部的固定偏移，加载器一升级就会失效，不能作为长期方案。
- `[XUser] 正在回放 N 条早期 XUser Bridge 诊断` 降为 DEBUG：这是早期缓冲回放的过程说明，不是用户需要的信息。

## 0.2.102 (2026-11-07)

- 删除 LeviLamina 日志桥与其配置项，改为抢占 `BedrockLogOut`：从 LeviLamina 自己的钩子簿记（`_HookTarget` / `_OriginalFunc`）取得入口与原始函数，把入口改为纯转发到原始函数。游戏自身的日志路径恢复执行，现有 OutputDebugString 钩子重新以 `[Minecraft]` 捕获 Bedrock 日志。
- 接管前逐项校验：钩子计数非零、入口落在游戏模块范围内、原始函数指针可执行、入口当前确实是跳转指令；任一不符即跳过（其它加载器版本不会误 hook）。
- 代价：该加载器自己的 `[Server]` 日志副本不再产生（其自身日志不受影响）。

## 0.2.101 (2026-11-07)

- LeviLamina 日志桥改为可选：默认关闭，不再默认对特定 Mod 做版本相关处理；需要从该路径获取游戏日志时在 config.json 设置 `log_levilamina_bridge: true`（安装前仍校验函数序言，版本不符自动停用）。
- 复核结论：PreLoader 本身不触碰调试通道（`OutputDebugString` / `BedrockLogOut` / `ContentLog` 引用数均为 0），日志消失发生在 LeviLamina 的 `BedrockLogOutHook`——其 detour 只把行写进 `ll::serverLogger`，从不调用原始函数。因此“提前 hook”无法恢复这些行：原始函数不再被调用，任何挂在上面的钩子都不会被触发。

## 0.2.100 (2026-11-07)

- 新增 LeviLamina 日志桥：逆向其官方 PDB（26.60.3）确认 `ll::BedrockLogOutHook` 的 detour 只把游戏 Bedrock 日志写进 `ll::serverLogger`、**从不调用原始 `BedrockLogOut`**，因此 LeviLamina 加载后游戏调试通道不再有这些行。现在直接 hook `ll::io::Logger::printStr(LogLevel, std::string&&)`（RVA 0x8DCD0，安装前校验函数序言），在进程内、落盘之前捕获每一行：`ll::serverLogger` 的行按 `[Minecraft]` 输出，其它 logger 按其标题输出 `[Mod名]`。版本不匹配时自动停用，不会 hook 错地址。
- 该桥接为实时捕获，不依赖文件轮转与轮询，LeviLamina 与游戏日志不再有中继延迟；60s/300s 自检会输出 `seen`/`forwarded` 计数（debug 级）。

## 0.2.99 (2026-11-07)

- 控制台改为单一写线程：生产者只格式化并排队，控制台（ConPTY/终端）阻塞时不再拖住游戏线程、外部日志中继和 BLoader 自身日志；队列顺序即显示顺序，LeviLamina 日志不再延迟/乱序出现。
- 启动横幅按用途重排：钩子安装数量、水印目标、水印文本、WinHTTP 钩子安装、ODS 钩子安装改为 DEBUG；`完整调试` 标签改为 `日志路径`；移除早已废弃的“面板渲染服务已停用”一行。
- 修复单披风文件夹（仅 `cape.json` + 纹理、无 `manifest.json`）被拒绝的问题：文件夹名派生稳定 UUID，`共享披风` / `我的披风1` 这类包正常加载。

## 0.2.98 (2026-11-07)

- 修复披风模块 WinHTTP 异步预取导致 libHttpClient 崩溃（0xC0000005，栈 WINHTTP → BLoader bridge → libHttpClient）：异步响应体预取默认关闭，异步请求的响应体改为原生直通；本地披风合成响应与同步改写不受影响。
- OutputDebugString 钩子提前到原生预加载链之前安装（PreLoader / LeviLamina 加载前），启动阶段 Mod 与游戏的调试通道输出不再因安装顺序丢失。
- 日志归因：`Server` 来源的行不再捕获；`Runtime Service struct ...` 这类游戏运行时组件内部诊断降为 DEBUG，不再以 `[Minecraft]` INFO 出现。

## 0.2.97 (2026-11-07)

- 修复披风模块 WinHTTP 异步预取与游戏自身读取的并发竞争（21:01 libHttpClient 崩溃根因）：预取期间不再向游戏转发任何 WinHTTP 状态、ReceiveResponse 保持惰性，预取完成后由单点唤醒游戏并全程从重放数据读取；不再出现同一响应的两个读者。
- 修复 `wuapicore` 等被误标为 Mod：Windows Update 组件实际位于 `C:\Windows\UUS\`（System32 中不存在）；系统目录判定放宽为任何 `\Windows\` 下的路径。
- 修复 LeviLamina 轮转共享 `latest.log`（rename+重建）后日志中继偏移失效、控制台不再收到原生 Mod / 游戏行的问题：中继现在按文件创建时间+大小识别文件被替换，自动从头重读新文件。
- 新增中继诊断计数与 60s/300s 自检（`log_level=debug` 可见）：全零说明中继没有看到共享日志的新内容。

## 0.2.96 (2026-11-06)

- 修复原生 Mod 与游戏自身的控制台输出完全未被捕获：控制台写入钩子现在同时覆盖 `kernel32` 与 `kernelbase`。游戏经 `api-ms-win-core-*` API Set、LeviLamina 经 ucrt 最终都落在 kernelbase，此前只挂 kernel32，导致 `[LeviLamina]` 启动横幅与游戏 ContentLog 全部丢失。
- 捕获行按调用者模块归因：游戏本体、随游戏分发的 DLL 与 Microsoft 游戏运行时（`xgameruntime`、`libHttpClient`）归为 `[Minecraft]`；只有 `mods/` 下的 DLL 使用紫色 Mod 名；Windows 组件（`nlansp_c`、`wuapicore`、`Windows.Media.Speech` 等）归为 `[System]`，默认 info 级不显示。
- 修复 `[xgameruntime] Runtime Service struct ... Created` 被当成 Mod 的问题：`xgameruntime.dll` 属于游戏自身运行时。
- 捕获到的原生 Mod / 游戏行现在同时写入 `logs/latest.log` 与归档日志（保留原始时间戳），不再只打印到控制台。
- 不再在初始化时清空共享的 `logs/latest.log`：LeviLamina 在同一文件写入并负责轮转，清空会破坏它已写好的启动横幅并造成稀疏 NUL 字节。
- `log_level=debug/trace` 现在同时作用于控制台，此前只作用于文件 Sink。
- 控制台写入钩子按 hook 单独启用、失败可重试；新增双模块 hook 去重与嵌套调用保护，避免同一行被捕获两次。

## 0.2.95 (2026-11-05)

- 内置披风模块日志收敛（作者自用，去掉独立 DLL 时代的开源研究式遥测）：默认（info 级）只保留启动状态与扫描结果一行摘要（discovered / loaded / failed / items），失败包单独以 warn 输出原因；横幅、逐包注册明细、规则清单与全部请求遥测降为 debug 级（`log_level=debug/trace` 或 `cosmetics.json` 的 `debug.verbose` 才可见）。

## 0.2.93 (2026-11-03)

- 披风包扫描根纠正：移除独立 DLL 时代的 `mods/Cosmetics/capes` 遗留路径，披风只从游戏 com.mojang 数据目录的 `capes/` 加载（GDK/UWP/预览，重定向感知）；原测试包已迁移至 `%APPDATA%\Minecraft Bedrock\Users\Shared\games\com.mojang\capes`。


## 0.2.92 (2026-11-03)

- 内置披风模块（Cosmetics 移植，来源 Chlna6666/Cosmetics，GPL-3.0-only），默认开启：bootstrap 阶段（OEP 前）安装 WinHTTP 钩子，向更衣室披风页追加本地披风、为本地商品合成 PlayFab 目录响应并从内存下发虚拟 persona ZIP；启用后自动抑制独立 `Cosmetics.dll`（BLoader 发现路径），避免钩子叠加。
- 彻底移除 Bedrock-Cosmos 通路：内嵌元数据预设、内嵌默认 `cosmetics.json` 与 Cosmos CDN（`capes.bedrock-cosmos.app`）URL 白名单全部删除，目录元数据仅接受本地虚拟资源；披风分区更名 "Community Capes"；外部覆盖文件缺失时不再注入任何内置条目，默认规则仅含两条 `local_only` 核心规则。
- 披风包支持解包文件夹形式（单披风 `cape.json + cape.png`、多披风 `pack.json + textures/`，文件夹名参与稳定 UUID 派生），并可扫描游戏 com.mojang 数据目录的 `capes/`：GDK `%APPDATA%\Minecraft Bedrock( Preview)`、`C:\Users\Shared\games\com.mojang`、UWP 零售/预览/教育各包，开启重定向时跟随重定向位置；多根按优先级去重。
- 修复高像素披风玩家身上不显示（鞘翅正常）：生成 persona 纹理默认按 alpha 加权块平均规范到 64x32 官方规格（游戏角色披风几何按 64x32 采样，高分辨率会采到透明角落）；`cosmetics_cape_texture=original` 可切回原样直通。
- 新增配置：`enable_cosmetics`（默认 true）、`cosmetics_dir`（默认 `mods/Cosmetics`）、`cosmetics_cape_texture`（默认 `canonical`）；日志经 scope `cosmetics` 写入 latest.log。
- 新增依赖：`image`（仅 png）、`flate2`、`brotli`、`zip`。

## 0.2.80 (2026-10-23)

- 修复 BL Mod 后台线程日志的错误来源 `[BLoader/BL]`：优先以注册的 BL 模块名识别原生日志前缀，统一为 `[模块名]`。
- 移除日志正文中与模块来源重复的 `[模块名]`；保持 INFO/WARN/ERROR/DEBUG 级别、秒级控制台时间戳和文件日志原有信息。
- 未能准确识别来源时仅使用通用 `[BL]` 标记，不猜测其他已加载模块，避免错误归属。

## 0.2.79 (2026-10-23)

- 控制台恢复紧凑日志：保留秒级时间、级别和来源，不显示毫秒与事件序号。
- 内部日志继续使用毫秒级时间戳、早期事件排序信息及串行写入锁，原生 Mod 兼容捕获保持不变。

## 0.2.78 (2026-10-23)

- 修复原生 Mod 绕过 BLoader 日志捕获及原生 Win32 标准句柄变更导致日志消失的问题。
- 在优先预加载前启用结构化日志，并统一 Win32 Console 输出为毫秒时间戳与序号。
- 保留早期 BLoader/XUser 事件来源时间，原生文件及管道写入保持透传。

# Changelog

本项目遵循语义化版本。每次 BLoader 行为、ABI、安全协议或发布格式发生变化时，必须更新版本号或本文件。

## [0.2.9] - 2026-08-01

### Added

- 为 ANSI 与 UTF-16 `XUserGetTokenAndSignatureAsync` 增加完整请求头复制，不再只检查调用方指针后丢弃 Header 内容。
- 增加签名策略模型，按策略规定的 Header 名称顺序、忽略大小写匹配 Header 值；策略要求但请求未携带的 Header 使用空值占位，保持 Proof-of-Possession 输入布局稳定。
- 内置 Xbox 默认 NSAL 策略：普通 `*.xboxlive.com` 服务使用签名版本 1、`MaxBodyBytes=8192`、无 `ExtraHeaders`；`device.mgt.xboxlive.com` 与 `data-vef.xboxlive.com` 使用完整请求正文。
- 增加请求头顺序、大小写、缺失占位、未知服务兼容回退和 CR/LF Header 注入拒绝测试。

### Changed

- 签名正文现在按策略的 `MaxBodyBytes` 截断，不再无条件签入完整请求正文。
- 对尚未提供 Partner Center Title NSAL 元数据的非 Xbox 自定义服务，暂按调用方 Header 顺序兼容回退，并排除 `Authorization`、`Signature`、`Host` 与 `Content-Length` 等派生 Header。
- BLoader 版本更新为 `0.2.9`。

### Fixed

- 修复 Token Provider 接收 `header_count` 与 `headers` 后只验证、不保存，导致非空 `ExtraHeaders` 签名策略无法工作的实现缺口。
- 修复签名 Header 名称大小写不同或策略 Header 缺失时，无法按照 Xbox Signing Policy 生成确定性字段序列的问题。
- 明确 Presence 默认 Xbox 签名策略的 `ExtraHeaders` 为空：`x-xbl-contract-version`、`Content-Type` 和 `Accept-Language` 等 Header 仍随 HTTP 请求发送，但不会被错误地强行加入默认 PoP Signature。

### Security

- Header 名称和值均有长度上限；Header 名称必须符合 HTTP token 字符集，值拒绝 CR、LF 与 NUL，防止 Header 注入。
- 复制到异步上下文中的策略 Header 值和请求正文会在完成或释放时清零；日志仍不记录 Header 值、Token、Authorization、Signature 或请求正文。

## [0.2.8] - 2026-08-01

### Added

- 新增进程内 Xbox Rich Presence Bridge，在自定义 XUser 会话启用时动态扫描并 Hook 已加载 XSAPI 模块导出的 `XblPresenceSetPresenceAsync`。
- 直接复用 Minecraft 传入的 SCID、Presence ID、Presence Token IDs 和 active/inactive 状态，不依赖游戏内部类、偏移或版本特定地图状态 Hook。
- 使用 BMCBL 注入账户的 Xbox Live XSTS、UHS 和 P-256 Proof-of-Possession 私钥，通过 WinHTTP 向 `userpresence.xboxlive.com` 独立提交原始 Rich Presence。
- Presence Bridge 使用 Microsoft 官方 `IXThreadingImpl` 驱动调用方的 `XAsyncBlock`，避免官方 XSAPI 因自定义 XUser 上下文失败而阻断游戏状态更新。
- 增加 XSAPI 导出发现、Hook 地址、HTTP 状态码、Rich Presence 参数数量和失败原因诊断；日志不包含 Token、Authorization、Signature 或请求正文。
- 增加 Presence JSON 结构单元测试，验证输出与 XSAPI `TitleRequest` 的 `state/activity/richPresence/id/scid/params` 格式一致。

### Changed

- 自定义 XUser 首次被查询时启动 Presence Bridge 后台 Hook 与发送线程；无 BMCBL 安全会话时不会启动或修改 XSAPI。
- Presence 更新采用 latest-wins 队列，避免游戏短时间连续切换界面或世界时堆积过时请求。
- BLoader 版本更新为 `0.2.8`。

### Fixed

- 修复使用 `QueryApiImpl` 替换 XUser 后，好友状态只能显示“正在玩 Minecraft”，无法显示主菜单、世界或地图等 Rich Presence 详细状态的问题。
- 修复官方 XSAPI Presence 上下文与 BMCBL 自定义 XUser 不完全兼容时，`XblPresenceSetPresenceAsync` 异步失败并停止详细状态上报的问题。

### Security

- Presence Bridge 只在已验证的一次性 BMCBL XUser 会话存在时启用。
- Xbox Token、UHS、Authorization 和 Signature 只保存在进程内临时缓冲区，并使用 `Zeroizing` 在释放时清理。
- Bridge 仅记录公开模块名、函数地址、HTTP 状态和参数计数，不记录 XUID、Token、私钥、Authorization、Signature 或 Presence 请求正文。

## [0.2.7] - 2026-07-31

### Added

- 将 `DllMain` 阶段产生的 XUser Bridge 入口、管道验证、系统 Runtime 和 Hook 安装状态缓存为安全诊断记录。
- 在正常日志系统和调试控制台就绪后回放早期诊断，使 `latest.log` 和控制台都能直接看到桥接结果。
- 最早构建标记新增 `xuser_bridge=embedded protocol=1`，便于识别实际加载的 DLL 是否包含内置 XUser Bridge。

### Fixed

- 修复桥接入口已经执行但日志只存在于 `bootstrap.marker.log`，导致主日志中看不到 Xbox Gamertag、系统 `xgameruntime.dll` 路径和 `QueryApiImpl` Hook 状态的问题。
- 修复用户仅根据常规控制台日志无法区分“未执行桥接”“桥接失败”和“Hook 尚未首次命中”的诊断歧义。

### Security

- 回放队列只保存经过清理的 Gamertag、系统 DLL 路径、函数地址和状态文本；仍不保存或输出 Token、XUID、私钥、Authorization、Signature、请求正文或原始 IPC 载荷。

## [0.2.6] - 2026-07-31

### Added

- XUser Bridge 在 `DllMain` 最早阶段输出明确入口标记，包括协议版本、管道门控模式和仅接管 `QueryApiImpl` 的范围。
- 有效 BMCBL 会话存在时，记录系统原生 `xgameruntime.dll` 的实际来源、完整路径、`QueryApiImpl` 地址和 MinHook trampoline 地址。
- 首次命中 `QueryApiImpl` Hook 和首次请求 `CLSID_XUserImpl` 时输出一次性诊断日志，便于区分“Hook 已安装”和“游戏实际调用了 Hook”。
- 无安全管道时明确记录系统原生 `xgameruntime.dll` 当前是否已由宿主映射，以及游戏将继续使用微软官方 XUser 登录。

### Changed

- 仅在 BMCBL 安全会话已通过 PID、时效和摘要验证后，若官方 Runtime 尚未映射，才从 `System32` 同步加载微软原生 `xgameruntime.dll` 并安装 Hook。
- 无会话启动仍保持零主动 Runtime 加载、零 MinHook 和零自定义 XUser 状态。
- 拒绝对非 `System32` 路径的 `xgameruntime.dll` 安装 Hook，防止本地同名 DLL 劫持。
- BLoader 版本更新为 `0.2.6`。

### Fixed

- 修复只有“安全管道传输完成”但缺少 BLoader 端 Runtime/Hook 状态，导致无法判断桥接入口、原生 Runtime 和 Hook 是否实际工作的诊断缺口。

### Security

- 新增日志只输出公开 Gamertag、系统 DLL 路径和函数地址，不输出 XUID、Token、私钥、Authorization、Signature、请求正文或管道原始载荷。

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
- Windows CI 使用 `Swatinem/rust-cache` 缓存 Cargo registry、Git 依赖和可复用 `target` 缓存；缓存键包含 Rust 环境与 Cargo 清单变化。
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