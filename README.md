# 端口管理工具（HydraPort）

Windows 10/11 的轻量端口管理桌面程序，使用 Windows IP Helper API 枚举 TCP/UDP 端口，不依赖解析 `netstat` 输出。

界面层是纯原生 Win32 控件（ListView / StatusBar / 自建窗口类的模态对话框），不使用任何 GUI 框架，
因此进程里不会加载 OpenGL 驱动——这是「空闲内存」指标能达标的根本原因，详见「实测指标」。

## 环境

- Rust stable（MSVC x64 工具链）
- Visual Studio Build Tools（提供 MSVC 链接器）
- 可选：Windows SDK 的 `rc.exe`，用于把图标与清单编译进 exe（缺失时会退化为无图标构建，不影响功能）

## 运行与构建

```powershell
cargo run
cargo test
cargo build --release
```

生成文件位于 `target\release\port-manager.exe`。如需结束系统或其他用户的进程，请右键以管理员身份运行。

设置环境变量 `HYDRAPORT_LOG=1` 可启用文件日志，位置为 `%LOCALAPPDATA%\HydraPort\hydraport.log`；
设为 `stderr` 则输出到标准错误。默认不写任何日志。

## 功能

- TCP/UDP 监听端口列表：端口、协议、本地地址、状态、PID、进程名、进程路径
- 搜索过滤（端口号、进程名、PID 模糊匹配，不区分大小写，输入即筛选）
- 精确端口查询：输入端口号后点击「查询」，直接选中占用该端口的全部记录
- 表格列排序（点击表头切换升序/降序，表头带排序箭头）
- 多选：单击选中，`Ctrl + 单击` 追加/取消，`Shift + 单击` 区间选择
- 双击行或右键「查看详情」：展示端口信息与进程详情（父进程、内存占用、启动时间、命令行）
- 右键菜单：复制端口、复制 PID、查看详情、打开文件位置、结束进程
- 结束进程：支持进程树（同时结束子进程），结束前弹窗确认，同一 PID 只结束一次并跳过本程序自身
- CSV 导出（导出当前过滤结果）
- 主题：默认跟随系统深色/浅色，可用工具栏按钮手动切换
- 系统托盘：「设置」菜单可开关「关闭窗口时最小化到托盘」，托盘菜单支持显示主窗口、刷新、退出；
  资源管理器重启后会自动重新注册图标
- 开机自启：「设置」菜单可开关，写入 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
- 状态栏：端口总数、过滤结果数、选中数、操作提示、最后刷新时间
- 非管理员运行时，窗口标题带「（非管理员）」后缀，状态栏同时给出提权提示
- `F5` 刷新；对话框内 `Tab` 切换焦点、`Esc` 关闭

## 项目结构

```
├── build.rs                     # 用 rc.exe 编译 assets/app.rc，嵌入图标与清单
├── scripts/                     # 辅助脚本（截图 / 内存与启动耗时测量）
│   └── bench/                   # 重写前的内存参照程序，见「实测指标」（历史记录，不参与主程序构建）
├── assets/
│   ├── make_icon.py             # 生成 app.ico 与 icon.png（纯标准库）
│   ├── app.ico / icon.png       # 程序图标（资源 / 窗口）
│   ├── app.rc                   # 资源脚本
│   └── app.manifest             # asInvoker + per-monitor v2 DPI 感知 + longPathAware
└── src/
    ├── main.rs                  # 入口：日志、启动计时、消息循环
    ├── app.rs                   # 应用状态与业务编排（界面层只经由它触碰业务）
    ├── models/port_info.rs      # PortInfo 数据结构
    ├── services/
    │   ├── port_service.rs      # IP Helper API 端口枚举
    │   ├── port_query.rs        # 过滤与精确查询（与 UI 解耦，便于测试）
    │   ├── process_service.rs   # 进程详情、结束进程（含进程树）、打开文件位置
    │   ├── autostart_service.rs # 开机自启（注册表读写）
    │   └── tray_service.rs      # 系统托盘（独立线程 + 消息循环）
    ├── ui/
    │   ├── mod.rs               # 公共 Win32 辅助：字体、DPI 换算、主题切换、控件遍历
    │   ├── main_window.rs       # 窗口类注册、消息循环与全部消息分发
    │   ├── toolbar.rs           # 搜索、精确查询、刷新、导出、结束、主题、设置
    │   ├── table.rs             # 端口表格：排序、多选、双击、右键菜单
    │   ├── status_bar.rs        # 状态栏（深色主题下改为自绘）
    │   ├── dialogs.rs           # 端口详情与结束进程确认（自建窗口类的模态框）
    │   └── dark_mode.rs         # 深色控件主题的进程级开关
    └── utils/
        ├── error.rs             # AppError（端口查询 / 进程 / 权限 / 路径）
        ├── format.rs            # 字节数格式化（进程内存占用）
        ├── clipboard.rs         # 剪贴板读写
        ├── file_dialog.rs       # 系统「另存为」对话框
        ├── logger.rs            # 极简 log 实现（默认关闭）
        ├── timing.rs            # 启动耗时打点
        ├── theme.rs             # 系统深色 / 浅色检测（读注册表）
        └── permission.rs        # 管理员权限检测
```

服务层不依赖界面层：`services/` 与 `utils/` 里没有任何 `ui::` 引用，界面层也只经由 `app.rs`
的方法触碰业务。因此 `PortService` / `PortQuery` / `ProcessService` 都能脱离窗口单独测试。

## 测试

`cargo test` 共 38 个用例，覆盖：端口枚举与字段合法性、端口号与 IPv4 地址的字节序解析、
扫描结果有序性与刷新一致性、搜索与精确查询（命中/未命中/组合条件/下标正确性）、进程详情查询、
结束普通进程与进程树、权限错误提示、路径比较、注册表读写往返与清理、剪贴板读写往返、
字节数格式化、详情文本的字段完整性、托盘启动/停止与幂等关闭、托盘事件码往返、
托盘与自启的宽字符编码、日志路径拼接。

以下场景需要人工验证：

- 真实的非管理员场景：本机以内置 Administrator 账户运行，UAC 对该账户不做令牌过滤，
  因此无法构造出「已提权但未过滤」以外的令牌，提权提示分支只能靠代码审查确认
- 真实的权限不足场景：需要以普通用户身份运行并尝试结束系统进程，自动化测试会危及系统稳定性，
  故仅覆盖同一条错误提示路径（无法打开的 PID）
- 开机自启的实际开机生效（需重启系统；注册表写入本身已在自动化测试与手工操作中确认）

## 辅助脚本

`scripts/` 下的两个 PowerShell 脚本用于复现下面的实测数据：

```powershell
pwsh -File scripts\measure.ps1                 # 多次运行取内存与启动耗时中位数（并区分有无第三方注入）
pwsh -File scripts\screenshot.ps1              # 截图（窗口会被钉到左上角并置顶）

# 先模拟点击再截图；-ClickAt 支持 x,y;x,y 序列，前缀 R 表示右键、坐标后加 ,2 表示双击
pwsh -File scripts\screenshot.ps1 -ClickAt "1326,64"
pwsh -File scripts\screenshot.ps1 -ClickAt "200,155,2" -Crop "380,170,960,780"
pwsh -File scripts\screenshot.ps1 -ClickAt "400,69" -Keys "vmware" -Crop "0,45,1700,120" -Zoom 2
```

`-Crop "x,y,w,h"` 额外导出一块区域到 `screenshot.crop.png`，`-Zoom`（默认 2）指定放大倍数，
`-Keys` 在点击序列之后发送按键（SendKeys 语法）——整屏图缩到终端后看不清边框、提示文字这类
小元素，核对细节都靠裁剪放大。两个脚本都已在 150% 缩放的屏幕上验证过，并用它们确认了：
高 DPI 下布局正常、搜索框输入即筛选、双击弹出详情、右键菜单与「设置」下拉菜单、深浅主题切换、
关闭到托盘后单击托盘图标可恢复窗口、托盘菜单可以正常退出。

## 实测指标（Release 构建）

测量方法：

- 内存：启动后静置 8 秒，取多次运行的中位数。三个口径一并列出以免歧义——
  工作集总计读 `Win32_Process.WorkingSetSize`，提交大小读 `PrivatePageCount`，
  专用工作集读性能计数器 `\Process(<实例>)\Working Set - Private`（任务管理器「内存」列）。
  计数器的实例名由 `ID Process` 反查得到，不用进程名硬拼
- 启动耗时：程序入口打点，主窗口显示时记录（`HYDRAPORT_LOG=1` 写日志），取多次运行的中位数

| 指标 | 实测 | 开发文档目标 |
|------|------|--------------|
| 二进制体积 | 648 KB | < 15 MB ✅ |
| 启动耗时（进程入口 → 主窗口显示） | 185 ms（8 次：167 / 179 / 182 / 184 / 185 / 186 / 189 / 247） | < 300 ms ✅ |
| 空闲内存（专用工作集） | 1.7 MB（无第三方注入时） | < 30 MB ✅ |

发布产物是真正单文件的：`.cargo/config.toml` 里开了 `+crt-static` 静态链接 C 运行库，
不依赖 `VCRUNTIME140.dll` / `VCRUNTIME140_1.dll`（这两个不是系统组件，目标机器没装
VC++ 可再发行组件时会直接起不来）。代价是二进制多约 96 KB，内存没有可测量的变化。
运行期只依赖 `ucrtbase.dll` 一类 Windows 10 起的系统组件。

内存的横向对比（中位数，启动后静置 8 秒读数）：

| 程序 | 工作集总计 | 提交大小 | 专用工作集 |
|------|-----------|---------|-----------|
| 重写前：本程序（egui + eframe）※ | 64.1 MB | 70.3 MB | 35.5 MB |
| 重写前：空白 eframe 窗口（只有框架，无任何业务代码）※ | 57.6 MB | 64.3 MB | 30.1 MB |
| 参照程序：原生 Win32 空窗口 + 300 行 ListView（`scripts/bench/`） | 10.8 MB | 2.0 MB | 1.4 MB |
| **本程序（原生 Win32 控件）** | **13.9 MB** | **2.5 MB** | **1.7 MB** |
| 参照程序（同一轮，被输入法注入时） | 25.0 MB | 8.6 MB | 6.9 MB |
| 本程序（同一轮，被输入法注入时） | 26.8 MB | 8.8 MB | 7.0 MB |

※ 重写前那一轮没有记录模块注入情况，因此无法区分下面说的两种口径。

## 读数为什么会跳动：第三方输入法注入

同一个 exe 连跑 8 次，专用工作集会在 1.7 MB 和 7.0 MB 之间跳，模块数在 38 和 58 之间跳。
差值不是程序自己的——是本机的微信输入法（WeType）在作怪：它会把自己的 DLL 注入到任何取得
焦点、带编辑框的窗口进程里，顺带拖进 `d2d1` / `DWrite` / `SHELL32` / `windows.storage`
一长串依赖。

```
wetype_tip.dll       -> C:\Windows\system32\wetype_tip.dll
wetype_tip_core.dll  -> C:\Program Files\Tencent\WeType\WetypeCore_2.1.3.18\x64\
CrashRpt1500.dll     -> C:\Program Files\Tencent\WeType\WetypeCore_2.1.3.18\x64\
```

这是注入到**任何**窗口进程的，与用不用 GUI 框架无关：那个只有空窗口的参照程序同样会在
10.8 MB 和 25.0 MB 之间跳。所以：

- 程序自身的内存开销，看「无第三方注入」那一行：本程序 13.9 / 2.5 / 1.7，参照程序
  10.8 / 2.0 / 1.4。也就是说整套界面加上全部业务逻辑，只比一个空窗口多约 3 MB 工作集、
  0.5 MB 提交
- 注入带来的约 13 MB 工作集 / 6.3 MB 提交，两种口径下都一样，可以整体扣掉

`scripts/measure.ps1` 已经把这件事做进测量里：每次运行都记录模块数，并把 `System32` 以外的
模块列出来，最后把「有注入」和「无注入」两组分开报中位数，避免中位数被搅成一个没有意义的
中间值。目标口径（< 30 MB）在两种情况下都满足。

## 界面层为什么重写为原生 Win32

原先的界面层是 egui + eframe。它满足功能与启动耗时要求，但空闲内存卡在 35.5 MB，
达不到开发文档 §1.3 的 30 MB 目标。拆解后发现瓶颈不在业务代码：

| 模块 | 映像大小 | 说明 |
|------|---------|------|
| `nvoglv64.dll` | 46.5 MB | NVIDIA OpenGL 驱动（ICD） |
| `nvgpucomp64.dll` | 94.0 MB | NVIDIA 着色器编译器 |
| `uiautomationcore.dll` | 4.2 MB | 无障碍支持（accesskit 引入，已关闭） |

eframe 的 OpenGL 后端会把显卡驱动整块拉进进程，**光是创建一个空白 eframe 窗口就已经占到
30.1 MB 专用工作集**，业务代码只在此基础上增加约 5 MB。也就是说只要界面层还是
egui + eframe，30 MB 在架构上就不可达，与业务代码写得多省无关。

开发文档 §2 把 winsafe / native-windows-gui 列为「备选 GUI」，§15 又规定
「如有任何技术方案冲突，以『低内存 + 稳定性』为最高优先级进行取舍」。据此把界面层整体重写为
直接调用 Win32 API（windows-sys 绑定，不引入任何 GUI 框架）。重写没有改动服务层与业务逻辑，
功能清单逐条对齐，二进制反而从 4.02 MB 降到 648 KB。

顺带消失的开销：

1. **不再加载任何字体文件**。原先 egui 内置字体不含中文，必须把微软雅黑（18.8 MB）映射进进程；
   现在界面字体直接取自系统消息字体（`SystemParametersInfoW(SPI_GETNONCLIENTMETRICS)` 的
   `lfMessageFont`），中文渲染正确，且零加载、零拷贝。
2. **不再有 accesskit / UI Automation**，`uiautomationcore.dll` 不再出现在进程里。
3. **不再有 GPU 驱动与着色器编译器**，工作集里最大的一块共享映像页就此消失。

重写后与参照程序的读数几乎重合（同一口径下 13.9 / 2.5 / 1.7 对 10.8 / 2.0 / 1.4），
说明这就是这套界面方案的下限，业务代码本身几乎不占额外内存。同一份业务代码在 eframe 下
比空白 eframe 窗口多出约 5 MB，在原生 Win32 下只比空窗口多约 0.3 MB——多出来的那部分
主要不是数据本身，而是 egui 为同样这些行分配的控件与顶点缓冲。

## 已知限制

- **深色主题依赖 uxtheme 的两个未公开序号导出**（135 `SetPreferredAppMode`、133
  `AllowDarkModeForWindow`）。Windows 的深色控件主题必须先把进程切到「允许深色」模式才会生效，
  而这一步没有公开 API。这两个序号自 Windows 10 1809 起一直存在且行为稳定，
  取不到时会静默降级（控件维持系统默认的浅色外观，功能不受影响），不会崩溃。
  状态栏是其中唯一没有深色皮肤的控件，深色主题下它的三段改为自绘（`SBT_OWNERDRAW` + `WM_DRAWITEM`），
  浅色主题下仍走系统默认外观。
- 按钮（包括对话框里的复选框）没有官方深色主题，沿用系统外观；对话框里复选框的文字颜色
  由 `WM_CTLCOLORBTN` 单独指定，否则会是深色背景上的黑字。
- 「关闭窗口时最小化到托盘」在托盘创建失败（例如资源管理器未运行）时会自动禁用，
  避免窗口隐藏后再也无法打开。
- 开机自启写入的是当前可执行文件的绝对路径，移动 exe 后需要重新开关一次。
