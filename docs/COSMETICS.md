# 内置披风模块（Cosmetics）实现文档

状态：随 BLoader 0.2.95 起为内置组件（0.2.91 引入）。来源
[Chlna6666/Cosmetics](https://github.com/Chlna6666/Cosmetics)（同作者，GPL-3.0-only），
已按加载器形态重构：OEP 前装钩子、去除全部内嵌 Bedrock-Cosmos 数据、支持解包文件夹
与游戏数据目录扫描、日志收敛为作者自用形态。

## 1. 功能边界

- 向更衣室披风页（`DressingRoom_Capes`）追加本地披风分区，不删除、不改写任何官方行；
- 为本地商品合成 PlayFab `Catalog/GetPublishedItem` 响应（仅本地已注册 UUID）；
- 从内存下发虚拟 persona ZIP 与缩略图（`https://cosmetics.invalid`，不出网）；
- 纯 PNG 披风自动合成 persona 包；原生 `persona.zip` 原样直通。

明确不做：TLS 拦截、皮肤/装备引擎钩子、权益签名伪造、inventory/receipt 改写、
第三方目录服务（Bedrock-Cosmos CDN 白名单已删除）。

## 2. 模块结构

```text
src/cosmetics/
├── mod.rs            装配入口 install/update_config/should_suppress_native_mod、默认规则
├── winhttp.rs        11 个 WinHTTP 导出钩子、请求表、异步捕获、合成响应（minhook crate）
├── response.rs       规则解析与 JSON 覆盖引擎（5 种 mode）
├── cape_sections.rs  更衣室分区克隆注入（GridList 模板复用）
├── local_capes.rs    披风包扫描/校验/注册表/persona 纹理规范化/缩略图
├── persona_pack.rs   persona 包字节生成（ppack0、AES-256-CFB8、contents.json）
├── persona.rs        虚拟资源路由与本地 URL 校验
├── codec.rs          gzip/deflate/brotli 有界解码
├── cape_roots.rs     读取 BMCBL 实例 Shared/capes 的物理路径（不依赖重定向 Hook）
├── debug.rs          脱敏 JSON 形状摘要（永不输出值）
└── diagnostics.rs    端点归一化标签（allowlist，防泄露）
```

装配点：`lib.rs bootstrap()` 中 `file_redirection::install` → `network_hook::install` →
`cosmetics::install`，全部发生在 Minecraft `main`（OEP）之前，覆盖启动期 marketplace 请求。
前两者是游戏实例文件和普通网络功能；披风模块自身只安装 WinHTTP Hook，
不会添加、修改、借用文件重定向规则来拦截披风请求。
本地披风注册表在启用 WinHTTP Hook 前同步读取并缓存，HTTP 响应读取不会触发 PNG/ZIP 扫描。

## 3. WinHTTP 钩子层

Hook 对象：`winhttp.dll` 的 `Open / Connect / OpenRequest / SendRequest / WriteData /
ReceiveResponse / ReadData / QueryDataAvailable / QueryHeaders / CloseHandle /
SetStatusCallback`，经 `minhook` crate 逐导出 create 后一次性 enable。

拦截判定（`should_intercept`）：`config.json enable_cosmetics`（热重载原子位）∧
`cosmetics.json enabled` ∧（host 为 `cosmetics.invalid` ∨ 命中规则）。未命中请求的
回调与标志位保持游戏原样；状态回调桥只安装在命中的单个 request handle 上，
并始终先转发游戏原回调。

三种响应路径：

1. **虚拟资产**（`cosmetics.invalid/personabinary/*.zip`、`/thumbnails/*.png`）：
   `SendRequest` 阶段直接内存合成 200/404/405，HEAD 返回 GET 的 Content-Length，
   不发出任何 DNS/网络请求；
2. **目录快路径**（`CatalogItem` 规则 + 完整 POST body + 本地已注册 UUID）：
   `SendRequest` 阶段合成 200，跳过上游（`LOCAL_CATALOG_SYNTHETIC`）；
3. **响应覆盖**（同步整体缓冲 ≤ `max_capture_bytes`，或异步状态回调逐块捕获）：
   解码 → overlay → 以未压缩实体回放，并剥离 Content-Encoding。

合成响应的 `QueryHeaders` 完全离线应答（状态行/版本/CT/CL/Cache-Control/自定义名），
未知查询返回 `ERROR_WINHTTP_HEADER_NOT_FOUND (12150)`，绝不回退到真实句柄。

## 4. 规则引擎（cosmetics.json）

```jsonc
{
  "enabled": true,
  "max_capture_bytes": 4194304,          // 单响应捕获上限
  "debug": {"verbose": false, "response_shape": false},
  "rules": [
    {
      "name": "dressing_room_capes",
      "enabled": true,
      "host": "store.mktpl.minecraft-services.net",
      "path": "/api/v2.0/layout/pages/DressingRoom_Capes",
      "method": "GET,POST",              // 逗号分隔多谓词；store 域允许带 query
      "mode": "append_array",
      "identity_key": "id",
      "local_only": true                  // true=只用内存注册的本地披风，不读 overrides
      // "pointer"        追加目标的 JSON Pointer（非披风页时）
      // "file"           append/replace 的外部 JSON（相对 cosmetics_dir）
      // "catalog_directory" GetPublishedItem 的 <uuid>.json 目录
      // "official_preview_ids" 本地 UI 所有权投影 allowlist（显式 opt-in）
    },
    { "name": "catalog_custom_item_metadata", "mode": "catalog_item",
      "host": "20ca2.playfabapi.com", "path": "/Catalog/GetPublishedItem",
      "method": "POST", "local_only": true }
  ]
}
```

mode 一览：`append_array`（身份去重追加，披风页走分区注入并同步
`totalItems/maxOffers` 渲染上限）、`catalog_item`（按请求 ItemId 覆盖，校验
`data.Item.Id` 一致与全部 URL 为本地虚拟资源）、`replace_json`（整页替换，仅限
显式配置的页面，永不用于 inventory）、`persona_profile`（导航瓦片）、
`inventory_probe`（只读）。任何 overlay 错误一律透传原始响应。

文件路径守卫：仅相对路径、仅 Normal 组件，拒绝绝对/穿越路径。

## 5. 披风包格式

扫描根内每包 ≤64 MiB、≤256 成员、目录嵌套 ≤4 层；全局 ≤128 包、≤512 披风、
≤128 MiB 资产。PNG 要求 ≤2048×2048、完整可解码；纯 PNG 披风的 UV 需为 64×32
整数倍等比（如 128×64、256×128），否则拒绝。三种包形式：

### 5.1 多披风合集（推荐）

BLoader 参考基岩版资源包 / 皮肤包的清单设计：使用 `manifest.json` 管理包身份和
`metadata.authors`，以 `cape.json` 列出披风。但 `cape_pack`、`cape.json`
属于 **BLoader 自定义扩展**，不是原生 `skin_pack`、`skins.json` 格式。

```text
MyCapes/
├── manifest.json
├── cape.json
├── textures/
│   ├── red.png
│   └── blue.png
└── texts/
    ├── en_US.lang
    └── zh_CN.lang
```

`manifest.json`（整个包使用一个作者）：

```json
{
  "format_version": 1,
  "header": {
    "name": "My Capes",
    "uuid": "11111111-2222-4333-8444-555555555555",
    "version": [1, 0, 0]
  },
  "modules": [
    {
      "type": "cape_pack",
      "uuid": "66666666-7777-4888-8999-000000000001",
      "version": [1, 0, 0]
    }
  ],
  "metadata": {
    "authors": ["Pack Artist"]
  }
}
```

`cape.json`（允许每个披风覆盖合集作者）：

```json
{
  "localization_name": "my_capes",
  "capes": [
    {
      "localization_name": "red",
      "texture": "textures/red.png"
    },
    {
      "localization_name": "blue",
      "texture": "textures/blue.png",
      "author": "Another Artist"
    }
  ]
}
```

也可以直接在 `cape.json` 顶层写 `"author": "Shared Studio"`，
用作所有未写 `capes[].author` 的披风作者。
`metadata.authors: ["Alice", "Bob"]` 显示为 `Alice, Bob`，
因为商品 `creatorName` 是单个字符串。支持单作者简写
`"metadata": {"author": "Artist"}`。

`cape.json` 条目还可选 `icon`（自定义缩略图）、`persona_zip`
（原生资源直通）、`item_uuid`（固定商品 UUID）、`rarity`、
`start_date`。仅使用 Persona 资源时可省略 `texture`。
路径相对包根，合集每包 1..64 件披风，失败时整包拒绝。

### 5.2 作者解析顺序

1. `capes[].author`：每件披风自己的作者；
2. `cape.json` 顶层 `author`：合集默认作者；
3. `manifest.json` 的 `metadata.authors`（或单作者 `metadata.author`）；
4. `texts/<locale>.lang` 的 `capepack.<包键>.by`：参考原版皮肤包 `.by` 的本地化方案；
5. 作者全部缺失时才使用 `Player`。

示例 `texts/zh_CN.lang`：

```ini
capepack.my_capes=我的披风合集
capepack.my_capes.by=中文作者
cape.my_capes.red=红色披风
cape.my_capes.blue=蓝色披风
```

本地化文件优先当前 `default_locale`，缺失时回退 `en_US.lang`。
作者值会去除前后空格；空值视为缺省；无效类型、控制字符或超出 90 字符
则拒绝包，而不是静默显示 `Player`。所有作者会写入
本地 Offer `creatorName`、Catalog `DisplayProperties.creatorName`；
纯 PNG 生成的 Persona 包也写入 `manifest.json metadata.authors`。
作者变更不改变商品 UUID，但会刷新生成资源版本/URL，避免游戏缓存旧署名。

### 5.3 无 manifest 的单披风文件夹

仅一个披风时可使用文件夹 `cape.json + cape.png` 或
`cape.json + persona.zip`：

```json
{"name": "共享披风", "author": "Independent Artist"}
```

文件夹名称派生包 UUID，显示名称与作者直接读取 `cape.json`。
ZIP 建议使用 5.1 的完整清单格式。旧版开发期 `pack.json`
格式已停用，不会自动兼容。

### 5.4 身份派生（确定性，无运行时随机）

`uuid_v8 = SHA256("Cosmetics/local-capes/v1/" + kind + 0x00 + seed)[..16]`：

| kind | seed | 用途 |
| --- | --- | --- |
| `item-v2` | `pack.id\0cape.id` | 所有披风商品（或显式 `item_uuid`） |
| `pack` | 单披风文件夹名 | 无 manifest 的单披风包 UUID |
| `resource` | item_id | persona 模块 UUID |
| `pack` | item_id | 生成包 header UUID |
| `persona-content-v3` | `item_id\0version\0SHA256(persona)\0SHA256(thumbnail)` | 下载资源 UUID（内容寻址：改图自动换 URL/版本，身份不变） |

生成包版本 = `[author_major, u16(d[0..2]), u16(d[2..4])?]`（哈希自
`generated-persona-cache-version/v4` + 版本 + 名字 + 作者 + PNG），专用于击穿游戏的
"pack UUID + version" 缓存。

### 5.5 限额与失败语义

见本节开头常量；重复 item/资源 UUID、超限、损坏包 → 该包整包拒绝并输出
`WARN cape pack rejected: <名>: <原因>`，不影响其他包（无部分注册）。

## 6. 扫描根解析（cape_roots）

按序去重，每个根下只读检查 `capes/`，不存在便跳过，**不会创建游戏数据目录**：

1. `enable_redirection=true` 时，直接读取 BMCBL 实例对应的**物理数据目录**：
   `<redirection_root>\Users\Shared\games\com.mojang\capes`（GDK），
   `<redirection_root>\games\com.mojang\capes`（UWP）。不扫描账号目录，也不回退到其他系统安装实例；
2. 未启用实例隔离时，只读检查系统 GDK/UWP 的实际数据目录，包括
   `%APPDATA%\Minecraft Bedrock\Users\Shared\games\com.mojang\capes`、
   `%APPDATA%\Minecraft Bedrock Preview\Users\Shared\games\com.mojang\capes`
   和 UWP 包 `LocalState\games\com.mojang\capes`。

这些位置只用于定位本地资源，**不参与 HTTP 地址改写**。披风实际注入仍由
`WinHttpSendRequest / WinHttpReceiveResponse / WinHttpReadData / WinHttpQueryHeaders`
拦截并返回合成或修改后的响应完成；官方请求继续使用原本的 host 与 path。

独立 DLL 时代的 `mods/Cosmetics/capes` 已不参与扫描；`cosmetics_dir` 仅承载
`cosmetics.json` 与 `overrides/`。

## 7. Persona 包生成器（persona_pack）

合成结构（与 MerelyCapes 公开格式兼容；原生包永不解密改写，仅裸包补 ppack0 外壳）：

```text
personabinary/<resource>.zip
└── ppack0.zip
    ├── manifest.json     明文（header.uuid=pack，modules[0]=persona_piece）
    ├── <module>_cape.png        AES-256-CFB8（per-file key）
    ├── <module>_cape.meta.json  AES-256-CFB8
    │     {"piece_id":"<pack>","piece_name":"<module>_cape",
    │      "piece_type":"persona_capes","zone":["body_back_upper","body_back_lower"],
    │      "texture_sources":[{"texture":"<module>_cape.png"}]}
    ├── signatures.json   明文 [{"hash":b64(SHA256(manifest.json)),"path":"manifest.json"}]
    ├── texts/languages.json、texts/en_US.lang  明文
    └── contents.json     256 字节头 + AES 密文索引
          头：u32 0 | u32 0x9BCFB9FC | u64 0 | u8 len | pack-uuid | 补零
          密钥：固定内容密钥（MerelyCapes 公开值）；per-file key=32 字节 ASCII，
          派生自 SHA256("Cosmetics/persona-file-key/v1/"+pack_uuid+0x00+path)
```

AES-256-CFB8：IV=key[..16]，逐字节反馈，零填充至 16 倍数（与 .NET `Aes.Create()`
CFB8 兼容，单元测试含 OpenSSL 参考向量）。

**纹理规范化（`cosmetics_cape_texture=canonical`，默认）**：任意 64×32 整数倍
源图按 alpha 加权块平均降采样到 64×32（逐纹素独立，杜绝外层面/内衬/边缘 UV 渗色），
palette PNG 先转 RGBA8；64×32 RGBA8 输入保持字节不变。`original` 模式仅做 RGBA
归一化、保留原分辨率（实验直通，见 §10 限制）。缩略图取外背面 UV（x=1..11,
y=1..17 ×scale）最近邻缩放后合成 64×64 人体模型画布，或使用作者 `icon`。

## 8. 虚拟资源服务

注册表（`OnceLock`，启动扫描时建立）按路径提供内存资产：
`/personabinary/<resource_id>.zip`（application/zip）与
`/thumbnails/<resource_id>.png`（image/png）。请求路径先剥离 `?query/#frag`。
`overrides/persona/<uuid>.zip` 为旧工作流的只读补充源（须含 persona_piece 模块，
≤64 MiB/≤4096 成员）。目录 URL 校验：`data.Item.Contents[]` 全部
`Type=personabinary` 且 Url 仅允许 `https://cosmetics.invalid/...`（Cosmos CDN
白名单已删除）。

## 9. 更衣室注入（cape_sections）

在响应副本上操作（失败不动原文）：定位 `result.layout[0].rows` 中带
`headerComp+itemListComp` 的 GridList 模板行，收集既有 id（去重），可选执行
显式 allowlist 的所有权投影，然后按来源分桶注入三个新分区
`Official Capes - Catalog Only` / `Community Capes` / `My Local ZIP Capes`
（本地优先；空分区省略；分隔行克隆 VerticalLineDivider）。幂等：重复注入为零变更。
永不移除原行、永不克隆 Owned 区的交互按钮组件。

## 10. 限制与已知问题

- **披风 64×32 为引擎硬限制**：persona 合成器为每片分配固定图集槽位
  （`PersonaTextureComposer::_readTextureIntoAtlas(..., TextureUVCoordinateSet,
  maxDimensionsSlice)`；LeviLamina v1_26_40 头文件确认 meta schema 无任何
  尺寸/UV 字段；GeyserMC ThirdPartyCosmetics#5 独立确认 HD 披风被客户端裁剪或
  丢弃）。鞘翅正常是因为其走资源包几何声明尺寸归一化，与 persona 槽位无关。
  因此默认 `canonical` 降采样；`original` 仅供实验，玩家背面大概率不可见。
- `game_accepted=UNVERIFIED`：HTTP 层 `client_read_complete` 只证明游戏完整读取
  合成响应，不证明装备成功；装备判定只能目测。
- 匹配依赖精确 host+path（store 域允许 query）；游戏改路由需同步 `cosmetics.json`。
- 旧版本游戏（legacy UWP 只读模式）文件写入受限时扫描/日志按既有策略降级。

## 11. 日志

默认安静（info 级共约 3 行）：`WinHTTP hooks installed before OEP`、
`cape scan discovered=N loaded=M failed=K items=P`；失败包逐条
`WARN cape pack rejected: ...`；`enable_cosmetics=false` 时输出 SAFE_MODE 一行。
其余全部为 debug 级遥测（横幅、CAPE_ROOT、逐包注册、规则清单、request #N 系列）：
`log_level=debug/trace` 或 `cosmetics.json debug.verbose=true` 时可见。
`[DEBUG]` 形状摘要永不包含 token/receipt/XUID/原始 JSON。

## 12. 测试

`cargo test`（127+ 用例）覆盖：AES-CFB8 参考向量、生成包字节可复现、官方包
解密对照、UV 维度校验与降采样正确性（128×64..1280×640、调色板 PNG）、文件夹/ZIP
等价与同名去重、多根优先级、规则匹配与 overlay 幂等、目录覆盖校验、合成响应头
（HEAD/404/405）、路由脱敏。运行环境差异用例（com.mojang 候选）在本机可能为空集。

## 13. 部署（BMCBL 集成）

BMCBL 构建时把 `assets/bin/BLoader.dll` 压缩内嵌进启动器，启动时按 **PE 版本号**
与 `versions/<v>/BLoader.dll` 比较，不同则覆盖。因此：

1. 部署新 BLoader = 更新 `Desktop\BMCBL\assets\bin\BLoader.dll` + 重建 BMCBL；
2. **版本号必须递增**：同版本号的新构建不会覆盖旧 DLL（0.2.94 两次踩坑）；
3. 正式部署用 `cargo build --release`（约 2.3 MB）；
4. 独立 `Cosmetics.dll` 的 BMCBL 清单（`.manifest.json`）已改名
   `.manifest.json.disabled-by-builtin-cosmetics`；若目录出现 BLoader 格式
   `manifest.json`，内置启用时亦会被 `SKIP_COSMETICS_BUILTIN` 抑制。
