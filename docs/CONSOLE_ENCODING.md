# BLoader 控制台日志编码兼容

BLoader 内部使用 Rust Unicode (UTF-8)。Windows 控制台输出采用 UTF-16 WriteConsoleW，
避免当前控制台代码页不为 UTF-8 时中文、日文等日志出现乱码。
Windows Terminal 非控制台管道保留 UTF-8，ANSI/VT 颜色序列按原样发送。

## 数据来源与解码

| 来源 | 处理方式 |
| --- | --- |
| BLoader 日志、早期横幅 | 如果 GetConsoleMode 成功，使用 WriteConsoleW；否则使用 WriteFile UTF-8 |
| Win32 WriteConsoleW | 已是 UTF-16，转换为 Rust Unicode，按完整逻辑行缓冲 |
| Win32 WriteConsoleA、控制台 WriteFile | 完整 UTF-8 优先；否则按 GetConsoleOutputCP，依次回退系统 ANSI/OEM |
| 第三方 CRT fwrite/_write、重定向字节流 | 完整 UTF-8 优先；否则按系统 ANSI；支持手动覆盖与回退 |
| BLoader 捕获的匿名 stdout/stderr 管道 | 按行缓冲，不切断 UTF-8/DBCS 字符；识别 UTF-8 BOM、UTF-16 LE/BE BOM 及具有明确 NUL 模式的 BOM-less UTF-16 |

对 legacy 字节流的检测有不可消除的歧义：一个没有 BOM 的字节序列无法可靠地自动区分
GBK、Shift-JIS、Windows-1252 等所有代码页。BLoader 使用来源对应的 Windows 代码页，
不进行可能破坏日志内容的语言统计推断。

对于 CRT/管道来源与系统 ANSI 代码页不同的 Mod，可在启动 Minecraft 之前设置环境变量：

```powershell
$env:BLOADER_LOG_CODE_PAGE = "936"   # GBK；日语 Shift-JIS 可使用 932
```

该环境变量仅影响 BLoader 的原生字节流捕获解码，不改变游戏的控制台代码页、
Windows Terminal 设置、文件编码或原生 Mod 的输出行为。
需要在进程启动前设置，不建议在 Mod 已加载后动态修改。

## 验证

1. 将控制台输出代码页设置为 936、932、437 或 65001，分别启动游戏，
   验证 BLoader 的中文消息、ANSI 彩色标签和横幅正常。
2. 让测试 Mod 从 WriteConsoleW、WriteConsoleA、WriteFile、fwrite 与 _write 输出
   UTF-8、GBK、Shift-JIS 样本，并将多字节字符拆成两次写入。
3. 检查匿名 stdout/stderr 管道对 UTF-16 BOM（含分片）与 UTF-8 BOM 的解码结果。
4. 验证 stdout/stderr 重定向至真实文件/管道时保持原始写入语义，
   不捕获其它磁盘文件，不重绑定第三方 CRT FILE*。
5. 运行 cargo test --locked，并在 Windows 上做 GDK/Windows Terminal 实机回归。

注意：已有乱码文本如果已在上游被错误解码并替换字符，输出端无法复原丢失的信息。
