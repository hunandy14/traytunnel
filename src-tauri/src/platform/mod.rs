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
// `home_dir` 是家目錄模式的落腳處；`stem_marks_portable` 是可攜模式的檔名記號。
pub use imp::{home_dir, stem_marks_portable};

// ---------------------------------------------------------------- 開外部程式
//
/// 在檔案管理員裡開啟並選中一個檔案。
///
/// 「用系統預設瀏覽器開網址」刻意**不在**這份介面裡：它唯一的呼叫端是更新那條路
/// （`update::open_release_page`／`open_releases_page`），而整個 update 子模組本來
/// 就是平台各自提供的，跨平台門面上再開一個沒有共用核心會用的洞只是多一個死角。
pub use imp::reveal_in_file_manager;
