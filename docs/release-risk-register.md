# 發佈與應用內更新——已知風險登記簿

記錄兩類不是我們自己寫的 bug、卻會影響使用者的風險：上游 `tauri-plugin-updater`
在 macOS 提權路徑上的缺陷，以及發佈管線本身（`latest.json` 的 manifest 格式）
帶來的先天限制。兩者都已經在程式碼裡繞開或擋下，這裡集中記一份，避免下一個人
重新踩一次才想起來。

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
