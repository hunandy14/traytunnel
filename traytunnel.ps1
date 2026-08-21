param([switch]$Tray)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

Add-Type -Namespace Win32 -Name Native -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool ReleaseCapture();
[DllImport("user32.dll")] public static extern int SendMessage(IntPtr hWnd, int Msg, int wParam, int lParam);
[DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
[DllImport("dwmapi.dll")] public static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int val, int size);
[DllImport("uxtheme.dll", CharSet=CharSet.Unicode)] public static extern int SetWindowTheme(IntPtr hWnd, string sub, string ids);
[DllImport("uxtheme.dll", EntryPoint="#135", CharSet=CharSet.Unicode)] public static extern int SetPreferredAppMode(int mode);
'@

try {
    if (-not [Win32.Native]::SetProcessDpiAwarenessContext([IntPtr]::new(-4))) {
        [void][Win32.Native]::SetProcessDPIAware()
    }
}
catch { try { [void][Win32.Native]::SetProcessDPIAware() } catch {} }

[System.Windows.Forms.Application]::EnableVisualStyles()

$script:crashLog = Join-Path $PSScriptRoot "traytunnel-error.log"
[System.Windows.Forms.Application]::SetUnhandledExceptionMode([System.Windows.Forms.UnhandledExceptionMode]::CatchException)
[System.Windows.Forms.Application]::add_ThreadException({
    param($sender, $e)
    try { "$(Get-Date -Format 'u')  $($e.Exception.ToString())`n" | Add-Content $script:crashLog } catch {}
})

try { [void][Win32.Native]::SetPreferredAppMode(2) } catch {}

$g = [System.Drawing.Graphics]::FromHwnd([IntPtr]::Zero)
$script:scale = $g.DpiX / 96.0
$g.Dispose()
function S([float]$v) { [int][Math]::Round($v * $script:scale) }
function P([float]$x, [float]$y) { New-Object System.Drawing.Point((S $x), (S $y)) }
function SZ([float]$w, [float]$h) { New-Object System.Drawing.Size((S $w), (S $h)) }

$script:appMutex = New-Object System.Threading.Mutex($false, "Local\traytunnel-singleton")
try { $script:isPrimary = $script:appMutex.WaitOne(0) }
catch [System.Threading.AbandonedMutexException] { $script:isPrimary = $true }
$script:showEvent = New-Object System.Threading.EventWaitHandle($false, [System.Threading.EventResetMode]::AutoReset, "Local\traytunnel-show")
if (-not $script:isPrimary) {
    [void]$script:showEvent.Set()
    exit
}

$script:configPath = Join-Path $PSScriptRoot "traytunnel.json"
$script:wantRun = $true
$script:proc = $null
$script:connected = $false
$script:retryAt = Get-Date
$script:jobs = @{}
$script:trayHintShown = $false

$cBg = [System.Drawing.Color]::FromArgb(255, 24, 26, 31)
$cTitle = [System.Drawing.Color]::FromArgb(255, 19, 21, 25)
$cCard = [System.Drawing.Color]::FromArgb(255, 34, 37, 44)
$cLogBg = [System.Drawing.Color]::FromArgb(255, 19, 21, 25)
$cText = [System.Drawing.Color]::FromArgb(255, 230, 231, 235)
$cMuted = [System.Drawing.Color]::FromArgb(255, 138, 142, 152)
$cAccent = [System.Drawing.Color]::FromArgb(255, 45, 212, 167)
$cAmber = [System.Drawing.Color]::FromArgb(255, 251, 191, 36)
$cRed = [System.Drawing.Color]::FromArgb(255, 248, 113, 113)
$cBtn = [System.Drawing.Color]::FromArgb(255, 45, 49, 58)
$cBtnHover = [System.Drawing.Color]::FromArgb(255, 58, 63, 74)
$cCloseHover = [System.Drawing.Color]::FromArgb(255, 196, 43, 28)

function Get-DefaultConfig {
    [pscustomobject]@{
        host = "your-host.example.com"
        user = "your-user"
        proxyCommand = "cloudflared access ssh --hostname %h"
        closeToTray = $true
        forwards = @(
            [pscustomobject]@{ name = "exit-a"; local = 1080; remote = "127.0.0.1:1080" },
            [pscustomobject]@{ name = "exit-b"; local = 1083; remote = "127.0.0.1:1083" }
        )
    }
}

function Load-Config {
    $cfg = $null
    if (Test-Path $script:configPath) {
        try { $cfg = Get-Content $script:configPath -Raw | ConvertFrom-Json } catch { }
    }
    if (-not $cfg) {
        $cfg = Get-DefaultConfig
        $cfg | ConvertTo-Json -Depth 5 | Set-Content $script:configPath -Encoding UTF8
        return $cfg
    }
    if (-not $cfg.PSObject.Properties['closeToTray']) {
        $cfg | Add-Member -NotePropertyName closeToTray -NotePropertyValue $true
    }
    return $cfg
}

$script:cfg = Load-Config

function Get-SshArgs {
    $parts = @("-N",
        "-o ServerAliveInterval=30",
        "-o ServerAliveCountMax=3",
        "-o ExitOnForwardFailure=yes",
        "-o StrictHostKeyChecking=accept-new")
    if ($script:cfg.proxyCommand) {
        $parts += "-o `"ProxyCommand=$($script:cfg.proxyCommand)`""
    }
    foreach ($f in $script:cfg.forwards) {
        $parts += "-L $($f.local):$($f.remote)"
    }
    $parts += "$($script:cfg.user)@$($script:cfg.host)"
    return ($parts -join " ")
}

$script:runKeyPath = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$script:runKeyName = "traytunnel"

function Test-Autostart {
    $null -ne (Get-ItemProperty -Path $script:runKeyPath -Name $script:runKeyName -ErrorAction SilentlyContinue)
}

function Set-Autostart([bool]$on) {
    if ($on) {
        $psExe = (Get-Process -Id $PID).Path
        $cmd = '"{0}" -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "{1}" -Tray' -f $psExe, $PSCommandPath
        Set-ItemProperty -Path $script:runKeyPath -Name $script:runKeyName -Value $cmd -Force
    }
    else {
        Remove-ItemProperty -Path $script:runKeyPath -Name $script:runKeyName -Force -ErrorAction SilentlyContinue
    }
}

$iconPath = Join-Path $PSScriptRoot "traytunnel.ico"
$appIcon = if (Test-Path $iconPath) { New-Object System.Drawing.Icon($iconPath) } else { [System.Drawing.SystemIcons]::Application }

function New-FlatButton([string]$text, [float]$x, [float]$y, [float]$w, [float]$h) {
    $b = New-Object System.Windows.Forms.Button
    $b.Text = $text
    $b.Location = P $x $y
    $b.Size = SZ $w $h
    $b.FlatStyle = "Flat"
    $b.FlatAppearance.BorderSize = 0
    $b.BackColor = $cBtn
    $b.ForeColor = $cText
    $b.FlatAppearance.MouseOverBackColor = $cBtnHover
    $b.FlatAppearance.MouseDownBackColor = $cBtn
    $b.Cursor = "Hand"
    return $b
}

function Add-TitleBar($window, [string]$title, [bool]$canMinimize, [scriptblock]$onClose) {
    $bar = New-Object System.Windows.Forms.Panel
    $bar.Location = P 0 0
    $bar.Size = New-Object System.Drawing.Size($window.ClientSize.Width, (S 36))
    $bar.BackColor = $cTitle
    $window.Controls.Add($bar)

    $ic = New-Object System.Windows.Forms.PictureBox
    $ic.Location = P 12 9
    $ic.Size = SZ 18 18
    $ic.SizeMode = "Zoom"
    try { $ic.Image = (New-Object System.Drawing.Icon($appIcon, (S 18), (S 18))).ToBitmap() }
    catch { try { $ic.Image = $appIcon.ToBitmap() } catch {} }
    $bar.Controls.Add($ic)

    $tl = New-Object System.Windows.Forms.Label
    $tl.Location = P 38 0
    $tl.Size = New-Object System.Drawing.Size((S 220), (S 36))
    $tl.TextAlign = "MiddleLeft"
    $tl.Font = New-Object System.Drawing.Font("Segoe UI Semibold", 9)
    $tl.ForeColor = $cText
    $tl.Text = $title
    $bar.Controls.Add($tl)

    $drag = {
        if ($_.Button -eq "Left") {
            [void][Win32.Native]::ReleaseCapture()
            [void][Win32.Native]::SendMessage($this.FindForm().Handle, 0xA1, 2, 0)
        }
    }
    $bar.Add_MouseDown($drag)
    $tl.Add_MouseDown($drag)
    $ic.Add_MouseDown($drag)

    $btnW = S 44
    $barH = S 36
    $closeBtn = New-Object System.Windows.Forms.Button
    $closeBtn.Size = New-Object System.Drawing.Size($btnW, $barH)
    $closeBtn.Location = New-Object System.Drawing.Point(($window.ClientSize.Width - $btnW), 0)
    $closeBtn.FlatStyle = "Flat"
    $closeBtn.FlatAppearance.BorderSize = 0
    $closeBtn.BackColor = $cTitle
    $closeBtn.ForeColor = $cText
    $closeBtn.FlatAppearance.MouseOverBackColor = $cCloseHover
    $closeBtn.Font = New-Object System.Drawing.Font("Segoe MDL2 Assets", 9)
    $closeBtn.Text = [string][char]0xE8BB
    $closeBtn.Add_Click($onClose)
    $bar.Controls.Add($closeBtn)

    if ($canMinimize) {
        $minBtn = New-Object System.Windows.Forms.Button
        $minBtn.Size = New-Object System.Drawing.Size($btnW, $barH)
        $minBtn.Location = New-Object System.Drawing.Point(($window.ClientSize.Width - 2 * $btnW), 0)
        $minBtn.FlatStyle = "Flat"
        $minBtn.FlatAppearance.BorderSize = 0
        $minBtn.BackColor = $cTitle
        $minBtn.ForeColor = $cText
        $minBtn.FlatAppearance.MouseOverBackColor = $cBtnHover
        $minBtn.Font = New-Object System.Drawing.Font("Segoe MDL2 Assets", 9)
        $minBtn.Text = [string][char]0xE921
        $minBtn.Add_Click({ $this.FindForm().WindowState = "Minimized" })
        $bar.Controls.Add($minBtn)
    }
    return $bar
}

function Set-RoundCorners($window) {
    try { $pref = 2; [void][Win32.Native]::DwmSetWindowAttribute($window.Handle, 33, [ref]$pref, 4) } catch {}
}

$form = New-Object System.Windows.Forms.Form
$form.Text = "Traytunnel"
$form.FormBorderStyle = "None"
$form.StartPosition = "CenterScreen"
$form.BackColor = $cBg
$form.Font = New-Object System.Drawing.Font("Segoe UI", 9)
$form.Icon = $appIcon
$form.ShowInTaskbar = $true
$form.ClientSize = SZ 464 456

$titleBar = Add-TitleBar $form "Traytunnel" $true { Invoke-CloseButton }
Set-RoundCorners $form

$statusDot = New-Object System.Windows.Forms.Label
$statusDot.Location = P 18 56
$statusDot.Size = SZ 30 32
$statusDot.Font = New-Object System.Drawing.Font("Segoe UI", 16)
$statusDot.Text = [char]0x25CF
$statusDot.ForeColor = $cMuted
$form.Controls.Add($statusDot)

$statusText = New-Object System.Windows.Forms.Label
$statusText.Location = P 50 52
$statusText.Size = SZ 190 28
$statusText.Font = New-Object System.Drawing.Font("Segoe UI Semibold", 14)
$statusText.ForeColor = $cText
$statusText.Text = "Starting..."
$form.Controls.Add($statusText)

$statusSub = New-Object System.Windows.Forms.Label
$statusSub.Location = P 52 81
$statusSub.Size = SZ 190 18
$statusSub.ForeColor = $cMuted
$statusSub.Text = "ssh tunnel"
$form.Controls.Add($statusSub)

$glyphStop = [string][char]0xE71A
$glyphStart = [string][char]0xE768
$mdlFont = New-Object System.Drawing.Font("Segoe MDL2 Assets", 11)
$tips = New-Object System.Windows.Forms.ToolTip
$tips.BackColor = $cCard
$tips.ForeColor = $cText

$settingsBtn = New-FlatButton ([string][char]0xE713) 350 58 30 32
$settingsBtn.Font = $mdlFont
$form.Controls.Add($settingsBtn)
$tips.SetToolTip($settingsBtn, "Settings")

$retestBtn = New-FlatButton ([string][char]0xE72C) 384 58 30 32
$retestBtn.Font = $mdlFont
$form.Controls.Add($retestBtn)
$tips.SetToolTip($retestBtn, "Retest exits")

$toggleBtn = New-FlatButton $glyphStop 418 58 30 32
$toggleBtn.Font = $mdlFont
$toggleBtn.ForeColor = $cRed
$form.Controls.Add($toggleBtn)
$tips.SetToolTip($toggleBtn, "Stop")

$sectionExits = New-Object System.Windows.Forms.Label
$sectionExits.Location = P 20 116
$sectionExits.Size = SZ 200 18
$sectionExits.ForeColor = $cMuted
$sectionExits.Font = New-Object System.Drawing.Font("Segoe UI", 8.5)
$sectionExits.Text = "EXIT NODES"
$form.Controls.Add($sectionExits)

$cardsPanel = New-Object System.Windows.Forms.Panel
$cardsPanel.Location = P 18 138
$cardsPanel.Size = SZ 428 10
$cardsPanel.BackColor = $cBg
$form.Controls.Add($cardsPanel)

$sectionLog = New-Object System.Windows.Forms.Label
$sectionLog.Size = SZ 200 18
$sectionLog.ForeColor = $cMuted
$sectionLog.Font = New-Object System.Drawing.Font("Segoe UI", 8.5)
$sectionLog.Text = "ACTIVITY"
$form.Controls.Add($sectionLog)

$logPanel = New-Object System.Windows.Forms.Panel
$logPanel.Size = SZ 428 132
$logPanel.BackColor = $cLogBg
$form.Controls.Add($logPanel)

$logBox = New-Object System.Windows.Forms.TextBox
$logBox.Multiline = $true
$logBox.ReadOnly = $true
$logBox.ScrollBars = "None"
$logBox.WordWrap = $true
$logBox.BorderStyle = "None"
$logBox.BackColor = $cLogBg
$logBox.ForeColor = $cMuted
$logBox.Font = New-Object System.Drawing.Font("Consolas", 8.5)
$logBox.Location = P 10 8
$logPanel.Controls.Add($logBox)

$sbTrack = New-Object System.Windows.Forms.Panel
$sbTrack.BackColor = $cLogBg
$sbTrack.Width = S 10
$logPanel.Controls.Add($sbTrack)

$sbThumb = New-Object System.Windows.Forms.Panel
$sbThumb.BackColor = $cBtn
$sbThumb.Width = S 6
$sbThumb.Left = S 2
$sbTrack.Controls.Add($sbThumb)

$EM_GETLINECOUNT = 0xBA
$EM_GETFIRSTVISIBLELINE = 0xCE
$EM_LINESCROLL = 0xB6

function Get-LineHeight {
    [int]$logBox.Font.Height
}

function Update-LogScrollbar {
    if (-not $logBox.IsHandleCreated) { return }
    $lh = Get-LineHeight
    if ($lh -le 0) { return }
    $total = [Win32.Native]::SendMessage($logBox.Handle, $EM_GETLINECOUNT, 0, 0)
    $visible = [int]([Math]::Floor($logBox.ClientSize.Height / $lh))
    if ($visible -lt 1) { $visible = 1 }
    if ($total -le $visible) {
        $sbThumb.Visible = $false
        return
    }
    $sbThumb.Visible = $true
    $first = [Win32.Native]::SendMessage($logBox.Handle, $EM_GETFIRSTVISIBLELINE, 0, 0)
    $trackH = $sbTrack.Height
    $thumbH = [int][Math]::Max((S 24), $trackH * $visible / $total)
    $maxFirst = $total - $visible
    $y = if ($maxFirst -gt 0) { [int](($trackH - $thumbH) * $first / $maxFirst) } else { 0 }
    $sbThumb.Height = $thumbH
    $sbThumb.Top = [Math]::Min([Math]::Max(0, $y), $trackH - $thumbH)
}

function Layout-LogPanel {
    $sbTrack.Left = $logPanel.ClientSize.Width - $sbTrack.Width - (S 2)
    $sbTrack.Top = S 6
    $sbTrack.Height = $logPanel.ClientSize.Height - (S 12)
    $logBox.Width = $sbTrack.Left - (S 10)
    $logBox.Height = $logPanel.ClientSize.Height - (S 16)
    Update-LogScrollbar
}
$logPanel.Add_Resize({ Layout-LogPanel })

$logBox.Add_MouseWheel({
    $lines = -([Math]::Sign($_.Delta)) * 3
    [void][Win32.Native]::SendMessage($logBox.Handle, $EM_LINESCROLL, 0, $lines)
    Update-LogScrollbar
})

$script:sbDrag = $false
$script:sbDragY = 0
$sbThumb.Add_MouseEnter({ $sbThumb.BackColor = $cBtnHover })
$sbThumb.Add_MouseLeave({ if (-not $script:sbDrag) { $sbThumb.BackColor = $cBtn } })
$sbThumb.Add_MouseDown({ $script:sbDrag = $true; $script:sbDragY = $_.Y })
$sbThumb.Add_MouseUp({ $script:sbDrag = $false; $sbThumb.BackColor = $cBtn })
$sbThumb.Add_MouseMove({
    if (-not $script:sbDrag) { return }
    $lh = Get-LineHeight
    $total = [Win32.Native]::SendMessage($logBox.Handle, $EM_GETLINECOUNT, 0, 0)
    $visible = [int]([Math]::Floor($logBox.ClientSize.Height / $lh))
    $maxFirst = $total - $visible
    if ($maxFirst -le 0) { return }
    $newTop = $sbThumb.Top + ($_.Y - $script:sbDragY)
    $newTop = [Math]::Min([Math]::Max(0, $newTop), $sbTrack.Height - $sbThumb.Height)
    $targetFirst = [int]([Math]::Round($maxFirst * $newTop / ($sbTrack.Height - $sbThumb.Height)))
    $cur = [Win32.Native]::SendMessage($logBox.Handle, $EM_GETFIRSTVISIBLELINE, 0, 0)
    [void][Win32.Native]::SendMessage($logBox.Handle, $EM_LINESCROLL, 0, ($targetFirst - $cur))
    Update-LogScrollbar
})

$script:exitDots = @{}
$script:exitLabels = @{}

function Build-ExitCards {
    $cardsPanel.Controls.Clear()
    $script:exitDots = @{}
    $script:exitLabels = @{}
    $y = 0
    foreach ($e in $script:cfg.forwards) {
        $card = New-Object System.Windows.Forms.Panel
        $card.Location = New-Object System.Drawing.Point(0, (S $y))
        $card.Size = SZ 428 58
        $card.BackColor = $cCard
        $cardsPanel.Controls.Add($card)

        $dot = New-Object System.Windows.Forms.Label
        $dot.Location = P 14 17
        $dot.Size = SZ 22 24
        $dot.Font = New-Object System.Drawing.Font("Segoe UI", 11)
        $dot.Text = [char]0x25CF
        $dot.ForeColor = $cMuted
        $card.Controls.Add($dot)
        $script:exitDots[[int]$e.local] = $dot

        $name = New-Object System.Windows.Forms.Label
        $name.Location = P 42 10
        $name.Size = SZ 150 20
        $name.Font = New-Object System.Drawing.Font("Segoe UI Semibold", 10)
        $name.ForeColor = $cText
        $name.Text = $e.name
        $card.Controls.Add($name)

        $port = New-Object System.Windows.Forms.Label
        $port.Location = P 42 32
        $port.Size = SZ 180 16
        $port.Font = New-Object System.Drawing.Font("Consolas", 8.5)
        $port.ForeColor = $cMuted
        $port.Text = "socks5://127.0.0.1:$($e.local)"
        $card.Controls.Add($port)

        $res = New-Object System.Windows.Forms.Label
        $res.Location = P 198 19
        $res.Size = SZ 216 20
        $res.TextAlign = "MiddleRight"
        $res.ForeColor = $cMuted
        $res.Text = "-"
        $card.Controls.Add($res)
        $script:exitLabels[[int]$e.local] = $res

        $y += 68
    }
    $cardsPanel.Height = S ([Math]::Max(10, $y - 10))
    $logY = 138 + [Math]::Max(10, $y - 10) + 14
    $sectionLog.Location = P 20 $logY
    $logPanel.Location = P 18 ($logY + 22)
    $form.ClientSize = SZ 464 ($logY + 22 + 132 + 16)
    $statusSub.Text = "ssh {0}@{1}" -f $script:cfg.user, $script:cfg.host
}

$notify = New-Object System.Windows.Forms.NotifyIcon
$notify.Icon = $appIcon
$notify.Text = "Traytunnel"
$notify.Visible = $true

$trayMenu = New-Object System.Windows.Forms.ContextMenuStrip
$trayMenu.BackColor = $cCard
$trayMenu.ForeColor = $cText
$menuShow = $trayMenu.Items.Add("Open window")
$menuExit = $trayMenu.Items.Add("Exit")
$notify.ContextMenuStrip = $trayMenu
$menuShow.Add_Click({ Show-MainWindow })
$menuExit.Add_Click({ $form.Close() })

function Write-Log([string]$msg) {
    $line = "{0}  {1}" -f (Get-Date -Format "HH:mm:ss"), $msg
    $logBox.AppendText($line + [Environment]::NewLine)
    $logBox.SelectionStart = $logBox.TextLength
    $logBox.ScrollToCaret()
    Update-LogScrollbar
}

function Set-Status([string]$text, [System.Drawing.Color]$color) {
    $statusText.Text = $text
    $statusDot.ForeColor = $color
    $notify.Text = "Traytunnel - " + $text
}

function Show-MainWindow {
    $form.Show()
    $form.ShowInTaskbar = $true
    $form.WindowState = "Normal"
    $form.Activate()
    $form.BringToFront()
}

function Hide-ToTray {
    $form.Hide()
    if (-not $script:trayHintShown) {
        $notify.BalloonTipTitle = "Traytunnel"
        $notify.BalloonTipText = "Closed to tray, still running. Double-click the tray icon to reopen."
        $notify.ShowBalloonTip(3000)
        $script:trayHintShown = $true
    }
}

function Invoke-CloseButton {
    if ($script:cfg.closeToTray) { Hide-ToTray } else { $form.Close() }
}

function Reset-ExitCards {
    foreach ($p in @($script:exitDots.Keys)) {
        $script:exitDots[$p].ForeColor = $cMuted
        $script:exitLabels[$p].Text = "-"
        $script:exitLabels[$p].ForeColor = $cMuted
    }
}

function Start-Tunnel {
    $script:connected = $false
    $script:proc = Start-Process ssh -ArgumentList (Get-SshArgs) -WindowStyle Hidden -PassThru
    Set-Status "Connecting..." $cAmber
    Write-Log "tunnel starting (pid $($script:proc.Id))"
}

function Stop-Tunnel {
    if ($script:proc -and -not $script:proc.HasExited) {
        Stop-Process -Id $script:proc.Id -Force -ErrorAction SilentlyContinue
        Write-Log "tunnel stopped"
    }
    $script:proc = $null
    $script:connected = $false
}

function Start-ExitTests {
    foreach ($e in $script:cfg.forwards) {
        $port = [int]$e.local
        if ($script:jobs.ContainsKey($port)) { continue }
        $script:exitLabels[$port].Text = "testing..."
        $script:exitLabels[$port].ForeColor = $cMuted
        $script:exitDots[$port].ForeColor = $cAmber
        $script:jobs[$port] = Start-Job -ScriptBlock {
            param($p)
            $r = & curl.exe -s -m 12 --socks5-hostname "127.0.0.1:$p" https://ipinfo.io/json 2>$null
            if ($r) {
                try {
                    $j = $r | ConvertFrom-Json
                    "OK|{0}  {1}, {2}" -f $j.ip, $j.city, $j.country
                }
                catch { "FAIL|bad response" }
            }
            else { "FAIL|no response" }
        } -ArgumentList $port
    }
    Write-Log "testing exits..."
}

function Show-SettingsDialog {
    $dlg = New-Object System.Windows.Forms.Form
    $dlg.Text = "Settings"
    $dlg.FormBorderStyle = "None"
    $dlg.StartPosition = "CenterParent"
    $dlg.BackColor = $cBg
    $dlg.ForeColor = $cText
    $dlg.Font = New-Object System.Drawing.Font("Segoe UI", 9)
    $dlg.Icon = $appIcon
    $dlg.ShowInTaskbar = $false
    $dlg.ClientSize = SZ 436 514
    [void](Add-TitleBar $dlg "Settings" $false { $this.FindForm().Close() })
    Set-RoundCorners $dlg

    function New-Section([string]$text, [float]$y) {
        $s = New-Object System.Windows.Forms.Label
        $s.Location = P 20 $y
        $s.Size = SZ 300 16
        $s.ForeColor = $cMuted
        $s.Font = New-Object System.Drawing.Font("Segoe UI", 8.5)
        $s.Text = $text
        $dlg.Controls.Add($s)
    }

    function New-Border($wrap) {
        $wrap.Add_Paint({
            $on = [bool]$this.Tag
            $col = if ($on) { $cAccent } else { $cBtn }
            $pen = New-Object System.Drawing.Pen($col, 1)
            $_.Graphics.DrawRectangle($pen, 0, 0, $this.Width - 1, $this.Height - 1)
            $pen.Dispose()
        })
    }

    function Add-Field([string]$label, [float]$y, [string]$value) {
        $lb = New-Object System.Windows.Forms.Label
        $lb.Location = P 20 ($y + 6)
        $lb.Size = SZ 96 20
        $lb.ForeColor = $cMuted
        $lb.Text = $label
        $dlg.Controls.Add($lb)

        $wrap = New-Object System.Windows.Forms.Panel
        $wrap.Location = P 120 $y
        $wrap.Size = SZ 296 30
        $wrap.BackColor = $cCard
        $dlg.Controls.Add($wrap)
        New-Border $wrap

        $tb = New-Object System.Windows.Forms.TextBox
        $tb.BorderStyle = "None"
        $tb.BackColor = $cCard
        $tb.ForeColor = $cText
        $tb.Text = $value
        $tb.Width = (S 296) - (S 16)
        $tb.Left = S 8
        $tb.Top = [int](((S 30) - $tb.PreferredHeight) / 2)
        $wrap.Controls.Add($tb)
        $tb.Add_Enter({ $this.Parent.Tag = $true; $this.Parent.Invalidate() })
        $tb.Add_Leave({ $this.Parent.Tag = $false; $this.Parent.Invalidate() })
        return $tb
    }

    New-Section "CONNECTION" 50
    $tbHost = Add-Field "Host" 72 $script:cfg.host
    $tbUser = Add-Field "User" 108 $script:cfg.user
    $tbProxy = Add-Field "ProxyCommand" 144 $script:cfg.proxyCommand

    New-Section "FORWARDS" 190
    $hint = New-Object System.Windows.Forms.Label
    $hint.Location = P 20 208
    $hint.Size = SZ 396 16
    $hint.ForeColor = $cMuted
    $hint.Font = New-Object System.Drawing.Font("Consolas", 8)
    $hint.Text = "name   localPort   remoteHost:remotePort"
    $dlg.Controls.Add($hint)

    $fwdWrap = New-Object System.Windows.Forms.Panel
    $fwdWrap.Location = P 20 228
    $fwdWrap.Size = SZ 396 128
    $fwdWrap.BackColor = $cCard
    $dlg.Controls.Add($fwdWrap)
    New-Border $fwdWrap

    $tbFwd = New-Object System.Windows.Forms.TextBox
    $tbFwd.Multiline = $true
    $tbFwd.ScrollBars = "None"
    $tbFwd.WordWrap = $false
    $tbFwd.BorderStyle = "None"
    $tbFwd.BackColor = $cCard
    $tbFwd.ForeColor = $cText
    $tbFwd.Font = New-Object System.Drawing.Font("Consolas", 9.5)
    $tbFwd.Location = P 8 6
    $tbFwd.Size = New-Object System.Drawing.Size(((S 396) - (S 24)), ((S 128) - (S 12)))
    $tbFwd.Text = (($script:cfg.forwards | ForEach-Object { "{0}  {1}  {2}" -f $_.name, $_.local, $_.remote }) -join [Environment]::NewLine)
    $fwdWrap.Controls.Add($tbFwd)

    $fwdTrack = New-Object System.Windows.Forms.Panel
    $fwdTrack.BackColor = $cCard
    $fwdTrack.Width = S 10
    $fwdTrack.Left = (S 396) - (S 12)
    $fwdTrack.Top = S 4
    $fwdTrack.Height = (S 128) - (S 8)
    $fwdWrap.Controls.Add($fwdTrack)

    $fwdThumb = New-Object System.Windows.Forms.Panel
    $fwdThumb.BackColor = $cBtn
    $fwdThumb.Width = S 6
    $fwdThumb.Left = S 2
    $fwdTrack.Controls.Add($fwdThumb)

    function Update-FwdScroll {
        if (-not $tbFwd.IsHandleCreated) { return }
        $lh = [int]$tbFwd.Font.Height
        if ($lh -le 0) { return }
        $total = [Win32.Native]::SendMessage($tbFwd.Handle, 0xBA, 0, 0)
        $visible = [int]([Math]::Floor($tbFwd.ClientSize.Height / $lh))
        if ($visible -lt 1) { $visible = 1 }
        if ($total -le $visible) { $fwdThumb.Visible = $false; return }
        $fwdThumb.Visible = $true
        $first = [Win32.Native]::SendMessage($tbFwd.Handle, 0xCE, 0, 0)
        $trackH = $fwdTrack.Height
        $thumbH = [int][Math]::Max((S 24), $trackH * $visible / $total)
        $maxFirst = $total - $visible
        $y = if ($maxFirst -gt 0) { [int](($trackH - $thumbH) * $first / $maxFirst) } else { 0 }
        $fwdThumb.Height = $thumbH
        $fwdThumb.Top = [Math]::Min([Math]::Max(0, $y), $trackH - $thumbH)
    }
    $tbFwd.Add_TextChanged({ Update-FwdScroll })
    $tbFwd.Add_MouseWheel({
        [void][Win32.Native]::SendMessage($tbFwd.Handle, 0xB6, 0, (-([Math]::Sign($_.Delta)) * 3))
        Update-FwdScroll
    })
    $tbFwd.Add_KeyUp({ Update-FwdScroll })
    $script:fwdDrag = $false
    $script:fwdDragY = 0
    $fwdThumb.Add_MouseEnter({ $fwdThumb.BackColor = $cBtnHover })
    $fwdThumb.Add_MouseLeave({ if (-not $script:fwdDrag) { $fwdThumb.BackColor = $cBtn } })
    $fwdThumb.Add_MouseDown({ $script:fwdDrag = $true; $script:fwdDragY = $_.Y })
    $fwdThumb.Add_MouseUp({ $script:fwdDrag = $false; $fwdThumb.BackColor = $cBtn })
    $fwdThumb.Add_MouseMove({
        if (-not $script:fwdDrag) { return }
        $lh = [int]$tbFwd.Font.Height
        $total = [Win32.Native]::SendMessage($tbFwd.Handle, 0xBA, 0, 0)
        $visible = [int]([Math]::Floor($tbFwd.ClientSize.Height / $lh))
        $maxFirst = $total - $visible
        if ($maxFirst -le 0) { return }
        $newTop = $fwdThumb.Top + ($_.Y - $script:fwdDragY)
        $newTop = [Math]::Min([Math]::Max(0, $newTop), $fwdTrack.Height - $fwdThumb.Height)
        $targetFirst = [int]([Math]::Round($maxFirst * $newTop / ($fwdTrack.Height - $fwdThumb.Height)))
        $cur = [Win32.Native]::SendMessage($tbFwd.Handle, 0xCE, 0, 0)
        [void][Win32.Native]::SendMessage($tbFwd.Handle, 0xB6, 0, ($targetFirst - $cur))
        Update-FwdScroll
    })

    function New-Toggle([float]$x, [float]$y, [bool]$initial, [string]$labelText) {
        $tg = New-Object System.Windows.Forms.Panel
        $tg.Location = P $x $y
        $tg.Size = SZ 44 24
        $tg.BackColor = $cBg
        $tg.Cursor = "Hand"
        $tg.Tag = $initial
        $dlg.Controls.Add($tg)
        $tg.Add_Paint({
            $gr = $_.Graphics
            $gr.SmoothingMode = "AntiAlias"
            $on = [bool]$this.Tag
            $w = $this.Width; $h = $this.Height
            $bg = if ($on) { $cAccent } else { $cBtn }
            $path = New-Object System.Drawing.Drawing2D.GraphicsPath
            $path.AddArc(0, 0, $h, $h, 90, 180)
            $path.AddArc($w - $h, 0, $h, $h, 270, 180)
            $path.CloseFigure()
            $br = New-Object System.Drawing.SolidBrush($bg)
            $gr.FillPath($br, $path)
            $m = S 3
            $kd = $h - 2 * $m
            $kx = if ($on) { $w - $kd - $m } else { $m }
            $gr.FillEllipse([System.Drawing.Brushes]::White, $kx, $m, $kd, $kd)
            $br.Dispose(); $path.Dispose()
        })
        $lb = New-Object System.Windows.Forms.Label
        $lb.Location = P ($x + 54) ($y + 4)
        $lb.Size = SZ 320 20
        $lb.ForeColor = $cText
        $lb.Text = $labelText
        $dlg.Controls.Add($lb)
        return $tg
    }

    New-Section "BEHAVIOR" 372

    $tgAuto = New-Toggle 20 392 (Test-Autostart) "Start on Windows login"
    $tgAuto.Add_Click({
        $new = -not [bool]$this.Tag
        try {
            Set-Autostart $new
            $this.Tag = $new
            $this.Invalidate()
            Write-Log $(if ($new) { "autostart enabled" } else { "autostart disabled" })
        }
        catch {
            [System.Windows.Forms.MessageBox]::Show("Failed to change autostart:`n$($_.Exception.Message)", "Traytunnel") | Out-Null
        }
    })

    $tgClose = New-Toggle 20 424 ([bool]$script:cfg.closeToTray) "Close button (X) hides to tray"
    $tgClose.Add_Click({
        $new = -not [bool]$this.Tag
        $this.Tag = $new
        $this.Invalidate()
        $script:cfg.closeToTray = $new
        $script:cfg | ConvertTo-Json -Depth 5 | Set-Content $script:configPath -Encoding UTF8
        Write-Log $(if ($new) { "close hides to tray" } else { "close exits app" })
    })

    $saveBtn = New-FlatButton "Save" 258 466 76 34
    $saveBtn.BackColor = $cAccent
    $saveBtn.ForeColor = $cBg
    $saveBtn.FlatAppearance.MouseOverBackColor = [System.Drawing.Color]::FromArgb(255, 74, 222, 187)
    $saveBtn.FlatAppearance.MouseDownBackColor = $cAccent
    $saveBtn.Font = New-Object System.Drawing.Font("Segoe UI Semibold", 9)
    $dlg.Controls.Add($saveBtn)
    $cancelBtn = New-FlatButton "Cancel" 342 466 74 34
    $dlg.Controls.Add($cancelBtn)
    $cancelBtn.Add_Click({ $this.FindForm().Close() })

    $dlg.Add_Shown({ Update-FwdScroll })

    $saveBtn.Add_Click({
        $forwards = @()
        $bad = $null
        foreach ($line in ($tbFwd.Text -split "`r?`n")) {
            $line = $line.Trim()
            if (-not $line) { continue }
            $parts = $line -split "\s+"
            if ($parts.Count -ne 3 -or $parts[1] -notmatch '^\d+$' -or $parts[2] -notmatch '^[^:\s]+:\d+$') {
                $bad = $line
                break
            }
            $forwards += [pscustomobject]@{ name = $parts[0]; local = [int]$parts[1]; remote = $parts[2] }
        }
        if ($bad) {
            [System.Windows.Forms.MessageBox]::Show("Invalid forward line:`n$bad`n`nExpected:  name  localPort  remoteHost:remotePort", "Traytunnel") | Out-Null
            return
        }
        if (-not $tbHost.Text.Trim() -or -not $tbUser.Text.Trim() -or $forwards.Count -eq 0) {
            [System.Windows.Forms.MessageBox]::Show("Host, User and at least one forward are required.", "Traytunnel") | Out-Null
            return
        }
        $script:cfg = [pscustomobject]@{
            host = $tbHost.Text.Trim()
            user = $tbUser.Text.Trim()
            proxyCommand = $tbProxy.Text.Trim()
            closeToTray = [bool]$script:cfg.closeToTray
            forwards = $forwards
        }
        $script:cfg | ConvertTo-Json -Depth 5 | Set-Content $script:configPath -Encoding UTF8
        $this.FindForm().Tag = "saved"
        $this.FindForm().Close()
    })

    [void]$dlg.ShowDialog($form)
    if ($dlg.Tag -eq "saved") {
        Write-Log "config saved, restarting tunnel"
        Build-ExitCards
        Stop-Tunnel
        if ($script:wantRun) { $script:retryAt = Get-Date }
    }
    $dlg.Dispose()
}

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 2000
$timer.Add_Tick({
    foreach ($port in @($script:jobs.Keys)) {
        $job = $script:jobs[$port]
        if ($job.State -in "Completed", "Failed") {
            $out = (Receive-Job $job -ErrorAction SilentlyContinue | Select-Object -Last 1)
            Remove-Job $job -Force
            $script:jobs.Remove($port)
            if (-not $script:exitLabels.ContainsKey($port)) { continue }
            $lb = $script:exitLabels[$port]
            $dt = $script:exitDots[$port]
            if ($out -and $out.StartsWith("OK|")) {
                $lb.Text = $out.Substring(3)
                $lb.ForeColor = $cText
                $dt.ForeColor = $cAccent
                Write-Log ("port {0} : {1}" -f $port, $out.Substring(3))
            }
            else {
                $lb.Text = "no response"
                $lb.ForeColor = $cRed
                $dt.ForeColor = $cRed
                Write-Log ("port {0} : no response" -f $port)
            }
        }
    }
    if (-not $script:wantRun) { return }
    if (-not $script:proc -or $script:proc.HasExited) {
        if ($script:connected -or $script:proc) {
            Write-Log "disconnected, retrying in 5s"
            $script:proc = $null
            $script:connected = $false
            $script:retryAt = (Get-Date).AddSeconds(5)
            Set-Status "Reconnecting..." $cAmber
            Reset-ExitCards
        }
        if ((Get-Date) -ge $script:retryAt) { Start-Tunnel }
        return
    }
    if (-not $script:connected) {
        $firstPort = [int]$script:cfg.forwards[0].local
        $listen = Get-NetTCPConnection -LocalPort $firstPort -State Listen -ErrorAction SilentlyContinue
        if ($listen) {
            $script:connected = $true
            Set-Status "Connected" $cAccent
            Write-Log "tunnel up"
            Start-ExitTests
        }
    }
})

$toggleBtn.Add_Click({
    if ($script:wantRun) {
        $script:wantRun = $false
        Stop-Tunnel
        Set-Status "Stopped" $cMuted
        $toggleBtn.Text = $glyphStart
        $toggleBtn.ForeColor = $cAccent
        $tips.SetToolTip($toggleBtn, "Start")
        Reset-ExitCards
    }
    else {
        $script:wantRun = $true
        $script:retryAt = Get-Date
        $toggleBtn.Text = $glyphStop
        $toggleBtn.ForeColor = $cRed
        $tips.SetToolTip($toggleBtn, "Stop")
    }
})

$retestBtn.Add_Click({
    if ($script:connected) { Start-ExitTests } else { Write-Log "not connected, cannot test" }
})

$settingsBtn.Add_Click({ Show-SettingsDialog })

$notify.Add_MouseDoubleClick({ Show-MainWindow })

$form.Add_FormClosing({
    $timer.Stop()
    $showTimer.Stop()
    $script:wantRun = $false
    Stop-Tunnel
    foreach ($job in $script:jobs.Values) { Remove-Job $job -Force -ErrorAction SilentlyContinue }
    $notify.Visible = $false
    $notify.Dispose()
    try { $script:appMutex.ReleaseMutex(); $script:appMutex.Dispose() } catch {}
    try { $script:showEvent.Dispose() } catch {}
})

$form.Add_Shown({
    Layout-LogPanel
    if ($Tray) {
        $form.Hide()
        $script:trayHintShown = $true
        $notify.BalloonTipTitle = "Traytunnel"
        $notify.BalloonTipText = "Started in the system tray. Double-click the tray icon to open."
        $notify.ShowBalloonTip(3000)
    }
})

$showTimer = New-Object System.Windows.Forms.Timer
$showTimer.Interval = 300
$showTimer.Add_Tick({
    if ($script:showEvent.WaitOne(0)) {
        Show-MainWindow
    }
})
$showTimer.Start()

Build-ExitCards
$timer.Start()
Write-Log "Traytunnel started"
if ($Tray) {
    $form.WindowState = "Minimized"
    $form.ShowInTaskbar = $false
}
[void]$form.ShowDialog()
