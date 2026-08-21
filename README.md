# traytunnel

Windows 系統匣（tray）SSH 隧道管理工具，以 [Tauri v2](https://tauri.app/) 撰寫，前端是 vanilla TypeScript + Vite，隧道管理、設定檔讀寫與出口檢測全部在 Rust 側完成。

程式讀取 `traytunnel.toml`，**每個出口各自維持一條獨立的 SSH 連線**並在斷線時各自重連，同時對每個轉發出口做連通自我檢測，狀態即時顯示在系統匣圖示與主視窗中。

## 功能

- 系統匣常駐，可設定開機自動啟動（啟動時帶 `--tray` 直接隱藏到系統匣）
- 每個 `[[forwards]]` 出口一條獨立的 `ssh.exe -N -L`，可個別連接／中斷，一個出口斷線或重連不會影響其他出口
- 出口的連接／中斷選擇會寫回設定檔的 `enabled`，下次啟動只自動連線 `enabled` 的出口
- 斷線後固定 5 秒重連，無退避、無次數上限，每個出口自己數自己的
- 本地埠衝突三層防護：設定階段擋重複埠、spawn 前偵測埠是否已被其他程序佔用（狀態顯示 `port_busy`，每 5 秒重查而不盲目 spawn），最後由 ssh 的 `ExitOnForwardFailure=yes` 兜底
- 各出口經本地 SOCKS5 埠檢測連通性，顯示對外 IP 與所在地
- 支援透過 `ProxyCommand`（例如 `cloudflared access ssh`）連線
- 單一實例：重複啟動只會把既有的主視窗叫出來
- 系統匣提示彙總所有出口狀態，例如 `Traytunnel - 2/2 connected`
- 每條 ssh 子程序各自放在一個 Windows Job Object 內，出口停掉或程式結束時整棵程序樹（含 `cloudflared`）一起收掉

## 介面

視窗分成左側的源軌道與右側的主區，兩者都可隨視窗縮放（最小 480×420）：

- **左側源軌道**：每個源一個圓角方塊，圖案是源名首字，底色由名稱 hash 決定，右下角的小點是該源的彙總狀態（全連綠／部分琥珀／全停灰／有出口出錯紅）。清單底部的虛線「＋」新增源；左下角固定放活動日誌與設定兩個鈕。
- **主區（選中的源）**：頂部彙總列顯示 `n/m Connected` 與 `ssh user@host`，右側依序是新增出口、啟停此源、重測此源、編輯此源。下方是出口卡清單，每張卡右側是連接／中斷、重新連接、編輯三個鈕，編輯就地展開；底部是縮小的即時日誌窗，只顯示屬於這個源的行。
- **活動日誌頁**：左下的時鐘鈕把主區換成所有源的完整日誌，點任一源 icon 即返回。
- **設定頁**：左下的齒輪把主區換成設定頁，目前有「關閉時縮到系統匣」與「開機自動啟動」兩個即時生效的開關。
- **源編輯**：頂部的鉛筆或側欄的「＋」會開出源的編輯面板（name／host／user／ProxyCommand），驗證錯誤逐欄顯示；刪除源需要一次確認，刪除出口則是先從畫面移除、5 秒內可以按 Undo 收回。

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

然後用瀏覽器打開 http://localhost:1420/ 。整個 UI 只有這一頁，主區靠左側欄切換。

這個模式下沒有 Tauri runtime，前端會自動掛上一層假後端：

- 用官方的 `@tauri-apps/api/mocks` 的 `mockIPC`（開啟 `shouldMockEvents`）攔截所有 `invoke`，並讓 `listen`／`emit` 走記憶體，所以前端程式碼完全不用為了 mock 改寫
- 偵測方式是 Tauri v2 官方提供的 `isTauri()`，偵測不到才啟用
- 假資料有三個源（`tokyo` 兩個出口、`taipei` 兩個出口、`lab` 零出口示範空狀態），涵蓋每個出口獨立的 `connecting → connected`、自測 `testing → ok`／`fail`、固定會撞埠的 `port_busy`，以及跨源的本地埠衝突；出口的連接／中斷／重新連接、源的啟停與重測、出口就地編輯與刪除（含 undo）、源的新增／編輯／刪除都能實際操作
- 設定存檔只寫進 `sessionStorage`，不會碰到真的設定檔
- 另外掛了 `window.__mock` 供演練特定狀態：`__mock.drop(1080)` 模擬斷線重連、`__mock.status(1080, "error", "…")` 直接指定狀態、`__mock.wipe()` 清掉所有源看零源空狀態、`__mock.reset()` 清掉暫存重來

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

欄位：

| 欄位 | 說明 |
| --- | --- |
| `host` | SSH 主機 |
| `user` | SSH 使用者 |
| `proxyCommand` | ssh 的 `ProxyCommand`，不需要時留空字串 |
| `closeToTray` | 關閉鈕（X）是否只隱藏到系統匣 |
| `[[forwards]]` | 一組本地埠轉發，含 `name`、`local`、`remote`、`enabled` |

`[[forwards]]` 的欄位：

| 欄位 | 說明 |
| --- | --- |
| `name` | 出口名稱，不可空白也不可含空格 |
| `local` | 本地埠，同時是這個出口的唯一鍵，不可與其他出口重複（含停用中的） |
| `remote` | 轉發目的地，格式 `host:port` |
| `enabled` | 是否要保持連線；省略時視為 `true`，舊設定檔不用改也能直接用 |

每個出口各自跑一條 `ssh`，並以自己的 `local` 埠是否進入 Listen 狀態判斷該出口是否連上。在介面上按連接／中斷會即時寫回對應的 `enabled`。

也可以在程式裡編輯：齒輪會在主視窗內開啟全域設定的覆蓋層（Host／User／ProxyCommand 與兩個即時生效的開關），轉發則是在出口卡片上就地展開編輯，清單最後的虛線卡片可以新增。存檔會寫回同一個檔案並保留你手寫的註解（包含寫在單筆 `[[forwards]]` 上方的註解）。

其他行為：

- 設定檔解析失敗時**不會被覆寫**，程式會另存一份 `traytunnel.toml.broken` 並改用預設值繼續執行
- 用 PowerShell 之類的工具存檔若帶了 UTF-8 BOM，也能正常解析

`traytunnel.toml` 為個人本機設定，已加入 `.gitignore`，不會被提交。
