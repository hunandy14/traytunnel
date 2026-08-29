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

## 平台碼歸屬規則

四條規則，外加前端現況、資料夾長大的階梯，一次講完「平台碼該放哪裡」這件事在
多大範圍內都成立。

### 四條規則

1. 共用核心只准寫 `use crate::platform::xxx`，不可以直接碰
   `platform::windows::xxx`／`platform::macos::xxx`——子模組刻意不是 `pub`，這條
   規則由可見性鎖死，不是靠自律，違反了根本編譯不過（細節見上一節）。
2. `platform/` 只放「怎麼做」（OS 機制：呼叫哪個系統 API、用什麼格式交棒），不放
   「做什麼」（業務邏輯：這項功能要不要做、對使用者代表什麼意思）——後者一律留在
   共用核心。這條界線一旦鬆動，平台夾很容易長成第二套業務邏輯，同一條規則被兩邊
   各抄一份，之後只會各自漂移、越改越對不齊。
3. CI 雙平台腿守門：`windows-latest`／`macos-latest` 各自完整
   build＋clippy＋test，平台碼放錯位置——例如該進 `platform/` 的東西留在共用核心
   裡用 `cfg` 硬撐——本機單一平台的 build／test 往往看不出來，另一腿的 CI 才會
   紅燈（見下文「CI 雙腿守門的意義」）。
4. **`windows/` 與 `macos/` 兩邊都要做的事不等於「純邏輯也要各抄一份」**：不碰
   系統 API、只是字串／數字運算的那一小段（逾時常數、網址組法、版本比較、
   「照一組尺寸挑哪一層」這類挑層演算法……）上提到一個平台中立的位置——唯一
   呼叫端所在的共用核心檔案（例如 `crate::appicon` 收了兩邊都用的
   `pick_icon_layer`），或像 `platform/update_common.rs` 這樣的平台中立子模組，
   兩邊改成引用同一份。真的要碰系統 API 的部分（例如「用哪個 API 開瀏覽器」）
   還是留在各自子模組，用函式指標之類的方式注入給共用邏輯，不要為了共用而把
   那一小段系統呼叫也硬套一份 `cfg`。判斷準則很單純：改一份會不會忘記改另一份
   ——會的話就是這條規則要擋的重複。

### 前端目前的位置，與長大之後的升級路徑

前端現在只有一處平台碼：`index.html` 的 `data-platform` 屬性（`vite.config.ts` 的
`htmlPlatformPlugin` 在建置期依 `process.platform` 寫入）配 `styles.css` 的
`[data-platform="macos"]` CSS selector。這是單一一條 CSS 分歧點，平台碼佔比遠低於
5%，目前的檔位就是對的，不必為此開資料夾。

如果前端真的長出**平台邏輯**（不只是 CSS 分歧，而是不同平台要跑不同 TS
程式碼），升級走兩步，不要一步跳到底：

1. 先用檔名字尾區分，例如 `foo.macos.ts`／`foo.windows.ts`（仿 React Native、.NET
   的平台檔慣例），建置工具依平台各選一份編譯進去。
2. 等這類檔案多到需要一層共用抽象（例如統一介面、多個模組都要分流）才成立
   `src/platform/` 小夾，對齊 `src-tauri/src/platform/` 現有的做法。

跳過第一步直接開資料夾，在只有一兩個檔案分歧的階段只會多一層空目錄。

### 演化階梯

平台碼隨規模長大會踩過幾個台階，本專案現在站在第三階：

1. **行內 `cfg`**：一兩處分歧，直接 `#[cfg(...)]` 包在原本的檔案裡。
2. **平行單檔**：分歧多到一個檔案塞不下，拆成 `foo_windows.rs`／`foo_macos.rs`
   兩份平行檔案。
3. **`platform/` 資料夾（本專案現位）**：分歧橫跨多個模組，子模組化＋門面收斂，
   見上文「資料夾結構」。
4. **獨立 crate**：升到這一階有明確觸發條件，不是規模到了就自動升——需要混
   Swift／Objective-C 之類的原生互操作（自己的建置工具鏈、FFI 邊界），或平台碼
   佔比已經明顯膨脹到快要蓋過共用核心，才值得把 `platform/` 整層抽成獨立 crate。

## 新增一項 OS 相依功能的流程

1. 決定這項功能要不要進共用門面。只有一個平台用得到、共用核心也不會呼叫，
   就留在對應子模組內部，不必動 `mod.rs`。共用核心**確實**要呼叫、但另一個平台
   沒有對應語意時，門面上開一個整段 `#[cfg(target_os = "...")]` 的項目，
   不要為了對稱去湊一份假的實作；呼叫端用同一個 `cfg` 包住即可（現有例子：
   `build_menu`／`MENU_QUIT_ID`、`install_termination_handler`／
   `sweep_supervised_leftovers`，三者都只有 macOS 有）。
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

會碰真實系統資源的測試（寫真的 `~/Library/LaunchAgents`、真的 SSH／WireGuard
連線……）一律 `#[ignore]`，預設測試輪（`cargo test`、CI）
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
的 `live_autostart_round_trips_through_the_launch_agents_folder`。

會寫進使用者資料夾的**產品程式碼**也適用同一條規則的另一半：測試輪跑得到的路徑
不准去動真實位置。`platform/macos/pgids.rs` 的 `registry_path()` 在 `cfg(test)`
下直接回 `None`（登記簿整個變 no-op），因為 `platform::process_tests` 會真的建
`ProcessSupervisor` 並 spawn 子程序；登記簿本身的行為改用 `*_at(path)` 系列打
tempdir 測，涵蓋率不打折。新增任何「會寫檔的平台功能」時照這個形狀做。

## 驗 UI 一定要用 `tauri build` 的產物（白屏最常見的假警報）

`cargo build` 產出的執行檔**不會內嵌前端**，它載的是 `tauri.conf.json` 的
`build.devUrl`（`http://localhost:1420`）。單獨把它跑起來、旁邊沒有
`npm run web:dev`，視窗會照常開出來、macOS 的紅綠燈也照常畫，但 webview 內容
是完全空白的一片白——**這是 dev 產物的正常行為，不是 app 的 bug**。
README 的「建置」章節本來就寫了這件事，這裡再記一次是因為它在 macOS 上
特別容易被誤判成視窗風格或 activation policy 的問題（實際量測：`cargo build`
的執行檔單獨啟動 17/17 全白；`tauri build` 的 `.app` 70/70 全部正常）。

驗 macOS UI 只有兩條路：`npm run dev`（完整 app，Vite 在跑），或
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

macOS 上這五秒逾時不只寫日誌：確認是 `build.devUrl` 又真的沒有任何 page load
時，還會把空白的 webview `navigate` 到一個內嵌深色底、繁體中文說明的 `data:`
URL（見 `lib.rs` 的 `DEV_BUILD_NOTICE_HTML`），所以裸執行檔不會一直維持完全
空白——五秒內仍是白屏，五秒後會換成那頁說明；`tauri build` 的正式產物走
`tauri://localhost`，不符合這個 URL 判斷，畫面不受影響。

這裡刻意選 `navigate` 而不是 `eval`：實測過 `eval`／`eval_with_callback`，兩者
在連線被拒絕的這個情境下完全沒有作用——wry 的 WKWebView 後端把
`evaluateJavaScript` 呼叫閘在一個 `pending_scripts` 佇列後面，只有
`didCommitNavigation` 才會把佇列送進 webview 真正執行；`http://localhost:1420`
連不上是在 provisional navigation 階段就失敗，從來不會走到
`didCommitNavigation`，佇列裡的 script 永遠卡在排隊狀態，`eval` 形同沒打中。
`navigate` 是全新的一次導航請求，不吃這個佇列，`data:` URL 也不需要任何網路
連線就能被直接當成一份完整文件載入。

## scripts 命名規則

`package.json` 的 script 鍵名是一棵樹，`:` 是階層分隔，不是扁平字串：

```
build
├── dist          build:dist        當前平台完整發佈建置＋打包，本機等價於 CI 走法
├── mac           build:mac         單一變體，停在兩層
└── win
    ├── exe       build:win:exe     只編免安裝執行檔，跳過打包
    └── setup     build:win:setup   執行檔＋NSIS 安裝檔

dev               完整 app（tauri dev，含 Rust 編譯與 webview）
web:dev           純前端（vite）
web:build         純前端（tsc && vite build）
web:preview       純前端（vite preview）
```

規則：

- **冒號＝層級**，不是命名裝飾。讀 `build:win:exe` 要當「`build` → `win` →
  `exe`」三層路徑讀，不是一個扁平的識別碼。
- **單一變體的平台停在兩層**：macOS 目前只有一種建置產物，鍵名就是
  `build:mac`，不必為了「將來可能有第二個變體」預先開第三層占位。等它真的
  長出第二個變體（例如公證版）才升到 `build:mac:app`／`build:mac:notarized`
  三層——沒有第二個變體之前硬升一層，只是多一層沒人用的空殼。Windows
  從一開始就有 `exe`／`setup` 兩個變體，所以它直接是三層
  `build:win:exe`／`build:win:setup`。
- **`dev` 與 `web:*` 是兩個不同的心智模型，不是同一件事的兩種寫法**：
  `dev`（`tauri dev`）啟動完整 app，含 Rust 編譯與 Tauri IPC，驗證含後端邏輯
  的完整行為要用它；`web:dev`／`web:build`／`web:preview` 只碰前端，不啟動
  Rust 那一側，只想調 UI、不想每次都等 Rust 編譯時用它們。
- **刻意不設裸 `build`**：光看 `npm run build` 猜不出是要建前端還是建整個
  app。拿掉之後，打錯字或沿用舊記憶敲 `npm run build` 會直接得到明確的
  `missing script` 錯誤，逼你在 `web:build`、`build:dist`、`build:win:exe`、
  `build:win:setup`、`build:mac` 之間選一個實際存在的鍵，不會誤觸一個「看似
  合理但做錯事」的指令。
- **`build:dist` 是 CI 建置的本機等價流程，不是 CI 實際呼叫的指令**：
  `.github/workflows/release.yml` 的 build job 不跑 `npm run build:dist`，
  而是直接呼叫 `tauri build --target <matrix.rust_target>` + `node
  scripts/package.mjs --target <matrix.rust_target>`——多帶了明確的
  `--target`，這樣 `platform_key` 才能從 target triple 推導，不必用
  runner 架構猜。本機沒有多平台 matrix 好帶，`build:dist` 因此省略
  `--target`，`package.mjs` 退回用 `process.platform`／`process.arch` 猜，
  兩者邏輯一致、只差這個旗標。
