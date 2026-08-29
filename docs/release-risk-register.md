# 發佈與應用內更新——已知風險登記簿

記錄三類不是我們自己寫的 bug、卻會影響使用者的風險：上游 `tauri-plugin-updater`
在 macOS 提權路徑上的缺陷、macOS `WKWebView` content process 被系統回收的白屏
風險，以及發佈管線本身（`latest.json` 的 manifest 格式）帶來的先天限制。三者
都已經在程式碼裡繞開、防護或擋下，這裡集中記一份，避免下一個人重新踩一次才
想起來。

## 上游 `tauri-plugin-updater` 的兩筆 macOS 缺陷

版本：`tauri-plugin-updater = "2"`（目前解析到 2.10.1），問題出在
`src/updater.rs` 的 macOS `install_inner`（`need_authorization` 為真、也就是
`.app` bundle 對目前使用者寫不動、必須提權的那條分支）。

### 1. 提權路徑的 `rm -rf` 沒有備份

非提權路徑（一般 admin 帳號、自己拖進 `/Applications` 的 bundle）會先把舊
bundle `rename` 進暫存目錄當備份，新 bundle 解壓失敗或搬移失敗都還原得回去。
**提權路徑不是這樣**：外掛組出的 AppleScript 是

```
do shell script "rm -rf '{舊 bundle 路徑}' && mv -f '{新 bundle 路徑}' '{舊 bundle 路徑}'" with administrator privileges
```

`rm -rf` 直接刪掉舊 bundle，中間完全沒有備份；如果 `mv` 那一步失敗（磁碟滿、
中途被使用者取消密碼框、系統中斷……），使用者只剩一個空位置，沒有 app 可以開，
也沒有東西可以自動復原。

### 2. AppleScript 裡的路徑用單引號包住、沒有跳脫

上面那段 `do shell script` 字串是用 Rust 的 `format!` 把兩個路徑
（`self.extract_path`、`tmp_extract_dir.path()`）直接用 `'...'` 包起來拼進去，
沒有對路徑本身的單引號字元做任何跳脫。如果 bundle 所在路徑含有單引號（例如
使用者自建的資料夾名稱裡有撇號），組出來的 shell 命令會在該處被單引號提前結束，
輕則整條命令解析失敗、更新中止，重則字串其餘部分被當成新的 shell token 執行。
`tmp_extract_dir` 是 `tempfile` crate 產生的隨機路徑，實務上不會含單引號；
真正的風險面是 bundle 自己的安裝路徑。

### 本專案的緩解

兩筆都不是我們能改的外掛內部實作，緩解走的是「讓提權路徑幾乎不會被觸發、觸發
時使用者一定在場」：

- **絕不背景安裝**：`platform::macos::update` 的背景車道（啟動後與之後每
  24 小時各查一次）**只查版本、只填狀態、只記活動日誌一行，不下載、不安裝**
  （見 `src-tauri/src/platform/macos/update.rs` 模組開頭「背景車道做什麼」一節）。
  真正的更新入口只有一個：設定頁那顆綠色主鈕（對應的 `install` 函式），必須使用者親自按下。
  這代表上面兩筆缺陷——尤其是「斷在中途沒有 app 可以開」——只可能發生在使用者
  主動觸發、正看著畫面、還能重試或回報的當下，不會在他不在場、或以為程式只是
  照常待在系統匣時，被背景邏輯默默炸掉。
- **README 把使用者導向不會觸發提權、也不會踩到單引號的路徑**：安裝指示明講
  「把 `Traytunnel.app` 拖進『應用程式』資料夾再開啟」——`/Applications` 是
  固定的系統路徑、不含特殊字元，且一般 admin 帳號對自己拖進去的 bundle 有寫入
  權限，走的是非提權的 `rename`＋備份路徑，兩筆缺陷都碰不到。真正會撞上提權
  路徑的情境（bundle 是 root 所有、或曾經 `sudo cp` 過）超出一般使用者的操作
  習慣，不在我們的緩解範圍內。

## macOS `WKWebView` content process 被系統回收後的白屏風險

`WKWebView` 的渲染跑在獨立於本體行程的系統行程（`com.apple.WebKit.WebContent`），
這個行程可以被系統獨立回收，不需要我們的 app 本身也被殺掉——這是 WebKit 本身
有文件記載的行為，不是 tauri 特有的問題。真正的觸發條件是系統記憶體壓力
（jetsam 式回收），不是單純隱藏視窗就會發生；但本專案的視窗語意是「隱藏到
系統匣」而不是真的關閉（`lib.rs::hide_to_tray`），而且會在隱藏時把 activation
policy 切到 `Accessory`（不進 Dock、不在前景，見 `show_main`／`hide_to_tray`
那組動態 activation policy）——這樣一個長時間背景常駐、沒有可見視窗的 app，
它的 WebContent 行程在系統真的碰上記憶體壓力時，會是優先被犧牲的對象。行程
死掉後 `WKWebView` 不會自己重新載入，畫面會一直卡在白屏，直到使用者手動整個
重開 app。

### 本專案的防護

`src-tauri/src/lib.rs` 的 `tauri::Builder` 鏈掛了官方原生的偵測掛鉤：
`Builder::on_web_content_process_terminate`（macOS／iOS 專屬，已核對存在於我們
釘死的 `tauri-v2.11.5` 標籤，`crates/tauri/src/app.rs` 第 1798 行；底層是 wry
的 `wkwebview/navigation.rs` 把 `WKUIDelegate` 的 `webContentProcessDidTerminate:`
轉呼叫上來）。content process 死掉時會先記一筆 `log::warn!`，接著呼叫
`Webview::reload()` 自救——tauri 本身不會自動重載，這兩步都是我們自己接的。

### 診斷指引：使用者若再遇到白屏

第一步查 `traytunnel.log`（macOS 預設路徑
`~/Library/Logs/<identifier>/traytunnel.log`）有沒有
`webview content process terminated` 這行：

- **有這行**：就是這個機制觸發了，`reload()` 應該已經自動處理掉；如果 reload
  之後畫面還是白的，代表自救本身失敗，緊接著會多一行
  `could not reload the webview after content process termination`，要往
  「reload 當下時機不對」或「某些 macOS 版本 reload 不可靠」的方向查。
- **沒有這行**：白屏另有原因，不是這個機制——先查資源載入路徑（絕對路徑／
  自訂協定那類問題，見 `fix/macos-white-window` 的修正）或其他成因。

這個防護目前無法在沒有真實記憶體壓力的情況下本機實測觸發（`memory_pressure`
工具可以模擬，但屬於壓力測試，未列入本次交付範圍），靠型別檢查與 CI 守門；
一旦有使用者真的在日誌裡看到這行 warn，就是這條防護第一次被證實在真實情境
下觸發過，值得回頭補一筆記錄。

## 發佈管線：`latest.json` 的 `version` 全域語意限制

Tauri 的 updater manifest 只有一個全域 `version` 欄位，沒有 per-platform 版本。
`release.yml` 支援 `workflow_dispatch` 只重建某一個平台（`platform: windows` /
`macos`），但「只發某一個平台的新版本」在語意上必然讓另一個平台的使用者看到
`version` 前進、卻拿到上一版的 `url`（甚至是這次 release 上根本不存在的資產）。
這不是實作疏漏，是 manifest 格式本身的限制，沒有辦法在不改 Tauri 的情況下修掉。

`scripts/lib/latest-json.mjs` 的 `mergeLatestJson` 把這條限制關進「陳舊條目
斷言」：合併時，任何從底稿沿用（這次沒建置）的平台條目，其 `url` 都必須指向
這次的 release tag，否則預設硬失敗，訊息指向 `allow_stale_platforms` 這個逃生門。

**`allow_stale_platforms=true`（`release.yml` 的 workflow input）何時該用：**

- **該用**——往「已經發過、tag 相同」的 release 補另一條腿（例如 `v0.6.6` 已經
  發過 Windows，現在單獨補 macOS）。此時底稿裡 Windows 的條目本來就指向
  `v0.6.6`，斷言自動通過，通常根本不需要這個開關；只有底稿條目意外對不上同一
  個 tag（例如上一輪只成功了一半）時才需要顯式帶這個開關重跑，且要先確認另一
  平台的資產確實已經在這個 tag 底下。
- **不該用**——對一個全新版本只發一個平台。這正是「陳舊條目斷言」設計要擋下
  的情境：另一平台的使用者會被通知「有新版本」，抓到的卻是舊檔案。真正安全的
  發法只有 `platform=all`，把兩個平台一起發掉。
- 這個開關 **只影響「保留條目」的斷言**，不影響這次有建置的平台——那些條目的
  `url` 是呼叫端用本次 tag 現組的，永遠不會觸發這條檢查（見
  `scripts/lib/latest-json.test.mjs` 的「陳舊條目」測試群組）。

`mergeLatestJson` 的 `options.tag` 是必填參數（缺省即 throw）：漏傳就等於整條
斷言靜默停用，而那正是這條防線存在的理由，所以刻意不給「忘記傳」留活路。

## 發佈管線：`SHA256SUMS.txt` 也要底稿合併，不能只用這次建置的檔案生成

跟上面 `latest.json` 同一類問題，出在另一個檔案上：`compose` job 過去只用
「這次建置、下載進 `out/` 的檔案」重新生成整份 `SHA256SUMS.txt`。
`softprops/action-gh-release` 上傳資產預設 overwrite——單平台補腿（例如
`v0.6.6` 已經發過 Windows，這次只補 macOS）時，這樣生出來的 `SHA256SUMS.txt`
只有 macOS 兩行，一旦蓋掉舊檔，Windows 那三個 `.exe` 的 checksum 就從 release
資產裡永久消失，而且整條流程全綠——這是一個**致命 bug**：`.exe` 本身還在，
卻再也沒有官方管道可以核對它們的完整性。

修法跟 `latest.json` 一樣是底稿合併（`scripts/lib/sha256sums.mjs` 的
`mergeSha256Sums`）：`compose` 先抓現行（這個 release tag 底下的）
`SHA256SUMS.txt` 當底稿，只用這次建置出來的檔案覆寫同名行，其餘行原樣保留。

**跟 `latest.json` 底稿抓取不同的一點**：`SHA256SUMS.txt` 的底稿網址用的是
「這次要發佈的 tag」本身（`releases/download/<tag>/SHA256SUMS.txt`），不是
`releases/latest` 那個浮動指標。`SHA256SUMS.txt` 沒有 `latest.json` 那種
「manifest 只有一個全域 `version`」的先天限制，不需要靠 `releases/latest`
對齊 updater endpoint；用浮動指標反而會在「要補腿的不是目前最新一個
release」時抓錯底稿。

**這兩份底稿刻意不保證來自同一個 release，這是正確設計，不要為了「看起來
要一致」而改掉**：`SHA256SUMS.txt` 是每個 release 各一份、跟這次的 tag
一對一對應的產物，底稿理所當然要用這次的 tag 去抓「這個 release 自己」的
內容；`latest.json` 是整個 repo 唯一一份、`releases/latest/download/
latest.json` 這個固定 endpoint，updater 只認這一份，底稿因此固定抓
`releases/latest`，跟這次是哪個 tag 無關。補腿到**目前最新的那個 tag**時
兩份底稿剛好同源（這次的 tag 本來就是 `releases/latest`指向的 release）；
補腿到**比較舊、不是目前最新**的 tag 時兩份底稿會刻意分岔：`SHA256SUMS.txt`
底稿仍抓那個較舊 tag 自己的內容（正確——不該被別的 release 污染），
`latest.json` 底稿仍抓 `releases/latest`（正確——它永遠只反映目前真正最新
的 release）。若把 `SHA256SUMS.txt` 的底稿網址也改成 `releases/latest`
以求「兩者同源」，會在補較舊 tag 的腿時抓到別的（更新的）release 的
`SHA256SUMS.txt` 當底稿，等於重新引入前面說的致命 bug。三種情境的邊界：

- **首發**：tag 對應的 release 還不存在，兩份底稿都是 404 → 空底稿，
  `SHA256SUMS.txt`／`latest.json` 都只有這次建置的內容。
- **補腿到最新 tag**（往已經發過、且目前就是 `releases/latest` 的 tag
  補另一個平台，最常見的補腿情境）：兩份底稿同源，都抓到同一個 tag 底下
  的既有內容，另一平台的 checksum／platforms 條目原樣保留。
- **補腿到較舊 tag**（往一個不是目前最新的舊 tag 補平台，較少見但合法）：
  兩份底稿刻意分岔（見上段），`SHA256SUMS.txt` 仍正確保留該舊 tag 自己的
  另一平台 checksum；`latest.json` 的底稿則是目前最新 release 的內容，跟
  這次要補的舊 tag 通常對不上，陳舊條目斷言（見上面的斷言 2）會視情況擋下
  或需要顯式 `allow_stale_platforms=true`。
- **全平台重發**（同一個 tag 重跑兩個平台）：底稿裡兩個平台的舊行都會被
  這次的新值覆寫，等於整份重新生成，但語意上仍然是「合併」而非「清空重建」。

`404` 與「暫時性失敗（網路抖動、5xx、rate limit）」的分野，以及重試邏輯，
跟 `latest.json` 底稿抓取完全一樣（見 `release.yml` 的兩個 `Fetch current
... as merge baseline` 步驟）：只有確定 404 才視為空底稿，其餘一律重試、
重試用盡就硬失敗——把暫時性失敗靜默當成空底稿，後果跟前面 `latest.json`
的陳舊條目斷言要擋的事故一樣：另一個平台的資產完整性驗證資訊被無聲抹掉。

### 殘餘風險：底稿合併不核對 release 實際的資產清單

`mergeSha256Sums` 只按「檔名」合併兩份清單，不會反過來核對底稿裡保留下來
的那些檔名，在這個 release 上是不是真的還有對應的資產存在。正常路徑下
（`softprops/action-gh-release` 用同一組檔名 overwrite）這不是問題——保留
的行本來就對應仍然存在的資產。但如果同一個 tag 被重新發過、而且檔名方案
本身也跟著改了（例如把 `traytunnel-<v>-setup.exe` 改名成別的規則、或整批
換掉某個平台的產物命名），底稿裡指向舊檔名的那一行會變成殭屍：`SHA256SUMS.
txt` 上留著一行雜湊，卻沒有對應資產可以下載核對。

失效方向是安全的：使用者拿這樣的 `SHA256SUMS.txt` 跑 `sha256sum -c` 時，
殭屍行的結果是「找不到那個檔案」（`No such file or directory`），不會是
「雜湊核對通過」的誤報——不會有人被騙去信任一個內容其實不對的檔案。目前
評估這個殘餘風險可以接受：換檔名方案本身就是低頻、需要人工同步改
`package.mjs`／`compose-latest-json.mjs` 的變更，屆時人工順手核對一次
`SHA256SUMS.txt` 內容即可；沒有為它加自動化的「跟 release 資產列表比對」
檢查。

### 附帶修好的既有缺陷：SHA256SUMS.txt 的換行字元

線上既有（`v0.6.5` 以前手刻 `sha256sum` 產出的）`SHA256SUMS.txt` 其實是
CRLF 換行——`scripts/lib/sha256sums.mjs` 的 `formatSha256Sums` 一律輸出
LF，這是本次順手修掉的一個既有小缺陷：CRLF 版本的 `SHA256SUMS.txt` 在
Linux／macOS 上直接 `sha256sum -c SHA256SUMS.txt` 會因為每一行多出的 `\r`
被當成檔名的一部分而報 `No such file or directory`，實際上並不能拿來驗證
（Windows 上用 `certutil` 之類工具通常不受影響，這是為什麼過去沒被發現）。
新管線輸出的 `SHA256SUMS.txt` 是純 LF，`sha256sum -c` 在三個平台上都能
正常核對。
