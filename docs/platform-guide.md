# 平台碼放哪裡

給要在 `src-tauri/` 動手的下一個人，一頁內講完規則。細節與取捨理由都寫在
`src-tauri/src/platform/mod.rs` 開頭的模組註解，這裡只整理成清單。

## 資料夾結構

```
src-tauri/src/platform/
├── mod.rs        # 唯一的對外門面，見下一節
├── windows/      # #[cfg(windows)] 才會被編譯
│   ├── mod.rs
│   ├── update.rs # 應用內更新（NSIS 暫存交棒）
│   └── ...
└── macos/        # #[cfg(target_os = "macos")] 才會被編譯
    ├── mod.rs
    ├── update.rs # 應用內更新（.app bundle 原地替換）
    └── ...
```

`platform/mod.rs` 用 `cfg` 選一個子模組當 `imp`，把 `imp` 的項目原名轉出去；子模組
本身刻意 **不是 `pub`**。這代表兩件事：

- 「介面長什麼樣」就是 `mod.rs` 那份 `pub use imp::...` 清單。兩個平台必須各自
  湊齊同名同簽章的項目，少一個就是編譯錯誤——這是刻意的，不會有平台悄悄少做一件事。
- 目前只有 Windows／macOS 兩個 `cfg` 分支；`compile_error!` 擋住第三個平台，
  真的要加（例如 Linux）得先開一個新的 `platform::linux` 子模組再解開這道擋。

## 共用核心的規則

共用核心（`config`、`state`、`ssh`、`wg`、`commands`、`lib` 等）**一律只能寫
`use crate::platform::xxx`**，不可以直接碰 `platform::windows::xxx` 或
`platform::macos::xxx`——子模組不是 `pub`，這條規則由可見性擋著，不是靠自律，
違反了根本編譯不過。

需要跨平台都能用的一支函式／型別，就在 `platform/mod.rs` 裡多開一個
`pub use imp::xxx`，兩邊各補一份實作；不需要跨平台共用的東西（例如某個平台特有的
內部細節）就留在子模組裡，不要為了「以防萬一」也搬到門面上。

## 新增一項 OS 相依功能的流程

1. 決定這項功能要不要進共用門面。只有一個平台用得到、共用核心也不會呼叫，
   就留在對應子模組內部，不必動 `mod.rs`。
2. 需要跨平台呼叫的話：先在 `mod.rs` 決定簽章（參數、回傳型別要兩邊通用），
   在清單裡加一行 `pub use imp::your_fn`。
3. 兩個子模組（`windows/`、`macos/`）都要補齊同名同簽章的實作，順序不拘，
   但兩邊都沒補齊之前不會過編譯——這正是用來抓「忘記另一腿」的機制。
4. 純邏輯（不碰檔案系統／系統 API 的部分）盡量拆成純函式，用一般 `#[test]`
   釘住；真的要碰系統資源才走下一節的 live 測試慣例。
5. 兩腿都綠、`cargo fmt`／`cargo clippy` 都過，才算完工——見下一節，CI 本來
   就會逐一擋。

## CI 雙腿守門的意義

`.github/workflows/ci.yml` 的 build matrix 在 `windows-latest` 與 `macos-latest`
（`fail-fast: false`，兩腿互不相依）各自跑一次完整的 build＋clippy＋test；下游有
一個彙總 job 叫 `ci`（branch protection 綁的正是這個名字，不是 matrix 展開後的
`build (windows-latest)`／`build (macos-latest)`）。

雙腿分開跑不是形式：`#[cfg(windows)]`／`#[cfg(target_os = "macos")]` 意味著
「只在另一個平台上才會被編譯」的程式碼，本機單一平台的 `cargo build`／
`cargo test` 完全看不到它有沒有壞掉。合併前沒有雙腿都綠，代表另一個平台的
子模組可能已經是編譯錯誤——CI 是唯一會真的把它建出來的地方。

## live 測試慣例（`#[ignore]` + 環境變數）

會碰真實系統資源的測試（寫真的 `~/Library/LaunchAgents`、`launchctl load`、
真的 SSH／WireGuard 連線……）一律 `#[ignore]`，預設測試輪（`cargo test`、CI）
不會跑到，只有手動指定測試名稱前綴時才會跑，例如：

```
cargo test --lib -- --ignored --nocapture live_autostart
```

需要外部設定（伺服器位址、`.conf` 路徑等）的，一律從環境變數讀，變數名前綴
`TRAYTUNNEL_TEST_`（例如 `TRAYTUNNEL_TEST_WG_CONF`），測試碼裡不寫死任何路徑，
也不去翻 `secrets/`；環境變數沒設就印一行說明並跳過，不算測試失敗。收尾要把
自己建立的狀態清乾淨（例如測試名稱帶 `std::process::id()` 避免撞到使用者真正
的登記項），但 assert 失敗仍可能跳過收尾——這正是這類測試不准留在預設測試輪的
理由，見 `src-tauri/src/wg_live_tests.rs`、`src-tauri/src/platform/macos/sys.rs`
的 `live_autostart_round_trips_through_launchd`。

## 驗 UI 一定要用 `tauri build` 的產物（白屏最常見的假警報）

`cargo build` 產出的執行檔**不會內嵌前端**，它載的是 `tauri.conf.json` 的
`build.devUrl`（`http://localhost:1420`）。單獨把它跑起來、旁邊沒有
`npm run dev`，視窗會照常開出來、macOS 的紅綠燈也照常畫，但 webview 內容
是完全空白的一片白——**這是 dev 產物的正常行為，不是 app 的 bug**。
README 的「建置」章節本來就寫了這件事，這裡再記一次是因為它在 macOS 上
特別容易被誤判成視窗風格或 activation policy 的問題（實際量測：`cargo build`
的執行檔單獨啟動 17/17 全白；`tauri build` 的 `.app` 70/70 全部正常）。

驗 macOS UI 只有兩條路：`npm run tauri dev`（Vite 在跑），或
`npx tauri build [--debug] --bundles app` 之後跑 `.app` 裡的執行檔。

分辨方法不必再靠截圖猜，日誌第一段就有（見 `lib.rs` 的 `watch_first_page_load`）：

```
main webview url: tauri://localhost        <- tauri build 的產物，內嵌前端
main webview url: http://localhost:1420/   <- cargo build 的產物，要 Vite 才有畫面
```

真的載不到時五秒後還會多一行 `the main webview has not finished loading ...`
的 warn。使用者回報白屏時，`traytunnel.log` 要先看這兩行，再看
`webview content process terminated`（那是另一條成因，見 `lib.rs` 的
`on_web_content_process_terminate`）。
