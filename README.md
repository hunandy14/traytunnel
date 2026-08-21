# traytunnel

Windows 系統匣（tray）SSH 隧道管理工具。以 PowerShell 撰寫，讀取 `traytunnel.json` 設定檔，自動維持 SSH 連線並在斷線時重連，同時對每個轉發出口做連通自我檢測，狀態即時顯示在托盤圖示與視窗中。

## 功能

- 系統匣常駐，開機自動啟動（可選）
- 依設定檔啟動 `ssh.exe` 建立多組本地埠轉發（Local Forward）
- 斷線自動重試
- 各出口連通性自我檢測
- 支援透過 `ProxyCommand`（例如 `cloudflared access ssh`）連線

## 需求

- Windows + PowerShell
- `ssh.exe`（OpenSSH 用戶端）
- 選配：`cloudflared`，若你的 SSH 主機需要透過 Cloudflare Access 存取

## 快速開始

1. 複製設定檔範本並改成自己的設定：

   ```
   copy traytunnel.json.example traytunnel.json
   ```

   編輯 `traytunnel.json`，填入你的主機、使用者與轉發設定。

2. 執行 `traytunnel.bat` 啟動程式。

`traytunnel.json` 為個人本機設定，已加入 `.gitignore`，不會被提交。
