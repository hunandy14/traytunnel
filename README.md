# traytunnel

Windows 系統匣（tray）SSH 隧道管理工具，以 [Tauri v2](https://tauri.app/) 撰寫，前端是 vanilla TypeScript + Vite，隧道管理、設定檔讀寫與出口檢測全部在 Rust 側完成。

程式讀取 `traytunnel.toml`，自動維持 SSH 連線並在斷線時重連，同時對每個轉發出口做連通自我檢測，狀態即時顯示在系統匣圖示與主視窗中。

## 功能

- 系統匣常駐，可設定開機自動啟動（啟動時帶 `--tray` 直接隱藏到系統匣）
- 依設定檔啟動 `ssh.exe` 建立多組本地埠轉發（Local Forward）
- 斷線後固定 5 秒重連，無退避、無次數上限
- 各出口經本地 SOCKS5 埠檢測連通性，顯示對外 IP 與所在地
- 支援透過 `ProxyCommand`（例如 `cloudflared access ssh`）連線
- 單一實例：重複啟動只會把既有的主視窗叫出來
- ssh 子程序放在 Windows Job Object 內，程式結束時整棵程序樹（含 `cloudflared`）一起收掉

## 需求

執行：

- Windows 10/11，內建 WebView2 Runtime（Windows 11 已預裝）
- `ssh.exe`（OpenSSH 用戶端）
- 選配：`cloudflared`，若你的 SSH 主機需要透過 Cloudflare Access 存取

開發與建置另外需要：

- [Rust 工具鏈](https://www.rust-lang.org/tools/install)（含 MSVC Build Tools）
- [Node.js](https://nodejs.org/) 18 以上

## 開發

```
npm install
npm run tauri dev
```

## 建置

```
npm run tauri build
```

產物：

- 免安裝執行檔：`src-tauri/target/release/traytunnel.exe`
- NSIS 安裝檔：`src-tauri/target/release/bundle/nsis/`

免安裝使用時，把 `traytunnel.exe`、`traytunnel.toml` 放在同一個資料夾即可。

## 設定檔

設定檔是 `traytunnel.toml`，位置固定在**執行檔同目錄**。第一次啟動若檔案不存在會自動產生一份預設值，也可以直接複製範本：

```
copy traytunnel.toml.example traytunnel.toml
```

欄位：

| 欄位 | 說明 |
| --- | --- |
| `host` | SSH 主機 |
| `user` | SSH 使用者 |
| `proxyCommand` | ssh 的 `ProxyCommand`，不需要時留空字串 |
| `closeToTray` | 關閉鈕（X）是否只隱藏到系統匣 |
| `[[forwards]]` | 一組本地埠轉發，含 `name`、`local`、`remote` |

第一個 `[[forwards]]` 的 `local` 埠會用來判斷隧道是否連上：該埠進入 Listen 狀態即視為已連線。

也可以在程式裡按齒輪開啟設定介面編輯，存檔會寫回同一個檔案並保留你手寫的註解。

其他行為：

- 舊版的 `traytunnel.json` 若還在，首次啟動會自動轉成 TOML，舊檔改名為 `traytunnel.json.bak`
- 設定檔解析失敗時**不會被覆寫**，程式會另存一份 `traytunnel.toml.broken` 並改用預設值繼續執行

`traytunnel.toml` 為個人本機設定，已加入 `.gitignore`，不會被提交。
