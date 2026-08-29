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

// ---------------------------------------------------------------- 本地埠偵測
//
/// 本地是否有程序在該埠 Listen。連線判定與 spawn 前的埠檢查都靠它。
pub use imp::is_listening;

// ---------------------------------------------------------------- 時間
//
/// 本地時間的 `HH:mm:ss`，活動日誌每一行的時間戳。
pub use imp::local_time_hms;

// ---------------------------------------------------------------- 圖示與 DPI
//
// 系統匣與視窗圖示要的像素尺寸由系統決定（Windows 是 SM_CXSMICON／SM_CXICON，
// 且隨 DPI 變動），挑層的規則本身則是共用邏輯的一部分。
pub use imp::{large_icon_size, pick_icon_layer, small_icon_size};

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

// ---------------------------------------------------------------- 介面契約測試
//
// W3-A：`is_listening` 與 `ProcessSupervisor` 的跨平台不變量。掛在門面這一層而
// 不是各自的實作底下，是因為要測的正是「兩邊都必須成立的那些話」——放進任何一邊
// 都會變成在測那一邊的作法。整份不帶平台閘（只有「拿什麼命令生一支長睡程序」
// 需要 cfg 挑，斷言邏輯共用）。
#[cfg(test)]
#[path = "process_tests.rs"]
mod process_tests;
