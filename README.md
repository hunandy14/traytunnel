# traytunnel

Windows 系統匣（tray）SSH 隧道管理工具，以 [Tauri v2](https://tauri.app/) 撰寫，前端是 vanilla TypeScript + Vite，隧道管理、設定檔讀寫與出口檢測全部在 Rust 側完成。

程式讀取一份 TOML 設定檔（預設是 `%USERPROFILE%\.traytunnel.toml`），支援**多個連線源**（各自的 host／user／ProxyCommand），**每個出口各自維持一條獨立的 SSH 連線**並在斷線時各自重連，同時對每個轉發出口做連通自我檢測，狀態即時顯示在系統匣圖示與主視窗中。

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

## 介面

視窗分成左側的源軌道與右側的主區，兩者都可隨視窗縮放（最小 480×420）：

- **左側源軌道**：每個源一個圓角方塊，圖案是源名首字，底色由名稱 hash 決定，右下角的小點是該源的彙總狀態（全連綠／部分琥珀／全停灰／有出口出錯紅）。清單底部的虛線「＋」新增源；左下角固定放活動日誌與設定兩個鈕。
- **主區（選中的源）**：頂部彙總列顯示 `n/m Connected` 與 `ssh user@host`，右側依序是新增出口、啟停此源、重測此源、編輯此源。下方是出口卡清單，每張卡右側是連接／中斷、重新連接、編輯三個鈕，編輯就地展開；底部是縮小的即時日誌窗，只顯示屬於這個源的行。
- **活動日誌頁**：左下的時鐘鈕把主區換成所有源的完整日誌，點任一源 icon 即返回。小日誌窗是照日誌行的 `[源名]` 前綴過濾的，所以源改名之後，改名前既有的那些行會留在舊名字下、不再出現在這個源的小日誌窗裡；完整日誌頁一律看得到。
- **設定頁**：左下的齒輪把主區換成設定頁，目前有「關閉時縮到系統匣」與「開機自動啟動」兩個即時生效的開關；下方的 About 分節顯示版本號，以及「Config file」一列（副標是實際生效的設定檔完整路徑，點整列會開檔案總管並選中它）。
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
- 另外掛了 `window.__mock` 供演練特定狀態：`__mock.drop(1080)` 模擬斷線重連、`__mock.status(1080, "error", "…")` 直接指定狀態、`__mock.wipe()` 清掉所有源看零源空狀態、`__mock.configDelay(1500)` 讓 `config-changed` 晚於 invoke 的 resolve 送達（真後端就是這個順序，用來驗證改名後的選中不會被回退吃掉）、`__mock.reset()` 清掉暫存重來

假後端只在 `npm run dev` 且偵測不到 Tauri 時才會動態載入。正式建置時 `import.meta.env.DEV` 是常數 `false`，整段連同 `src/dev-mock.ts` 都會被搖掉，不會進打包產物。

## 建置

```
npm run tauri build
```

產物：

- 免安裝執行檔：`src-tauri/target/release/traytunnel.exe`
- NSIS 安裝檔：`src-tauri/target/release/bundle/nsis/`

注意：一定要走 `npm run tauri build`。直接下 `cargo build --release` 產出的執行檔會去連 Vite 開發伺服器（`devUrl`），而不是內嵌的前端檔案，開起來會是一片空白。`cargo build` 只適合拿來檢查 Rust 端能不能編譯。

免安裝使用時把 `traytunnel.exe` 放哪裡都行，設定檔預設落在 `%USERPROFILE%\.traytunnel.toml`；想連設定一起帶著走，把執行檔改名成 `p` 結尾（例如 `traytunnel-0.2.0p.exe`）或在旁邊放一個 `traytunnel.toml` 即可（見下方「設定檔」）。

## 設定檔

設定檔的位置在啟動時解析一次，優先序如下：

| 順序 | 位置 | 何時生效 |
| --- | --- | --- |
| 1 | `<執行檔同目錄>\traytunnel.toml` | **可攜模式**。兩個觸發條件任一成立即可（見下），整支程式的讀寫都留在執行檔旁邊 |
| 2 | `%USERPROFILE%\.traytunnel.toml` | 預設位置。沒進可攜模式時一律用它 |

### 可攜模式

兩種觸發方式，任一成立就進可攜模式，設定檔都是執行檔旁邊的 `traytunnel.toml`：

- **執行檔名以 `p` 結尾**（Rufus 那套命名記號，本尊就是 `rufus-4.5p.exe`）：產品名 `traytunnel` 不是 p 結尾，所以主檔名（副檔名以外的部分）的最後一個字元是 p 就是刻意加上去的可攜記號。大小寫不敏感，例如 `traytunnel-0.2.0p.exe`、`traytunnel-p.exe`。這種情況下設定檔還不存在時會**自動在執行檔旁邊建一份預設值**，就像 Rufus 建自己的 ini。
  只認結尾是為了避開誤判：`traytunnel - Copy.exe`（Windows 複製檔案自動取的名字，Copy 裡有 p）、`traytunnel-preview.exe` 這種都**不算**可攜。
- **執行檔旁放一個 `traytunnel.toml`**（KeePass／Rufus 那套同名檔偵測）：檔案存在就改用它，空檔也算（程式會補齊內容）；把它刪掉或改名就回到家目錄那份。

放在隨身碟上、或想讓同一台機器的多份執行檔各自帶設定時用得到。要注意檔名記號一旦成立就沒得退回：`traytunnel-p.exe` 不會再去讀家目錄那份設定。

第一次啟動若檔案不存在會自動在生效位置產生一份預設值，也可以直接複製範本：

```
copy traytunnel.toml.example %USERPROFILE%\.traytunnel.toml
```

實際生效的完整路徑一律以程式為準：設定頁的 About 分節有「Config file」一列，副標就是那個路徑，點整列會開檔案總管並選中該檔；啟動時的活動日誌也會記一行 `config: <路徑>`。

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
- **不會自動搬家**：舊版把設定檔固定放在執行檔同目錄，升級後那份檔案會直接被當成可攜模式繼續使用；想改用家目錄的預設位置，請自行把 `traytunnel.toml` 移到 `%USERPROFILE%\.traytunnel.toml`（執行檔旁邊那份要刪掉或改名，否則它優先）
- 升級注意：舊設定檔如果含有**重複的 `local` 埠**（舊版沒擋下來的話），升級時會被判為無法解析，另存一份 `.broken` 並改用預設值啟動，請手動把重複的埠改掉再放回去
- 設定檔解析失敗時**不會被覆寫**，程式會在同一個資料夾另存一份「生效檔名 + `.broken`」（家目錄模式是 `.traytunnel.toml.broken`，可攜模式是 `traytunnel.toml.broken`）並改用預設值繼續執行；內容自相矛盾（源名重複、跨源撞埠、`host`／`user` 空白）也算解析失敗
- 用 PowerShell 之類的工具存檔若帶了 UTF-8 BOM，也能正常解析

設定檔為個人本機設定，`traytunnel.toml` 已加入 `.gitignore`，不會被提交。
