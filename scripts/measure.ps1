# 内存 / 启动耗时测量：多次运行取中位数，避免首次运行的预热偏差。
#
#   pwsh -File scripts\measure.ps1
#
# 内存三个口径都读，避免歧义：
#   WorkingSetSize     工作集总计（含共享的 DLL 映像页）
#   PrivatePageCount   提交大小（私有字节）
#   Working Set - Private  专用工作集，即任务管理器「内存」列
# 启动耗时只统计「进程启动 -> 主窗口可见」，首帧耗时见程序自身的日志（HYDRAPORT_LOG=1）。
#
# 关于第三方输入法：微信输入法（WeType）之类的 IME 会把自己的 DLL 注入到任何取得
# 焦点的窗口进程里，一并拖进 d2d1 / DWrite / SHELL32 / windows.storage，读数因此会
# 多出十几个 MB。同一个 exe 在两次运行之间就能差出一倍，所以每次运行都会记录模块数，
# 并把 System32 以外的模块列出来——看到注入提示就说明这次读的是「带输入法」的口径。

param(
    [string]$Exe = "",
    [int]$Runs = 5,
    [int]$SettleSeconds = 8
)

if ($Exe -eq "") {
    $Exe = Join-Path (Split-Path $PSScriptRoot -Parent) "target\release\port-manager.exe"
}
if (-not (Test-Path $Exe)) {
    Write-Error "找不到 $Exe，请先执行 cargo build --release"
    exit 1
}

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class M {
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
}
"@

# 用性能计数器的 ID Process 反查实例名：同名进程多开时，直接写 \Process(名字)\
# 会读到别的实例（甚至是上一次运行残留的那个），数字就全是错的。
function Get-PrivateWorkingSet([int]$TargetPid) {
    $map = (Get-Counter "\Process(*)\ID Process" -ErrorAction SilentlyContinue).CounterSamples
    $instance = $map | Where-Object { [int]$_.CookedValue -eq $TargetPid } | Select-Object -First 1
    if (-not $instance) { return 0 }
    $path = "\Process(" + $instance.InstanceName + ")\Working Set - Private"
    $samples = (Get-Counter $path -ErrorAction SilentlyContinue).CounterSamples
    if ($samples) { return $samples[0].CookedValue / 1MB }
    return 0
}

$results = @()
for ($i = 1; $i -le $Runs; $i++) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = Start-Process -FilePath $Exe -PassThru

    $windowMs = -1
    while ($sw.Elapsed.TotalSeconds -lt 20) {
        $p = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
        if ($p -and $p.MainWindowHandle -ne [IntPtr]::Zero -and [M]::IsWindowVisible($p.MainWindowHandle)) {
            $windowMs = $sw.Elapsed.TotalMilliseconds
            break
        }
        Start-Sleep -Milliseconds 5
    }

    Start-Sleep -Seconds $SettleSeconds
    $m = Get-CimInstance Win32_Process -Filter "ProcessId = $($proc.Id)"
    $ws = $m.WorkingSetSize / 1MB
    $commit = $m.PrivatePageCount / 1MB
    $priv = Get-PrivateWorkingSet $proc.Id

    # System32 / WinSxS 以外的模块基本都是被注入进来的第三方组件（被测程序自己除外）
    $modules = @((Get-Process -Id $proc.Id -ErrorAction SilentlyContinue).Modules)
    $injected = @($modules | Where-Object {
        $_.FileName -and $_.ModuleName -ne (Split-Path $Exe -Leaf) -and
        $_.FileName -notmatch '(?i)\\Windows\\(System32|SysWOW64|WinSxS|SystemApps)\\' -and
        $_.FileName -notmatch '(?i)\\Windows\\[^\\]+\.dll$'
    } | ForEach-Object { $_.ModuleName })

    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 1500

    $results += [pscustomobject]@{
        Run = $i; WindowMs = [int]$windowMs; WS_MB = $ws; Commit_MB = $commit
        Priv_MB = $priv; Modules = $modules.Count; Injected = $injected
    }
    Write-Output ("run {0}: 窗口可见 {1} ms, 工作集 {2:N1} MB, 提交 {3:N1} MB, 专用工作集 {4:N1} MB, 模块 {5} 个" -f `
        $i, [int]$windowMs, $ws, $commit, $priv, $modules.Count)
    if ($injected.Count -gt 0) {
        Write-Output ("        注意：检测到注入模块 {0} 个（{1}），本次读数含第三方输入法开销" -f `
            $injected.Count, ($injected -join ", "))
    }
}

function Get-Median($values) {
    $sorted = $values | Sort-Object
    $mid = [int][math]::Floor($sorted.Count / 2)
    if ($sorted.Count % 2 -eq 1) { return $sorted[$mid] }
    return ($sorted[$mid - 1] + $sorted[$mid]) / 2
}

Write-Output ""
Write-Output ("中位数：窗口可见 {0:N0} ms | 工作集 {1:N1} MB | 提交 {2:N1} MB | 专用工作集 {3:N1} MB" -f `
    (Get-Median $results.WindowMs), (Get-Median $results.WS_MB), (Get-Median $results.Commit_MB), (Get-Median $results.Priv_MB))

# 两种口径差别很大，分开报，免得中位数被搅成没有意义的中间值
$clean = @($results | Where-Object { $_.Injected.Count -eq 0 })
$dirty = @($results | Where-Object { $_.Injected.Count -gt 0 })
if ($clean.Count -gt 0 -and $dirty.Count -gt 0) {
    Write-Output ""
    Write-Output ("无第三方注入的 {0} 次：专用工作集 {1:N1} MB" -f $clean.Count, (Get-Median $clean.Priv_MB))
    Write-Output ("有第三方注入的 {0} 次：专用工作集 {1:N1} MB" -f $dirty.Count, (Get-Median $dirty.Priv_MB))
}
