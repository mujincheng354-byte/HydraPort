# 启动程序、按需模拟点击、整屏截图并报告内存。
#
#   pwsh -File scripts\screenshot.ps1                                  # 只截图
#   pwsh -File scripts\screenshot.ps1 -ClickAt "1326,64"               # 先点一下再截图
#   pwsh -File scripts\screenshot.ps1 -ClickAt "2141,1562;R2054,1473"  # 点击序列，R 前缀为右键
#   pwsh -File scripts\screenshot.ps1 -Crop "0,45,1700,45" -Zoom 3      # 额外导出放大后的局部图
#
# 窗口会被钉到屏幕左上角并置顶，保证点击坐标可复现。
# 注意：本脚本必须先声明 DPI 感知，否则截图按物理像素、点击按虚拟化坐标，两者对不上。

param(
    [string]$Exe = "",
    [string]$Out = "",
    [int]$WaitSeconds = 8,
    [string]$ClickAt = "",
    [int]$ClickDelayMs = 1200,
    # 额外导出一块区域用于放大查看，格式 "x,y,w,h"
    [string]$Crop = "",
    # 裁剪图放大倍数
    [int]$Zoom = 2,
    # 点击序列之后发送的按键（SendKeys 语法，如 "8080"）
    [string]$Keys = ""
)

if ($Exe -eq "") {
    $Exe = Join-Path (Split-Path $PSScriptRoot -Parent) "target\release\port-manager.exe"
}
if ($Out -eq "") {
    $Out = Join-Path (Split-Path $PSScriptRoot -Parent) "target\screenshot.png"
}

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Dpi { [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); }
public class Win {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, IntPtr pid);
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, IntPtr e);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
[void][Dpi]::SetProcessDPIAware()

# 抢前台：后台进程直接调 SetForegroundWindow 会被系统忽略，
# 先把自己的线程输入队列挂到当前前台线程上才能生效。
function Set-Foreground([IntPtr]$h) {
    $fg = [Win]::GetForegroundWindow()
    $target = [Win]::GetWindowThreadProcessId($h, [IntPtr]::Zero)
    $current = [Win]::GetWindowThreadProcessId($fg, [IntPtr]::Zero)
    if ($target -ne $current) {
        [void][Win]::AttachThreadInput($current, $target, $true)
        [void][Win]::SetForegroundWindow($h)
        [void][Win]::AttachThreadInput($current, $target, $false)
    } else {
        [void][Win]::SetForegroundWindow($h)
    }
}

function Invoke-MouseClick([int]$x, [int]$y, [bool]$right) {
    [void][Win]::SetCursorPos($x, $y)
    Start-Sleep -Milliseconds 150
    if ($right) {
        [Win]::mouse_event(0x0008, 0, 0, 0, [IntPtr]::Zero)  # RIGHTDOWN
        Start-Sleep -Milliseconds 60
        [Win]::mouse_event(0x0010, 0, 0, 0, [IntPtr]::Zero)  # RIGHTUP
    } else {
        [Win]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero)  # LEFTDOWN
        Start-Sleep -Milliseconds 60
        [Win]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero)  # LEFTUP
    }
}

$proc = Start-Process -FilePath $Exe -PassThru
Start-Sleep -Seconds $WaitSeconds

$handle = [IntPtr]::Zero
foreach ($p in Get-Process -Id $proc.Id) {
    if ($p.MainWindowHandle -ne [IntPtr]::Zero -and [Win]::IsWindowVisible($p.MainWindowHandle)) {
        $r = New-Object Win+RECT
        [void][Win]::GetWindowRect($p.MainWindowHandle, [ref]$r)
        if (($r.Right - $r.Left) -gt 400) { $handle = $p.MainWindowHandle; break }
    }
}

if ($handle -eq [IntPtr]::Zero) {
    Write-Output "NO_WINDOW_FOUND（窗口可能已被最小化到托盘）"
} else {
    # 钉到 (0,0) 并置顶：默认窗口位置是层叠的，点击坐标无法复现
    [void][Win]::SetWindowPos($handle, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0040)
    Set-Foreground $handle
    Start-Sleep -Milliseconds 900

    if ($ClickAt -ne "") {
        # 支持 "x1,y1;x2,y2" 这样的点击序列；步骤前缀 R 表示右键，坐标后加 ",2" 表示快速双击
        foreach ($step in $ClickAt.Split(";")) {
            $right = $step.StartsWith("R")
            $parts = ($step -replace "^R", "").Split(",")
            $x = [int]$parts[0]
            $y = [int]$parts[1]
            Invoke-MouseClick $x $y $right
            if ($parts.Count -ge 3 -and $parts[2] -eq "2") {
                Start-Sleep -Milliseconds 60
                Invoke-MouseClick $x $y $false
            }
            Start-Sleep -Milliseconds $ClickDelayMs
        }
    }

    # 输入筛选关键字等：必须先让窗口处于前台，否则按键会打到别的窗口上
    if ($Keys -ne "") {
        Set-Foreground $handle
        Start-Sleep -Milliseconds 300
        [System.Windows.Forms.SendKeys]::SendWait($Keys)
        Start-Sleep -Milliseconds $ClickDelayMs
    }

    # 整屏截图：窗口带 DWM 阴影与缩放，按窗口矩形截图会裁掉边缘
    $screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap($screen.Width, $screen.Height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($screen.X, $screen.Y, 0, 0, $bmp.Size)
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose()

    # 细节核对靠裁剪放大：整屏图缩到终端后看不清边框、提示文字这类小元素
    if ($Crop -ne "") {
        $parts = $Crop.Split(",")
        $rect = New-Object System.Drawing.Rectangle([int]$parts[0], [int]$parts[1], [int]$parts[2], [int]$parts[3])
        $zoom = [Math]::Max(1, $Zoom)
        $cropOut = [System.IO.Path]::ChangeExtension($Out, $null) + "crop.png"
        # 变量名不能叫 $crop：PowerShell 不区分大小写，会和 $Crop 参数撞车
        $cropped = New-Object System.Drawing.Bitmap($rect.Width, $rect.Height)
        $cg = [System.Drawing.Graphics]::FromImage($cropped)
        $cg.DrawImage($bmp, (New-Object System.Drawing.Rectangle(0, 0, $rect.Width, $rect.Height)), $rect, [System.Drawing.GraphicsUnit]::Pixel)
        $cg.Dispose()
        $scaled = New-Object System.Drawing.Bitmap(($rect.Width * $zoom), ($rect.Height * $zoom))
        $sg = [System.Drawing.Graphics]::FromImage($scaled)
        $sg.InterpolationMode = "NearestNeighbor"
        $sg.DrawImage($cropped, 0, 0, $scaled.Width, $scaled.Height)
        $sg.Dispose(); $cropped.Dispose()
        $scaled.Save($cropOut, [System.Drawing.Imaging.ImageFormat]::Png)
        $scaled.Dispose()
        Write-Output "CROP $cropOut ($($rect.Width) x $($rect.Height) x$zoom)"
    }

    $bmp.Dispose()
    Write-Output "SAVED $Out ($($screen.Width) x $($screen.Height))"
}

$m = Get-CimInstance Win32_Process -Filter "ProcessId = $($proc.Id)"
if ($m) {
    Write-Output ("WS_MB  = {0:N1}" -f ($m.WorkingSetSize / 1MB))
    Write-Output ("PRIV_MB= {0:N1}" -f ($m.PrivatePageCount / 1MB))
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
}
