# traytunnel

Windows 系統匣（tray）SSH 隧道管理工具，以 [Tauri v2](https://tauri.app/) 撰寫，前端是 vanilla TypeScript + Vite，隧道管理、設定檔讀寫與出口檢測全部在 Rust 側完成。

程式讀取 `traytunnel.toml`，支援**多個連線源**（各自的 host／user／ProxyCommand），**每個出口各自維持一條獨立的 SSH 連線**並在斷線時各自重連，同時對每個轉發出口做連通自我檢測，狀態即時顯示在系統匣圖示與主視窗中。

## 功能

- 系統匣常駐，可設定開機自動啟動（啟動時帶 `--tray` 直接隱藏到系統匣）
- 多連線源：每個 `[[sources]]` 是一組獨立的 ssh 連線參數，底下各自掛自己的出口，可整源連接／中斷／重測
- 每個 `[[sources.forwards]]` 出口一條獨立的 `ssh.exe -N -L`，可個別連接／中斷／重接，一個出口斷線或重連不會影響其他出口
- 出口的連接／中斷選擇會寫回設定檔的 `enabled`，下次啟動只自動連線 `enabled` 的出口（所有源一起）
- 斷線後固定 5 秒重連，無退避、無次數上限，每個出口自己數自己的
- 本地埠衝突三層防護：設定階段擋重複埠（跨源也擋，訊息會點名佔用者與它所屬的源）、spawn 前偵測埠是否已被其他程序佔用（狀態顯示 `port_busy`，每 5 秒重查而不盲目 spawn），最後由 ssh 的 `ExitOnForwardFailure=yes` 兜底
- 各出口經本地 SOCKS5 埠檢測連通性，顯示對外 IP 與所在地
- 支援透過 `ProxyCommand`（例如 `cloudflared access ssh`）連線
- 單一實例：重複啟動只會把既有的主視窗叫出來
- 系統匣提示跨源彙總所有出口狀態，例如 `Traytunnel - 3/4 connected`
- 系統匣圖示依 `SM_CXSMICON` 從多層 ICO 挑原生尺寸的那一層（含 16／20／24／28／32px），高 DPI 下不會被 GDI 拉伸糊掉
- 通知掛在自己的 AppUserModelID 底下：啟動時自註冊開始選單捷徑與 `HKCU\Software\Classes\AppUserModelId`，toast 顯示的是 Traytunnel 而不是 Windows PowerShell
- 每條 ssh 子程序各自放在一個 Windows Job Object 內，出口停掉或程式結束時整棵程序樹（含 `cloudflared`）一起收掉

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

### 瀏覽器 UI 開發模式

只想調畫面、不想每次都等 Rust 編譯時，可以只開前端：

```
npm run dev
```

然後用瀏覽器打開 http://localhost:1420/ 。整個 UI 只有這一頁，全域設定是主視窗內的覆蓋層。

這個模式下沒有 Tauri runtime，前端會自動掛上一層假後端：

- 用官方的 `@tauri-apps/api/mocks` 的 `mockIPC`（開啟 `shouldMockEvents`）攔截所有 `invoke`，並讓 `listen`／`emit` 走記憶體，所以前端程式碼完全不用為了 mock 改寫
- 偵測方式是 Tauri v2 官方提供的 `isTauri()`，偵測不到才啟用
- 假資料有三組轉發，涵蓋每個出口獨立的 `connecting → connected`、自測 `testing → ok`／`fail`，以及固定會撞埠的 `port_busy`；單一出口的連接／中斷、全停／全啟、重測、就地編輯、新增與刪除（含 undo）都能實際操作
- 設定存檔只寫進 `sessionStorage`，不會碰到真的 `traytunnel.toml`
- 另外掛了 `window.__mock` 供演練特定狀態：`__mock.drop(1080)` 模擬斷線重連、`__mock.status(1080, "error", "…")` 直接指定狀態、`__mock.reset()` 清掉暫存重來

假後端只在 `npm run dev` 且偵測不到 Tauri 時才會動態載入。正式建置時 `import.meta.env.DEV` 是常數 `false`，整段連同 `src/dev-mock.ts` 都會被搖掉，不會進打包產物。

## 建置

```
npm run tauri build
```

產物：

- 免安裝執行檔：`src-tauri/target/release/traytunnel.exe`
- NSIS 安裝檔：`src-tauri/target/release/bundle/nsis/`

注意：一定要走 `npm run tauri build`。直接下 `cargo build --release` 產出的執行檔會去連 Vite 開發伺服器（`devUrl`），而不是內嵌的前端檔案，開起來會是一片空白。`cargo build` 只適合拿來檢查 Rust 端能不能編譯。

免安裝使用時，把 `traytunnel.exe`、`traytunnel.toml` 放在同一個資料夾即可。

## 設定檔

設定檔是 `traytunnel.toml`，位置固定在**執行檔同目錄**。第一次啟動若檔案不存在會自動產生一份預設值，也可以直接複製範本：

```
copy traytunnel.toml.example traytunnel.toml
```

頂層欄位：

| 欄位 | 說明 |
| --- | --- |
| `closeToTray` | 關閉鈕（X）是否只隱藏到系統匣 |
| `[[sources]]` | 一個連線源，含 `name`、`host`、`user`、`proxyCommand` 與底下的 `[[sources.forwards]]` |

`[[sources]]` 的欄位：

| 欄位 | 說明 |
| --- | --- |
| `name` | 連線源名稱，不可空白也不可含空格，且不可與其他源重複 |
| `host` | SSH 主機 |
| `user` | SSH 使用者 |
| `proxyCommand` | ssh 的 `ProxyCommand`，不需要時留空字串 |

`[[sources.forwards]]` 的欄位：

| 欄位 | 說明 |
| --- | --- |
| `name` | 出口名稱，不可空白也不可含空格 |
| `local` | 本地埠，同時是這個出口的**全域**唯一鍵，跨源也不可重複（含停用中的） |
| `remote` | 轉發目的地，格式 `host:port` |
| `enabled` | 是否要保持連線；省略時視為 `true` |

每個出口各自跑一條 `ssh`，連線參數（`host`／`user`／`proxyCommand`）取自它所屬的源，並以自己的 `local` 埠是否進入 Listen 狀態判斷該出口是否連上。在介面上按連接／中斷會即時寫回對應的 `enabled`。

也可以在程式裡編輯，存檔會寫回同一個檔案並保留你手寫的註解（包含寫在單一 `[[sources]]` 或單筆 `[[sources.forwards]]` 上方的註解）。

其他行為：

- **舊制設定檔自動遷移**：偵測到頂層還有 `host` 欄位（單一連線源的舊格式）時，會把它整包成一個 `[[sources]]`（源名預設用 `host` 的值，其中的空白與中括號會被剝掉，例如 `[::1]` 會變成源名 `::1`）、把原本的 `[[forwards]]` 搬成 `[[sources.forwards]]`，並就地寫回新格式；檔頭與逐筆出口上方的註解都會保留
- 升級注意：舊設定檔如果含有**重複的 `local` 埠**（舊版沒擋下來的話），升級時會被判為無法解析，改名成 `traytunnel.toml.broken` 並改用預設值啟動，請手動把重複的埠改掉再放回 `traytunnel.toml`
- 設定檔解析失敗時**不會被覆寫**，程式會另存一份 `traytunnel.toml.broken` 並改用預設值繼續執行；內容自相矛盾（源名重複、跨源撞埠、`host`／`user` 空白）也算解析失敗
- 用 PowerShell 之類的工具存檔若帶了 UTF-8 BOM，也能正常解析

`traytunnel.toml` 為個人本機設定，已加入 `.gitignore`，不會被提交。
