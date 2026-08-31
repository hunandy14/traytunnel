//! 平台抽象層：所有「只有某一個作業系統才成立」的東西都收在這裡。
//!
//! 共用核心（config、state、ssh、wg、commands、lib……）一律只能寫
//! `crate::platform::xxx`，**不可以**直接碰 `platform::windows::xxx`——子模組
//! 刻意不是 `pub`，於是這條規則由可見性擋著，不是靠自律。想加一條跨平台的
//! 能力就在這裡多開一個 re-export，兩邊各補一份實作。
//!
//! 分派規則很單純：`cfg` 選一個子模組當 `imp`，本檔把 `imp` 的項目原名轉出去。
//! 因此「介面長什麼樣」就是下面這一份清單，兩個平台必須各自湊齊同名同簽章的項目，
//! 少一個就是編譯錯誤——這正是我們要的，不會有平台悄悄少實作一件事。
//!
//! 第四條規則（`docs/platform-guide.md` 的「三條規則」補的那一條）：**不看平台的
//! 純邏輯不准在 `windows/`、`macos/` 兩邊逐字複製**——那不是「這個平台怎麼做」，
//! 兩邊各抄一份只會漂，一改忘了改另一邊。上提到一個平台中立的位置（唯一呼叫端所在
//! 的共用核心檔案，或像 [`update_common`] 這樣的平台中立子模組），兩邊改成引用
//! 同一份；真的要碰系統 API 的那一小段（例如「用哪個 API 開瀏覽器」）還是留在
//! 各自的子模組，用函式指標之類的方式注入給共用邏輯，不要為了共用而把系統呼叫也
//! 硬套一份 cfg。`update_common` 與 `crate::appicon::pick_icon_layer`（原本
//! `sys.rs`／`winsys.rs` 各一份的挑層邏輯）是這條規則的兩個例子。

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(not(any(windows, target_os = "macos")))]
compile_error!("traytunnel 目前只支援 Windows 與 macOS，其餘平台請先補 src/platform/<os>");

// ---------------------------------------------------------------- 應用內更新
//
// 更新整條路（車道判定、背景檢查、暫存交棒、開 Releases 頁）與封裝格式綁得很死
// （Windows 是 NSIS setup.exe + HKCU 解除安裝機碼），因此整個子模組由平台提供。
// 對外的名字仍是 `platform::update::*`，呼叫端與搬移前一字不差。
//
// 這條路上不看平台的那一小段（逾時常數、release 網址組法、版本號比較）不受這條
// 規則管：那是純邏輯，兩邊硬要各自維護一份只會漂，收在平台中立的 [`update_common`]
// 一份，兩邊的 `update.rs` 各自 `use` 進來——見該檔開頭的說明。
mod update_common;
pub use imp::update;

// ---------------------------------------------------------------- 受監督 spawn
//
// 子程序不可以只殺自己：ssh 的 ProxyCommand 會再生出 cloudflared 之類的孫程序。
// 「怎麼把整棵樹綁在一起、怎麼避免主控台視窗一閃而過」是平台的事，
// 呼叫端只負責組命令列。
pub use imp::ProcessSupervisor;

// ---------------------------------------------------------------- 程序樹的收尾（僅 macOS）
//
// 這兩個名字**只有 macOS 有**，而且是刻意的不對稱（同 `build_menu` 那一段的
// 理由）：Windows 的 Job Object 帶著核心層級的 `KILL_ON_JOB_CLOSE`——handle
// 一關，不管行程是正常退出、當掉、被工作管理員結束還是登出，核心都會把整個
// job 收乾淨，使用者空間一行程式碼都不必跑。
//
// macOS 沒有等價物：`ProcessSupervisor` 的收尾是 `Drop`，那是使用者空間的
// 程式碼，被 SIGKILL／SIGTERM 帶走或當掉時一次都不會跑，於是 `ssh -N` 會變成
// 孤兒繼續握著 `-L` 的本地埠。差額由這兩支補：
//
// * `install_termination_handler`：掛 SIGTERM／SIGHUP／SIGINT，讓那些路徑也走
//   得到我們自己的收尾（呼叫端決定收尾要做什麼，這裡只負責「怎麼接住訊號」）。
// * `sweep_supervised_leftovers`：啟動時把上一輪被 SIGKILL／當機留下的程序樹
//   清掉——那一格沒有任何當下補救的辦法，只能事後收屍。
//
// Windows 編譯時這兩個名字根本不存在，呼叫端（`lib.rs`）也用同一個 cfg 包住，
// 不會有「呼叫了但 Windows 沒實作」這種事。
#[cfg(target_os = "macos")]
pub use imp::{install_termination_handler, sweep_supervised_leftovers};

// ---------------------------------------------------------------- GUI 啟動的 PATH（僅 macOS）
//
// 同樣是刻意的不對稱，而且理由跟上面那組一樣強：**Windows 沒有這個問題**。
// Windows 的 GUI 行程由 Explorer／登錄檔的 Run 值啟動，環境變數整份從使用者的
// session 繼承，`PATH` 與從 `cmd` 敲指令拿到的是同一份。macOS 的 launchd 只給
// GUI 行程一份最小集（`/usr/bin:/bin:/usr/sbin:/sbin`），使用者裝在
// `/opt/homebrew/bin` 的 `cloudflared`（ssh `ProxyCommand` 的預設值就在用它）
// 因此完全找不到。補一份 Windows no-op 只會讓人以為那邊也需要做這件事。
// 實作與「為什麼不用 `fix-path-env` 那顆 crate」都在 `platform::macos::sys`。
#[cfg(target_os = "macos")]
pub use imp::fix_gui_launch_path;

// ---------------------------------------------------------------- 本地埠偵測
//
// **兩平台的可見範圍不一樣，這是這組門面唯一沒有被抹平的差異**（WRP-1）：
//
// * Windows 讀的是 `GetExtendedTcpTable` 的**全系統** LISTEN 表，不管那個埠是
//   誰開的都看得見。
// * macOS 走 libproc 逐一問程序的 fd，只看得到**同一個 uid** 的程序。root
//   或別的使用者佔住的埠（例如 launchd 隨選啟動、uid 0 的 `sshd`）在沒有
//   root 權限時一律查不到，會回「沒人在聽」。
//
// 本專案自己會綁的監聽者（SOCKS5、我們 spawn 出來的 ssh）一律跟查詢者同一個
// uid，所以日常路徑不受影響；現形的情境是「別人的程序剛好佔住我們要用的埠」
// ——那時 mac 上會判成沒人佔、照樣 spawn，ssh 的 `-L` bind 失敗、
// `ExitOnForwardFailure` 讓它退出，狀態一路停在 RECONNECTING 而沒有 PORT_BUSY
// 提示。真正的補洞是把 ssh 的 stderr 接起來分流成 PORT_BUSY（backlog 的
// 「方案 A」），不是在這一層再想辦法——libproc 這條路徑本身就到這裡為止。
//
/// 本地是否有程序在該埠 Listen。連線判定與 spawn 前的埠檢查都靠它。
///
/// 可見範圍在兩個平台不同（macOS 只看得到同 uid 的程序），見上面那段說明。
pub use imp::is_listening;

/// 一次取回本機所有正在 TCP LISTEN 的埠號。
///
/// 給「一口氣要問很多個埠」的呼叫端（`wg::busy_rows`）用：語意與
/// [`is_listening`] 逐字相同（含上面那條 macOS 只看得到同 uid 的限制），
/// 差別只在成本——取一次快照再 `contains`，不必每個埠各跑一次全表走訪，
/// 而且那一批答案來自同一個瞬間。查表失敗一律回空集合，方向與
/// `is_listening` 回 `false` 一致。
pub use imp::listening_ports;

// ---------------------------------------------------------------- 時間
//
/// 本地時間的 `HH:mm:ss`，活動日誌每一行的時間戳。
pub use imp::local_time_hms;

// ---------------------------------------------------------------- 圖示與 DPI
//
// 系統匣與視窗圖示要的像素尺寸由系統決定（Windows 是 SM_CXSMICON／SM_CXICON，
// 且隨 DPI 變動），因此「要哪個尺寸」這兩支還是平台各自提供。「照著一組尺寸挑哪一
// 層」（`pick_icon_layer`）不看系統、不看平台，是純數字邏輯，兩邊原本各抄一份逐字
// 相同的實作——這種東西不該進這份「兩平台各自實作」的介面清單，已經上提到唯一的
// 呼叫端 `crate::appicon`，不再由這裡分派。
pub use imp::{large_icon_size, small_icon_size};

// ---------------------------------------------------------------- 系統匣圖示
//
/// 系統匣圖示，連同「這張圖是不是 template image」的旗標一起回。
///
/// 旗標與圖必須同源，這是這支門面存在的全部理由。舊做法在 `lib.rs::build_tray`
/// 裡分兩段 `cfg`：一段挑圖（macOS 先試 template PNG，解不開就退回彩色 ICO），
/// 另一段無條件 `icon_as_template(true)`。退路一旦踩到，彩色圖就被當成 template
/// 交給 AppKit——它只讀 alpha 通道重畫剪影，顏色整個丟掉，系統匣上是一團走樣的
/// 黑影。回傳 `(Image, bool)` 之後這個分岔在型別上就不成立了。
///
/// Windows 恆 `false`（沒有 template image 這個概念，圖示直接吃自己的顏色）；
/// macOS 只有真的解出 template PNG 那一條回 `true`。兩邊都在挑不到任何圖時回
/// `None`，呼叫端寧可先把系統匣建起來也不要 panic。
pub use imp::tray_icon;

// ---------------------------------------------------------------- 系統匣手勢文案
//
/// 通知裡「怎麼把視窗叫回來」那半句，只給動作、不含尾巴的
/// 「to reopen.」／「to open.」（呼叫端接）。
///
/// **與 `lib.rs::build_tray` 的點擊政策綁定**：Windows 是
/// `show_menu_on_left_click(false)` ＋ 左鍵雙擊開窗，所以文案講雙擊；macOS 依
/// D4 決議左右鍵一律開選單、沒有雙擊語意，文案因此指向選單裡的「Open window」
/// 項。兩者是同一件事的兩面，改一邊就要改另一邊——所以文案跟 `tray_icon` 一起
/// 住在各平台的 `trayicon` 子模組，而不是留在 `lib.rs` 手抄一組 `cfg` 常數。
pub use imp::TRAY_OPEN_GESTURE_HINT;

// ---------------------------------------------------------------- 開機自啟
//
// Windows 是 HKCU 的 Run 登錄項（外加工作管理員的 StartupApproved）。
// `read_autostart_command` 回的是目前登記的那一行命令，自癒靠它判斷還指不指得到
// 現在這支執行檔。
pub use imp::{autostart_enabled, disable_autostart, enable_autostart, read_autostart_command};

// ---------------------------------------------------------------- 系統通知
//
// `prepare_notifications` 是「讓系統認得我們的通知掛名」，必須在任何 UI 之前跑完，
// 回傳要補進活動日誌的行；`show_notification` 才是真的彈一顆出來。
pub use imp::{prepare_notifications, show_notification};

// ---------------------------------------------------------------- 設定檔位置
//
// `home_dir` 是家目錄模式的落腳處；可攜模式兩個觸發條件都在這裡：
// `stem_marks_portable` 是檔名記號，`exe_toml_marks_portable` 是「exe 旁已有
// traytunnel.toml」。兩支是不是都成立由平台自己決定——Windows 兩條都用，
// macOS 兩條都恆 false（W3 決議：macOS 不做可攜模式）。
pub use imp::{exe_toml_marks_portable, home_dir, stem_marks_portable};

// ---------------------------------------------------------------- 開外部程式
//
/// 在檔案管理員裡開啟並選中一個檔案。
///
/// 「用系統預設瀏覽器開網址」刻意**不在**這份介面裡：它唯一的呼叫端是更新那條路
/// （`update::open_release_page`／`open_releases_page`），而整個 update 子模組本來
/// 就是平台各自提供的，跨平台門面上再開一個沒有共用核心會用的洞只是多一個死角。
pub use imp::reveal_in_file_manager;

// ---------------------------------------------------------------- App 選單（僅 macOS）
//
// 「activation policy」「選單列上的 App／Edit／Window」是 macOS 獨有的概念，
// Windows 沒有對應語意（工作列圖示與視窗系統選單本來就是系統內建，不必自己組）。
// 這裡刻意不做「兩邊都要湊一份」的門面項目——整段 `#[cfg(target_os = "macos")]`，
// Windows 編譯時這兩個名字根本不存在，呼叫端（`lib.rs`）也用同一個 cfg 包住
// 呼叫的地方，不會有「呼叫了但 Windows 沒實作」這種事。`build_menu` 是三個
// 子選單（App／Edit／Window）的組裝；`MENU_QUIT_ID` 是自訂 Quit 項目的 id，
// 事件路由靠它辨認要不要呼叫 `do_exit`（原因見 `platform::macos::menu` 模組
// 開頭：`PredefinedMenuItem::quit` 直接 `exit(0)`，繞過我們的收尾）。
#[cfg(target_os = "macos")]
pub use imp::{build_menu, MENU_QUIT_ID};

// ---------------------------------------------------------------- Activation policy
//
// 「前景／背景」語意（Dock 圖示、選單列、Cmd+Tab 切換器要不要跟著視窗一起
// 出現／消失）只有 macOS 有——但跟上面 `build_menu` 那組不對稱門面不同，這裡
// 兩邊都湊了同名同簽章的項目：Windows 側是三支 no-op（工作列圖示本來就是
// 系統內建，沒有東西可切；實作在 `platform::windows::policy`），讓 `lib.rs`
// 的呼叫端（`show_main`／`hide_to_tray`／`setup`）不必為了平台差異各自包
// 一段 `#[cfg(target_os = "macos")]`。macOS 側的實作在
// `platform::macos::policy`（該模組說明有解釋為什麼這裡跟 `build_menu` 走
// 不同的門面風格）。跟本檔其他跨平台項目同款：一行 `pub use imp::{...}`
// 轉出去，不在門面這層另外分 cfg。
//
// `initial_policy_for_tray_start` 收 `&AppHandle`、而且在 `setup` 裡呼叫，
// 這兩件事本輪 review（M1）當成回歸查過又否掉了：唯一能在 launch 之前寫進
// tao aux state 的位置是 `Builder::build()` 與 `App::run()` 之間，搬過去確實
// 少掉啟動時那一格 `Foreground`，但會讓 tao 無條件執行的
// `activateIgnoringOtherApps` 生效並**永久搶走鍵盤焦點**。三個時機的
// lsappinfo／NSWorkspace 實測數據與結論全記在 `platform::macos::policy`
// 模組開頭，別再走一次。
pub use imp::{enter_foreground, initial_policy_for_tray_start, retire_to_tray};

// ---------------------------------------------------------------- 介面契約測試
//
// W3-A：`is_listening` 與 `ProcessSupervisor` 的跨平台不變量。掛在門面這一層而
// 不是各自的實作底下，是因為要測的正是「兩邊都必須成立的那些話」——放進任何一邊
// 都會變成在測那一邊的作法。整份不帶平台閘（只有「拿什麼命令生一支長睡程序」
// 需要 cfg 挑，斷言邏輯共用）。
#[cfg(test)]
#[path = "process_tests.rs"]
mod process_tests;
