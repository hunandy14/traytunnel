<p align="center"><img src="src-tauri/icons/128x128.png" width="96" alt="traytunnel icon"></p>

<h1 align="center">traytunnel</h1>

<p align="center">Windows／macOS 系統匣 SSH 隧道管理工具</p>
<p align="center"><i>Windows/macOS tray SSH tunnel manager built with Tauri</i></p>

<p align="center">
  <a href="https://github.com/hunandy14/traytunnel/releases"><img src="https://img.shields.io/github/v/release/hunandy14/traytunnel" alt="GitHub release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/hunandy14/traytunnel" alt="License"></a>
</p>

以 [Tauri v2](https://tauri.app/) 撰寫的系統匣 SSH 隧道管理工具：多組連線、每條隧道各自連線與重連，狀態即時顯示在系統匣與主視窗。

## 截圖

<p align="center">
  <img src="docs/screenshots/main-window.png" width="700" alt="主視窗">
</p>

## 功能

- 多組連線、多條隧道，各自獨立 Connect／Disconnect／Reconnect，互不影響
- 系統匣常駐，可設定開機自動啟動
- 斷線自動重連，本地埠衝突偵測
- 連通性自我檢測，顯示對外 IP 與所在地
- 支援 `ProxyCommand`（例如 `cloudflared access ssh`）
- 應用內自動更新：安裝版自動下載安裝，可攜／單檔版提示新版

## 下載與安裝

到 [Releases](https://github.com/hunandy14/traytunnel/releases) 頁面下載，依作業系統挑一種；`SHA256SUMS.txt` 可核對雜湊。

### Windows

需求：Windows 10/11（含 WebView2，Windows 11 已內建）、`ssh.exe`；選配 `cloudflared`。

| 檔名 | 說明 |
| --- | --- |
| `traytunnel-<版本>.exe` | 一般單檔，免安裝 |
| `traytunnel-<版本>p.exe` | 可攜版，設定檔跟著 exe 走 |
| `traytunnel-<版本>-setup.exe` | NSIS 安裝檔 |

程式碼簽章由 [SignPath.io](https://signpath.io/) 與 [SignPath Foundation](https://signpath.org/) 免費提供。

### macOS

macOS 版目前仍屬 **beta**：核心功能與 Windows 版對齊，但實機驗證的時間還沒有 Windows 版久，發佈時會在 Release 說明中標註為 beta。僅支援 Apple Silicon（`arm64`），需要 macOS 12（Monterey）以上；`ssh` 系統內建，選配 `cloudflared`。

| 檔名 | 說明 |
| --- | --- |
| `traytunnel-<版本>-aarch64.dmg` | 安裝映像，拖進「應用程式」資料夾 |
| `traytunnel-<版本>-aarch64.app.tar.gz` | 免安裝 `.app` 壓縮包，同樣要拖進「應用程式」資料夾 |

**先移到「應用程式」資料夾再開啟**（否則自動更新會因 App Translocation 失敗）。我們用 ad-hoc 簽章（沒有 Apple Developer 憑證），第一次開啟需右鍵「打開」，或執行 `xattr -dr com.apple.quarantine /Applications/Traytunnel.app`。其餘已知限制見 [docs/development.md](docs/development.md#macos-已知限制)。

## 使用

一個 `[[sources]]` 是一條**連線**，底下的 `[[sources.forwards]]` 是各自獨立的**隧道**；左側切換連線，右側管理隧道的 Connect／Disconnect／Reconnect。設定檔是 TOML，預設在使用者家目錄（Windows `%USERPROFILE%\.traytunnel.toml`，macOS `~/.traytunnel.toml`），範本見 [`traytunnel.toml.example`](traytunnel.toml.example)。完整介面說明與設定檔欄位見 [docs/development.md](docs/development.md)。

## 開發與建置

```
npm install
npm run dev
```

完整開發環境、建置指令、發佈與版本管理流程見 [docs/development.md](docs/development.md)；平台相依程式碼規則見 [docs/platform-guide.md](docs/platform-guide.md)。

## 授權

MIT License，詳見 [LICENSE](LICENSE)。
