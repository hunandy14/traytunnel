<p align="center"><img src="src-tauri/icons/128x128.png" width="96" alt="traytunnel icon"></p>

<h1 align="center">traytunnel</h1>

<p align="center">Windows 系統匣 SSH 隧道管理工具</p>
<p align="center"><i>Windows tray SSH tunnel manager built with Tauri</i></p>

<p align="center">
  <a href="https://github.com/hunandy14/traytunnel/releases"><img src="https://img.shields.io/github/v/release/hunandy14/traytunnel" alt="GitHub release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/hunandy14/traytunnel" alt="License"></a>
</p>

Windows 系統匣（tray）SSH 隧道管理工具，以 [Tauri v2](https://tauri.app/) 撰寫，前端是 vanilla TypeScript + Vite，隧道管理、設定檔讀寫與連通檢測全部在 Rust 側完成。

程式讀取一份 TOML 設定檔（預設是 `%USERPROFILE%\.traytunnel.toml`），支援**多組連線（connection）**（各自的 host／user／ProxyCommand），**每條隧道（tunnel）各自維持一條獨立的 SSH 連線**並在斷線時各自重連，同時對每條隧道做連通自我檢測，狀態即時顯示在系統匣圖示與主視窗中。

## 截圖

<p align="center">
  <img src="docs/screenshots/main-window.png" width="700" alt="主視窗">
</p>

<!-- 待補：托盤選單截圖
<p align="center">
  <img src="docs/screenshots/tray-menu.png" width="700" alt="托盤選單">
</p>
-->


## 功能

- 系統匣常駐，可設定開機自動啟動（啟動時帶 `--tray` 直接隱藏到系統匣）
- 多組連線：每個 `[[sources]]` 是一組獨立的 ssh 連線參數，底下各自掛自己的隧道，可整組 Connect／Disconnect／Reconnect
- 每個 `[[sources.forwards]]` 隧道一條獨立的 `ssh.exe -N -L`，可個別 Connect／Disconnect／Reconnect，一條隧道斷線或重連不會影響其他隧道
- 隧道的 Connect／Disconnect 選擇會寫回設定檔的 `enabled`，下次啟動只自動連線 `enabled` 的隧道（所有連線一起）
- 斷線後固定 5 秒重連，無退避、無次數上限，每條隧道自己數自己的
- 本地埠衝突三層防護：設定階段擋重複埠（跨連線也擋，訊息會點名佔用者與它所屬的連線）、spawn 前偵測埠是否已被其他程序佔用（狀態顯示 `port_busy`，每 5 秒重查而不盲目 spawn），最後由 ssh 的 `ExitOnForwardFailure=yes` 兜底
- 各隧道經本地 SOCKS5 埠檢測連通性，顯示對外 IP 與所在地（此功能會經隧道向第三方服務 ipinfo.io 發出請求）
- 支援透過 `ProxyCommand`（例如 `cloudflared access ssh`）連線
- 自動更新（VSCode 式）：啟動幾秒後與之後每 24 小時各查一次 Releases 上的 `latest.json`。**安裝版**（NSIS 裝的那一份）查到新版就在背景把安裝檔下載回來、驗過 minisign 簽章，存進 `%LOCALAPPDATA%\com.traytunnel.desktop\pending-update` 並記下它的 SHA-256；**下一次啟動的最早期**（UI 之前）重算一次雜湊確認檔案沒被動過，然後靜默交棒給安裝程式（NSIS 的 `/S /R /UPDATE`，連進度視窗都不出現），裝完由安裝程式把新版重新啟動起來——原本是 `--tray` 開機自啟進來的就照樣 `--tray`，不會突然彈一個視窗。不想等下一次啟動的話，設定頁的 `Restart to update` 與系統匣選單的同名項都可以現在就套用。同一版不會重複下載，下載失敗會退避重試（15 分、30 分、1 小時……封頂一天）並清掉殘檔，同一版連三次交棒都沒把版本換掉就放棄它、重新下載一份。**一般單檔與可攜版**不會改寫自己，只比對版本並把按鈕變成 `Get v<新版>`，開系統瀏覽器到那一版的 release 頁讓你自己換檔案（不下載、不安裝）。是不是安裝版由登錄檔的解除安裝資訊判定（`InstallLocation` 要真的就是這支執行檔的所在資料夾）。檢查與下載失敗完全靜默，只在活動日誌留一行，而畫面上會誠實顯示 `Download failed — will retry`（不是一顆轉到天荒地老的 spinner）。整條路可以用設定頁的「Automatic updates」開關（設定檔的 `checkForUpdates`）關掉，關掉後完全不發網路請求；**預設兩種模式都是開的**（可攜版原本預設關，但那讓可攜使用者完全失去「知道有新版」這件事，而它本來就只會拉一份幾百位元組的 latest.json）。關掉時會把已經下載好等著裝的那一份也一併丟掉，而且套用那一步自己還會再讀一次設定檔確認開關是開的——它跑在 `AppState` 存在之前，只有這樣「關掉之後不會再被自動更新」才是真的

  自動安裝還有兩道保護：**已經有另一個實例在跑時絕不套用**（使用者雙擊了第二次圖示，那個第二實例只該去喚醒既有視窗，不可以起安裝程式把他正在用的第一實例連同隧道一起關掉——探測走 single-instance 外掛自己那把 `{identifier}-sim` 具名鎖），以及**同一版連三次交棒都沒把版本換掉就放棄它**（免得變成每次開機都跑一次安裝程式的迴圈）
- 主視窗位置／大小會記住，下次啟動（含應用內更新後的重啟）沿用上次的位置與大小，不會每次都歸零置中
- 單一實例：重複啟動只會把既有的主視窗叫出來
- 系統匣提示跨連線彙總所有隧道狀態，例如 `Traytunnel - 3/4 connected`；右鍵選單是狀態行、隧道勾選、`Connect all`／`Disconnect all`／`Reconnect all`，多組連線時每組收成一個子選單（底下有 `Reconnect`）
- 系統匣圖示依 `SM_CXSMICON` 從多層 ICO 挑原生尺寸的那一層（含 16／20／24／28／32px），高 DPI 下不會被 GDI 拉伸糊掉
- 通知掛在自己的 AppUserModelID 底下：啟動時自註冊開始選單捷徑與 `HKCU\Software\Classes\AppUserModelId`，toast 顯示的是 Traytunnel 而不是 Windows PowerShell
- 每條 ssh 子程序各自放在一個 Windows Job Object 內，隧道停掉或程式結束時整棵程序樹（含 `cloudflared`）一起收掉

## 下載

到 [Releases](https://github.com/hunandy14/traytunnel/releases) 頁面抓最新版，依需求挑一種：

| 檔名 | 說明 |
| --- | --- |
| `traytunnel-<版本>.exe` | 一般單檔，免安裝，設定檔在家目錄（`%USERPROFILE%\.traytunnel.toml`） |
| `traytunnel-<版本>p.exe` | 可攜版，與上面**同一顆二進位**，檔名以 `p` 結尾＝設定檔跟著 exe 走 |
| `traytunnel-<版本>-setup.exe` | NSIS 安裝檔 |

每個 Release 也會附上 `SHA256SUMS.txt`，可用它核對下載檔案的完整性（例如 `Get-FileHash -Algorithm SHA256 <檔案>` 比對雜湊值是否一致）。另外還有一個 `latest.json`，那是應用內更新自己要讀的清單（版本號、安裝檔網址與簽章），不必手動下載。

本專案使用 SignPath Foundation 提供的憑證做程式碼簽章。

Free code signing provided by [SignPath.io](https://signpath.io/) , certificate by [SignPath Foundation](https://signpath.org/) .

## macOS 支援

macOS 版目前仍屬 **beta**：核心功能與 Windows 版對齊，但實機驗證的時間還沒有 Windows 版久，發佈時 Release 也會標記為 beta。

### 需求

- macOS 12（Monterey）以上
- **僅支援 Apple Silicon（`arm64`）**，沒有 Intel（`x86_64`）版本
- `ssh`（系統內建的 OpenSSH 用戶端即可，不必額外安裝）
- 選配：`cloudflared`，若你的 SSH 主機需要透過 Cloudflare Access 存取

### 安裝

到 [Releases](https://github.com/hunandy14/traytunnel/releases) 頁面下載其中一種：

| 檔名 | 說明 |
| --- | --- |
| `traytunnel-<版本>-aarch64.dmg` | 安裝映像，掛載後把 `Traytunnel.app` 拖進「應用程式」資料夾 |
| `traytunnel-<版本>-aarch64.app.tar.gz` | 免安裝的 `.app` 壓縮包，解壓後一樣要拖進「應用程式」資料夾 |

**不論哪一種，都務必先把 `Traytunnel.app` 拖進「應用程式」資料夾再開啟**，不要直接從掛載的 dmg 視窗或 `~/Downloads` 裡雙擊執行。這不只是慣例：macOS 對「帶隔離標記、卻沒被搬進正式位置」的 app 會做 App Translocation（Gatekeeper 的路徑隨機化），直接執行的話系統會把它塞進一個唯讀的隨機路徑跑，應用內更新在那個路徑下必定失敗（寫入被拒絕）；搬進「應用程式」資料夾之後 macOS 才不會再套用這個機制。

我們用的是 **ad-hoc 簽章**（沒有 Apple Developer 憑證），所以第一次開啟會被 Gatekeeper 擋下「無法驗證開發者」。解法：在「應用程式」資料夾裡對 `Traytunnel.app` 按右鍵 → **打開**，跳出的對話框再按一次「打開」即可；只有第一次需要這樣做，之後雙擊就能正常啟動。

### 已知限制

- **從 Finder／開機自啟啟動時，`PATH` 會由登入 shell 補回來**：launchd 給 GUI 行程的 `PATH` 只有 `/usr/bin:/bin:/usr/sbin:/sbin`，你在 `.zshrc`／`.zprofile` 裡加的東西一概不在裡面——而 ssh 的 `ProxyCommand`（預設值 `cloudflared access ssh --hostname %h`）是交給 `/bin/sh -c` 跑的，Homebrew 裝的 `cloudflared` 在 `/opt/homebrew/bin`，於是雙擊啟動的實例會每一條隧道都在 `sh: cloudflared: not found` 上失敗，從終端機啟動的同一支程式卻完全正常。traytunnel 啟動時若發現 `PATH` 就是那份最小集，會跑一次 `$SHELL -ilc` 把你真正的 `PATH` 問回來（五秒逾時，問不到就照原樣啟動並在活動日誌留一行）。從終端機啟動時這一步完全不會跑。Windows 版沒有這個問題（GUI 行程本來就繼承使用者的 `PATH`）。
- **開機自啟的開關是「下一次登入」生效**：打開就是寫一份 plist 進 `~/Library/LaunchAgents`，關掉就是把它刪掉，launchd 在下一次登入時讀那個資料夾。程式**不會**呼叫 `launchctl load`／`unload` 讓它立即生效——那樣做會有兩個很不舒服的副作用：`load` 會當場多開一個實例（因為那份 plist 的 `RunAtLoad` 是 true），而 `unload` 在「這一次就是開機自啟進來的」情況下等於請系統把 traytunnel 自己殺掉（連同它管的 ssh 一起變成孤兒）。Windows 版寫 HKCU 的 Run 值同樣是下次登入才生效，兩邊語意一致。
- **沒搬進「應用程式」資料夾就開啟時，開機自啟會拒絕開啟**：直接從 dmg 視窗或 `~/Downloads` 雙擊時，macOS 會做 App Translocation，把程式掛在一個唯讀的隨機路徑（`/private/var/folders/…/AppTranslocation/…`）底下跑。那個路徑下次登入就不存在了，寫進 LaunchAgent 只會得到「開關顯示為開、實際永遠啟動不到」。因此這種情況下開關會直接回一句「請先把 `Traytunnel.app` 搬進『應用程式』資料夾再開啟」而不寫入；同一道保護也讓自啟自癒不會把你原本指向 `/Applications` 的那份好設定覆寫掉。
- **開機自啟的偵測不含「使用者在系統設定裡手動停用」的狀態**：程式判斷開機自啟是否生效，看的是自己有沒有寫入 `~/Library/LaunchAgents` 底下的 plist；如果你在「系統設定 → 一般 → 登入項目」把它關掉，程式不會發現，畫面上的開關依然顯示為開啟。Windows 版有對齊「工作管理員」的停用紀錄，macOS 版目前還沒有對應的偵測。
- **被強制結束（`kill -9`）或當掉時，那一瞬間的 ssh 會活下來，但下一次啟動會清掉**：Windows 版靠 Job Object 由核心保證「主程式沒了，整棵程序樹就沒了」，macOS 沒有等價機制。正常退出、Dock 的 Quit、登出、`kill`（SIGTERM）、Ctrl+C 都會走過完整的收尾；只有 `kill -9` 與真正的當機來不及。那種情況下 traytunnel 會在下一次啟動時，比對 `~/Library/Application Support/com.traytunnel.desktop/supervised-pgids.json` 裡記下的命令列，把上一輪留下、還握著本地埠的 ssh 清掉。

## 介面

介面上的用詞與設定檔的結構對應：一個 `[[sources]]` 在畫面上叫一條**連線**（connection），底下的每個 `[[sources.forwards]]` 叫一條**隧道**（tunnel）。

視窗分成左側的連線軌道與右側的主區，兩者都可隨視窗縮放（最小 480×420）：

- **左側連線軌道**：每條連線一個圓角方塊，圖案是連線名首字，底色由名稱 hash 決定，右下角的小點是該連線的彙總狀態（全連綠／部分琥珀／全停灰／有隧道出錯紅）。清單底部的虛線「＋」新增連線；左下角固定放活動日誌與設定兩個鈕。
- **主區（選中的連線）**：頂部彙總卡分三段——左邊是連線名稱與 `ssh user@host`，中間用豎分隔線隔出大分數 `n/m` 與 `CONNECTED` 小字（顏色跟著整條連線的健康度走），右邊只有一顆「⋯」。⋯ 點開的選單收了 Add tunnel、Disconnect／Connect（整條連線一起）、Reconnect、Activity，分隔線下再放 Edit connection。
- **隧道清單**：彙總卡下方是標題 `TUNNELS` 的隧道清單，整份包在單一外框裡，列與列之間用內縮的細分隔線分開，滑過去整列微亮。每列左側是狀態點、名稱與 `:local → remote`，右側兩行是最近一次自測結果（上行地點、下行對外 IP），最右邊是連接／中斷、重新連接、編輯三個鈕。外框高度跟著列數長，長到超過主區可用高度時就固定在那裡、改由框內捲動，外框與圓角一直留在畫面上。
- **活動日誌頁**：左下的時鐘鈕（或 ⋯ 選單的 Activity）把主區換成所有連線的完整日誌，點任一連線 icon 即返回。
- **設定頁**：左下的齒輪把主區換成設定頁。General 分節有「關閉時縮到系統匣」、「開機自動啟動」、「自動更新」三個即時生效的開關。下方的 About 分節有兩列：
  - **版本列**——左邊是 app 的盾牌 logo，中間的標題平時是 `Version`、有新版時變成 `Update available`，副標永遠只是純版號（`v0.6.1`）。右邊是一顆 split button：**沒有事情可做時主鈕整顆不出現**，只留右側的柄。安裝版查到新版之後主鈕會依序走 `Downloading v<新版>…`（後端在背景下載，不必按任何東西）→ 綠色的 `Restart to update (v<新版>)`；按下去就交棒給安裝程式，鈕停在 `Restarting…`。下載失敗時它會誠實地變成琥珀色的 `Download failed — will retry`（不轉圈——轉圈的意思是「正在下載」，而那時並沒有）。可攜／單檔版沒有自動更新這條路，主鈕直接是綠色的 `Get v<新版>`，按下去開那一版的 release 頁。右側的柄點開下拉，收兩個次要動作：`View release notes`（開手上那一版的 release 頁，沒有就是 `releases/latest`）、`Download from Releases`（開 Releases 列表頁自己挑版本）。主鈕與圖示欄都是固定寬度、下拉走絕對定位，狀態文字再怎麼變版面都不會跳動。手動的「Check for updates／Check now」已經拿掉——背景每天查一次、查到就自己下載，那顆鈕能做的事後端早就做完了。
  - **Config file 一列**——副標是實際生效的設定檔完整路徑，右側的圖示鈕開檔案總管並選中它。
- **連線編輯**：⋯ 選單的 Edit connection 或側欄的「＋」會開出置中的編輯 sheet（name／host／user／ProxyCommand），驗證錯誤逐欄顯示；刪除連線需要一次確認。頁腳左側的 Test 鈕可以在存檔前就拿表單當下填的值試連一次（spawn 一次性 `ssh ... exit`，不建立任何轉發），結果就地顯示成功的綠字「Connected」或失敗的紅字（帶 ssh 的錯誤原因，例如 DNS 解析失敗、逾時、金鑰被拒），逾時 15 秒兜底；host／user 空白時按 Test 直接顯示欄位驗證錯誤，不會真的去連。
- **隧道編輯**：列上的鉛筆、⋯ 選單的 Add tunnel、以及零隧道時的虛線引導卡，都開同一個 sheet（name／local port／remote）。remote 欄的提示是 `1080 (server-side port) or host:port`——只填埠號就是伺服器本機的那個埠，補成 `host:port` 由後端做。刪除隧道不跳確認，先從畫面移除、5 秒內可以按 Undo 收回。

## 需求

執行：

- Windows 10/11，內建 WebView2 Runtime（Windows 11 已預裝）
- `ssh.exe`（OpenSSH 用戶端）
- 選配：`cloudflared`，若你的 SSH 主機需要透過 Cloudflare Access 存取

開發與建置另外需要：

- [Rust 工具鏈](https://www.rust-lang.org/tools/install)（含 MSVC Build Tools）
- [Node.js](https://nodejs.org/) 20 以上

## 開發

```
npm install
npm run dev
```

要在 `src-tauri/src/platform/` 底下加平台相依功能，或想知道跨平台程式碼的規則，先看
[`docs/platform-guide.md`](docs/platform-guide.md)（一頁內講完：資料夾結構、共用核心的
可見性規則、新增功能的流程、CI 雙腿守門、live 測試慣例）。

### 瀏覽器 UI 開發模式

只想調畫面、不想每次都等 Rust 編譯時，可以只開前端：

```
npm run web:dev
```

然後用瀏覽器打開 http://localhost:1420/ 。整個 UI 只有這一頁，主區靠左側欄切換。

這個模式下沒有 Tauri runtime，前端會自動掛上一層假後端：

- 用官方的 `@tauri-apps/api/mocks` 的 `mockIPC`（開啟 `shouldMockEvents`）攔截所有 `invoke`，並讓 `listen`／`emit` 走記憶體，所以前端程式碼完全不用為了 mock 改寫
- 偵測方式是 Tauri v2 官方提供的 `isTauri()`，偵測不到才啟用
- 假資料有三條連線（`tokyo` 兩條隧道、`taipei` 兩條隧道、`lab` 零隧道示範空狀態），涵蓋每條隧道獨立的 `connecting → connected`、自測 `testing → ok`／`fail`、固定會撞埠的 `port_busy`，以及跨連線的本地埠衝突；隧道的連接／中斷／重新連接、整條連線的啟停與重接、隧道的新增／編輯／刪除（含 undo）、連線的新增／編輯／刪除都能實際操作
- 設定存檔只寫進 `sessionStorage`，不會碰到真的設定檔
- 另外掛了 `window.__mock` 供演練特定狀態：`__mock.drop(1080)` 模擬斷線重連、`__mock.status(1080, "error", "…")` 直接指定狀態、`__mock.wipe()` 清掉所有連線看零連線空狀態、`__mock.configDelay(1500)` 讓 `config-changed` 晚於 invoke 的 resolve 送達（真後端就是這個順序，用來驗證改名後的選中不會被回退吃掉）、`__mock.reset()` 清掉暫存重來
- 版本列那顆 split button 的每一個狀態都演得到，兩條車道各一套：
  - `__mock.update("installed", "0.7.0")` 演整條自動更新：主鈕先轉 `Downloading v0.7.0…`，約兩秒後變成綠色的 `Restart to update (v0.7.0)`
  - `__mock.update("portable", "0.7.0")` 演可攜車道：主鈕直接是綠色的 `Get v0.7.0`，沒有下載那一段
  - `__mock.updateStalls()`（先 `update("installed")` 再叫）演下載失敗：主鈕變成琥珀色的 `Download failed — will retry`，**不轉圈**
  - `__mock.updateNone()` 回到沒有更新的狀態：標題退回 `Version`，主鈕整顆消失
  - `__mock.updateFails()` 讓按下 `Restart to update` 演成失敗，看鈕從 `Restarting…` 彈回來、原因寫進設定頁的錯誤列
  - 真後端的背景車道對「已是最新」與「檢查失敗」都是靜默的（失敗只在活動日誌留一行），畫面上沒有對應的狀態，所以那兩種結果沒有東西好演

假後端只在 `npm run web:dev` 且偵測不到 Tauri 時才會動態載入。正式建置時 `import.meta.env.DEV` 是常數 `false`，整段連同 `src/dev-mock.ts` 都會被搖掉，不會進打包產物。

## 建置

四個指令，差別只在目標平台、要不要順便打包安裝檔：

| 指令 | 做什麼 |
| --- | --- |
| `npm run build:win:exe` | 只編 Windows 免安裝執行檔，跳過打包（`tauri build --no-bundle`），平常改完程式驗一下最快 |
| `npm run build:win:setup` | Windows 執行檔＋NSIS 安裝檔（`tauri build --bundles nsis`） |
| `npm run build:mac` | macOS `.app`（`tauri build --bundles app`），建完複製一份到 `bin/` 方便本機直接雙擊試跑 |
| `npm run build:dist` | 走設定檔裡列的全部 bundle 目標（`tauri build`），當前平台的完整發佈建置＋打包，要發佈時用，CI 的 `release.yml` 雙腿共用這顆指令 |

沒有裸的 `npm run build`：光看這個名字猜不出是要建前端還是建整個 app，改名後打錯字會直接得到明確的 `missing script` 錯誤（腳本命名規則見 [`docs/platform-guide.md`](docs/platform-guide.md#scripts-命名規則)）。

`build:win:exe`／`build:win:setup`／`build:dist` 會在建置後跑 `node scripts/package.mjs`，把產物複製成發佈用的檔名放進根目錄的 `out/`（每次重跑會先清空）：

| 發佈檔 | 說明 |
| --- | --- |
| `out/traytunnel-<版本>.exe` | 一般單檔，設定檔走 `%USERPROFILE%\.traytunnel.toml` |
| `out/traytunnel-<版本>p.exe` | 可攜版。**與上面同一顆二進位**，差別只在檔名結尾的 `p`——那個 p 就是可攜模式的記號，設定檔改放 exe 旁邊 |
| `out/traytunnel-<版本>-setup.exe` | NSIS 安裝檔 |

`build:mac` 跑的是另一支腳本 `node scripts/copy-app-bundle.mjs`，把 `.app` 複製到根目錄的 `bin/`（同樣每次重跑先清空），不寫 `out/`——macOS 的正式發佈檔（`.dmg`／`.app.tar.gz`）是 `build:dist` 的產物。

版本號取自 `src-tauri/Cargo.toml` 的 `[package]` `version`（單一來源）。來源檔還沒建出來的那一項會跳過並印一行提示，所以 `build:win:exe` 不產安裝檔也能照跑。原始產物仍留在 `src-tauri/target/release/` 底下，`out/`／`bin/` 只是複製出來的方便命名版本，都已列入 `.gitignore`。

更新簽章：`tauri.conf.json` 開了 `bundle.createUpdaterArtifacts`，打包時會替 updater 產物（Windows 的 NSIS 安裝檔、macOS 的 `.app.tar.gz`）簽出一份 `.sig`。**本機沒有簽章私鑰時**：`build:win:setup`／`build:dist` 會在最後一步失敗——安裝檔本身已經產出，但簽不出 `.sig`，`tauri` 以非零狀態結束，後面的 `scripts/package.mjs` 也就不會跑到；`build:win:exe`（`--no-bundle`）完全不受影響，本來就不會走到打包與簽章那一步。`build:mac` 則是刻意在指令裡多帶一個 `--config '{"bundle":{"createUpdaterArtifacts":false}}'`，只為本機快速試跑 `.app` 這個用途覆寫掉 updater 產物開關——沒有這個覆寫的話，`--bundles app` 一樣會嘗試產出 `.app.tar.gz` 並簽章，本機沒有私鑰時會在最後一步炸掉，`.app` 明明已經建好卻因為 `&&` 短路，`scripts/copy-app-bundle.mjs` 完全不會被跑到，`bin/` 什麼都拿不到（這正是 `build:mac` 曾經回報過的「npm run 跑失敗、直接下 `tauri build --bundles app` 卻像是成功」的根因：兩者其實同樣會失敗，只是後者的失敗訊號很容易在只看終端機最後幾行、或指令有接 `| tee`／`| tail` 之類管線吃掉 exit code 時被忽略掉）。平常只是要驗程式請用 `build:win:exe`（Windows）或 `build:mac`（macOS），兩者都不需要簽章私鑰也能穩定出 `.exe`／`.app`；真的要在本地打包安裝檔或 updater 產物時，先設好 `TAURI_SIGNING_PRIVATE_KEY`（私鑰檔的**內容**）與 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`（沒設密碼也要給空字串，否則 tauri 會停下來問）。私鑰放在 repo 的 `secrets\`，已列入 `.gitignore`；CI 的 `release.yml` 則從 GitHub Secrets 取同一把鑰匙。私鑰與 `tauri.conf.json` 裡的 `plugins.updater.pubkey` 對不起來時 tauri 會印一行警告，那種簽章在使用者端會驗不過。

注意：一定要走上面這幾個指令（底層都是 `tauri build`）。直接下 `cargo build --release` 產出的執行檔會去連 Vite 開發伺服器（`devUrl`），而不是內嵌的前端檔案，開起來會是一片空白。`cargo build` 只適合拿來檢查 Rust 端能不能編譯。

免安裝使用時把 `traytunnel.exe` 放哪裡都行，設定檔預設落在 `%USERPROFILE%\.traytunnel.toml`；想連設定一起帶著走，把執行檔改名成 `p` 結尾（例如 `traytunnel-0.2.0p.exe`）或在旁邊放一個 `traytunnel.toml` 即可（見下方「設定檔」）。

### 版本管理

單一權威是 `src-tauri/Cargo.toml` 的 `[package]` `version`；`package.json` 是惰性副本。`src-tauri/tauri.conf.json` 沒有 `version` 欄位——這是刻意的：Tauri v2 官方規定省略該欄位時會 fallback 讀 `src-tauri/Cargo.toml` 的 `package.version`，`getVersion()`、NSIS 安裝檔名、`tauri-action` 全部自動跟著這顆版號走，不用再另外同步一份。升版一律用：

```
npm run bump <x.y.z>
```

會驗參數是嚴格 semver、確認兩處現值一致後才動手改，改完不會自動 commit、不會自動 tag，只印一行建議指令供複製。下次建置時 `Cargo.lock` 裡的版號會跟著更新，記得跟兩個檔案一併提交。

### Release 流程

發版走兩個 workflow 接力：`.github/workflows/autotag.yml` 負責建立並推送 tag，`.github/workflows/release.yml` 在 `windows-latest` 與 `macos-14` 兩個 runner 上分別跑 `npm run build:dist`，兩腿的建置產物與簽章下載回來後，再由 compose job 合併成一份雙平台的 `latest.json`，與 `out/*.exe`、`out/*.dmg`、`out/*.app.tar.gz`、`SHA256SUMS.txt` 一起上傳成 GitHub Release。主流程只需要一個指令：

```
npm run release <x.y.z>
```

它（`scripts/release.mjs`）會依序：建立 `release/<x.y.z>` 分支 → 跑 `npm run bump <x.y.z>` 同步版本號 → 同步 `Cargo.lock`／`package-lock.json` → commit → push → `gh pr create` 開 PR → `gh pr merge --auto --merge` 掛上 auto-merge → 切回 `main`。

`release.yml` 也支援 `workflow_dispatch` 只重建其中一個平台（例如單獨補發 macOS）；`latest.json` 只有一個全域 `version` 欄位、沒有 per-platform 版本，因此單平台發佈有先天限制，用法與風險見 [`docs/release-risk-register.md`](docs/release-risk-register.md)。

`main` 已開 branch protection，required status check 綁定的是 job 名稱，目前是 `.github/workflows/ci.yml` 裡的 `ci` 這個 job，要它綠燈才能合併（若之後改了這個 job 的 id，記得同步更新 branch protection 設定，否則 required check 會找不到對應狀態而永遠卡住）；`gh pr merge --auto` 會等 CI 通過後自動合併，合併進 `main` 後 `autotag.yml` 偵測到版號變動，自動建立並推送 tag `v<x.y.z>`，接著觸發 `release.yml` 建置發佈——全程不必再手動介入。

任一步失敗就地停止（不自動回滾），並印出目前停在哪一步、怎麼收拾。合併前想反悔：`gh pr merge --disable-auto`（取消 auto-merge，PR 留著）或直接把 PR 關掉。

前置檢查會擋下不高於現行版本（`src-tauri/Cargo.toml` 的 `version`）的輸入，相等也算擋下，避免手滑打錯版號或重複發同一版。

正式跑之前可以先 `npm run release <x.y.z> --dry-run`，只印出每一步會執行的指令，不建分支、不改檔案、不 push、不開 PR。

<details>
<summary>備援：手動流程（不用 <code>npm run release</code>）</summary>

`npm run release` 本質上就是把下面這串手動步驟接起來；需要更細的掌控（例如中途要插入其他改動再一起 commit）時可以照這樣手動跑：

```
npm run bump <x.y.z>
cargo update -p traytunnel --manifest-path src-tauri/Cargo.toml
npm install --package-lock-only
git add src-tauri/Cargo.toml src-tauri/Cargo.lock package.json package-lock.json
git commit -m "版本升級至 <x.y.z>"
git push
```

開 PR、合併進 `main` 後，`autotag.yml` 偵測到 `src-tauri/Cargo.toml` 的版號變動，會自動建立 annotated tag `v<x.y.z>` 並推送，接著主動 dispatch `release.yml` 觸發建置發佈（GitHub 的防遞迴機制不會讓 `GITHUB_TOKEN` push 的 tag 自己觸發其他 workflow，所以由 `autotag.yml` 補這一腳）；`release.yml` 收到後照原本邏輯確認同名 tag 已存在再建置。

</details>

以下兩項是更底層的備援手段，主流程（不論是 `npm run release` 還是上面的手動流程）正常運作時都不需要用到：

<details>
<summary>備援：手動建立並推送 tag</summary>

`autotag.yml` 若因故沒有跑（例如版號變動沒有進到 `main`、workflow 被停用），可以自己補 tag 觸發：

```
git tag -a v<x.y.z> -m "v<x.y.z>"
git push --tags
```

push tag（`v*`）會自動觸發 `release.yml`，比對 tag 版號與 `src-tauri/Cargo.toml` 的 `version` 一致後才建置發佈，不一致直接 fail。

</details>

<details>
<summary>備援：Actions 頁手動補發</summary>

發佈失敗要重跑時，改走 Actions 頁面的 *Run workflow*（`release.yml` 的 `workflow_dispatch`）：不用帶參數，直接以目前 `src-tauri/Cargo.toml` 的版號去掛對應的既有 tag（例如版號是 `0.4.1` 就掛 `v0.4.1`）；tag 還沒建立會直接 fail 並在 log 裡註明要先 tag、push --tags。

</details>

## 設定檔

設定檔的位置在啟動時解析一次，優先序如下：

| 順序 | 位置 | 何時生效 |
| --- | --- | --- |
| 1 | `<執行檔同目錄>\traytunnel.toml` | **可攜模式**。兩個觸發條件任一成立即可（見下方摺疊區塊），整支程式的讀寫都留在執行檔旁邊 |
| 2 | `%USERPROFILE%\.traytunnel.toml` | 預設位置。沒進可攜模式時一律用它 |

可攜模式概述：執行檔主檔名以 `p` 結尾，或執行檔旁放一個 `traytunnel.toml`，任一成立即進可攜模式，設定檔就在執行檔旁邊。完整判定規則、誤判排除見下方摺疊區塊。

第一次啟動若檔案不存在會自動在生效位置產生一份預設值，也可以直接複製範本：

```
copy traytunnel.toml.example %USERPROFILE%\.traytunnel.toml
```

實際生效的完整路徑一律以程式為準：設定頁的 About 分節有「Config file」一列，副標就是那個路徑，點整列會開檔案總管並選中該檔；啟動時的活動日誌也會記一行 `config: <路徑>`。

<details>
<summary>可攜模式完整判定規則</summary>

兩種觸發方式，任一成立就進可攜模式，設定檔都是執行檔旁邊的 `traytunnel.toml`：

- **執行檔名以 `p` 結尾**（Rufus 那套命名記號，本尊就是 `rufus-4.5p.exe`）：產品名 `traytunnel` 不是 p 結尾，所以主檔名（副檔名以外的部分）的最後一個字元是 p 就是刻意加上去的可攜記號。大小寫不敏感，例如 `traytunnel-0.2.0p.exe`、`traytunnel-p.exe`。這種情況下設定檔還不存在時會**自動在執行檔旁邊建一份預設值**，就像 Rufus 建自己的 ini。
  只認結尾是為了避開誤判：`traytunnel - Copy.exe`（Windows 複製檔案自動取的名字，Copy 裡有 p）、`traytunnel-preview.exe` 這種都**不算**可攜。
- **執行檔旁放一個 `traytunnel.toml`**（KeePass／Rufus 那套同名檔偵測）：檔案存在就改用它，空檔也算（程式會補齊內容）；把它刪掉或改名就回到家目錄那份。

放在隨身碟上、或想讓同一台機器的多份執行檔各自帶設定時用得到。要注意檔名記號一旦成立就沒得退回：`traytunnel-p.exe` 不會再去讀家目錄那份設定。

</details>

<details>
<summary>術語對照與欄位詳表</summary>

**術語對照**：介面上稱 **Connection**（一組 SSH 連線）與 **Tunnel**（一條轉發），設定檔的鍵名維持原樣不動——`[[sources]]` 就是 Connection、`[[sources.forwards]]` 就是 Tunnel。手改檔案的人不必跟著改名，舊檔案也照吃。

頂層欄位：

| 欄位 | 說明 |
| --- | --- |
| `closeToTray` | 關閉鈕（X）是否只隱藏到系統匣 |
| `checkForUpdates` | 是否在**背景**檢查新版（啟動後一次，之後每 24 小時一次）。**省略時的預設值跟著模式走**：一般模式視為 `true`，可攜模式視為 `false`。關閉時背景完全不發網路請求，但版本列上手動按下的檢查不受它管（親手按鈕就是對那一次連外的明示同意）。設定頁 General 分節有對應的開關，動過開關才會把這個鍵寫進檔案 |
| `[[sources]]` | 一組連線（介面稱 Connection），含 `name`、`host`、`user`、`proxyCommand` 與底下的 `[[sources.forwards]]` |

`[[sources]]` 的欄位：

| 欄位 | 說明 |
| --- | --- |
| `name` | 連線名稱，不可空白也不可含空格，且不可與其他連線重複 |
| `host` | SSH 主機 |
| `user` | SSH 使用者 |
| `proxyCommand` | ssh 的 `ProxyCommand`，不需要時留空字串 |

`[[sources.forwards]]` 的欄位（介面稱 Tunnel）：

| 欄位 | 說明 |
| --- | --- |
| `name` | 隧道名稱，不可空白也不可含空格 |
| `local` | 本地埠，同時是這條隧道的**全域**唯一鍵，跨連線也不可重複（含停用中的） |
| `remote` | 轉發目的地，格式 `host:port`。**只填埠號**（1-65535）代表伺服器本機的那個埠：介面上可以這樣填，直接手寫在檔案裡也算數，兩條路都會補成完整的 `127.0.0.1:<port>`，下次存檔後檔案裡就是完整形式 |
| `enabled` | 是否要保持連線；省略時視為 `true` |

每條隧道各自跑一條 `ssh`，連線參數（`host`／`user`／`proxyCommand`）取自它所屬的那組連線，並以自己的 `local` 埠是否進入 Listen 狀態判斷這條隧道是否連上。在介面上按 Connect／Disconnect 會即時寫回對應的 `enabled`。

也可以在程式裡編輯，存檔會寫回同一個檔案並保留你手寫的註解（包含寫在單一 `[[sources]]` 或單筆 `[[sources.forwards]]` 上方的註解）。

</details>

<details>
<summary>`.broken` 與遷移相關</summary>

- **舊制設定檔自動遷移**：偵測到頂層還有 `host` 欄位（只有單一連線的舊格式）時，會把它整包成一個 `[[sources]]`（連線名稱預設用 `host` 的值，其中的空白與中括號會被剝掉，例如 `[::1]` 會變成名稱 `::1`）、把原本的 `[[forwards]]` 搬成 `[[sources.forwards]]`，並就地寫回新格式；檔頭與逐筆隧道上方的註解都會保留
- **不會自動搬家**：舊版把設定檔固定放在執行檔同目錄，升級後那份檔案會直接被當成可攜模式繼續使用；想改用家目錄的預設位置，請自行把 `traytunnel.toml` 移到 `%USERPROFILE%\.traytunnel.toml`（執行檔旁邊那份要刪掉或改名，否則它優先）
- 升級注意：舊設定檔如果含有**重複的 `local` 埠**（舊版沒擋下來的話），升級時會被判為無法解析，另存一份 `.broken` 並改用預設值啟動，請手動把重複的埠改掉再放回去
- 設定檔解析失敗時**不會被覆寫**，程式會在同一個資料夾另存一份「生效檔名 + `.broken`」（家目錄模式是 `.traytunnel.toml.broken`，可攜模式是 `traytunnel.toml.broken`）並改用預設值繼續執行；內容自相矛盾（連線名稱重複、跨連線撞埠、`host`／`user` 空白）也算解析失敗
- 用 PowerShell 之類的工具存檔若帶了 UTF-8 BOM，也能正常解析

</details>

設定檔為個人本機設定，`traytunnel.toml` 已加入 `.gitignore`，不會被提交。

## 授權

MIT License，詳見 [LICENSE](LICENSE)。
