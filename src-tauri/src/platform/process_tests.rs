//! W3-A：程序管理門面的跨平台契約測試——`platform::is_listening` 與
//! `platform::ProcessSupervisor` 的不變量（§1／§2）；F3 車道起，順帶收留其餘
//! 「兩平台原本各自測試模組逐字重複」的門面契約（§3：`local_time_hms`、
//! `small_icon_size`／`large_icon_size`），不必為了兩三支測試另開一個檔案。
//!
//! 這一份刻意掛在 `platform/mod.rs`（門面）底下，而不是塞進任何一邊的實作：
//! 契約是「兩個平台都得成立的那些話」，寫在門面這一層才擋得住「某一邊悄悄
//! 少做一件事」。因此**整份不帶任何平台閘**——`#[cfg]` 只出現在「拿什麼命令
//! 去生一支會睡很久的程序」這件事上（Windows 沒有 `sh`，macOS 沒有 `cmd`），
//! 斷言邏輯本身兩平台一字不差。
//!
//! ## 競態紀律
//!
//! 程序的生與死、埠的開與關都是非同步事件：作業系統不保證「我這一行呼叫回來時
//! 它已經生效」。所以這裡沒有任何一條是「固定睡幾秒再問一次」——一律是
//! **輪詢＋期限**（等一件事發生）或**觀察窗**（確認一件事在一段時間內都沒發生）。
//! 期限給得很寬，CI runner 慢一點也不會變成偶發紅燈；期限不是效能指標，
//! 只是「不要無限等下去」的上限。
//!
//! ## 孫程序怎麼觀測
//!
//! 防孤兒那一條的重點是**孫程序**：ssh 的 ProxyCommand 會再生出 cloudflared，
//! 只殺直接子程序等於留下一個握著埠不放的孤兒。要跨平台判斷「孫程序也死了」，
//! 這裡用的是管道的 EOF：子程序與孫程序繼承同一支 stdout 寫端，而讀端只有在
//! **所有**寫端都關閉時才會收到 EOF。於是「read 讀到 EOF」精準等價於
//! 「整棵樹都不在了」，不必去問任何一個作業系統「這個 pid 還活著嗎」。

use std::net::TcpListener;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use super::{is_listening, large_icon_size, local_time_hms, small_icon_size, ProcessSupervisor};

/// 等一件「應該會發生」的事的上限。給得很寬：CI runner 上程序建立與埠狀態
/// 更新都可能慢上一個數量級，這個數字只負責讓測試不要無限掛住。
///
/// `pub(super)`：macOS 的 `sys::tests::a_wildcard_listener_is_visible` 也借用
/// 這個期限與下面的 [`poll_until`]，不必自己另外手刻一份輪詢迴圈——那支測試查
/// 的是 wildcard 位址這個 macOS 專屬的額外語意，不屬於兩平台都要成立的契約，
/// 所以留在它自己的模組，只是輪詢機制跟這裡共用。
pub(super) const DEADLINE: Duration = Duration::from_secs(20);

/// 輪詢間隔。夠密才不會把「已經生效」誤記成「花了很久」，夠疏才不會空轉燒 CPU。
const TICK: Duration = Duration::from_millis(50);

/// 確認一件「不應該發生」的事沒發生的觀察窗。
/// 誤殺與早退都是一發生就發生，撐過這段時間基本上就是沒發生。
const QUIET_WINDOW: Duration = Duration::from_secs(3);

/// 受測程序睡多久。只要遠大於 `DEADLINE` 就行；不寫成天長地久是留一道保險——
/// 實作真的沒收乾淨時，殘留的程序自己會在兩分鐘內散掉，不會卡在機器上。
const SLEEP_SECONDS: &str = "120";

/// 輪詢到條件成立，或到期限為止。回 false 就是逾時。
///
/// `pub(super)`：理由同 [`DEADLINE`]。
pub(super) fn poll_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + deadline;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(TICK);
    }
}

/// 向作業系統要一個當下沒人佔的埠：綁 0 讓系統配發，讀出實際埠號後把
/// listener 收掉。測試裡一律這樣拿埠，不寫死任何埠號——寫死的埠在別人的機器上
/// 遲早會撞到真的有東西在聽。
fn borrow_a_free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("要向 OS 借得到一個 ephemeral 埠");
    listener.local_addr().expect("listener 一定有本地位址").port()
}

/// 一支活很久、什麼都不印的程序。
///
/// `#[cfg]` 只挑「命令長什麼樣」，因為 Windows 上沒有 `sh`、macOS 上沒有 `cmd`；
/// 底下所有斷言都不分平台。
fn a_long_sleeper() -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep", "-Seconds"])
            .arg(SLEEP_SECONDS);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!("sleep {SLEEP_SECONDS}"));
        cmd
    }
}

/// 一支會再 spawn 出孫程序的程序：直接子程序是 shell，真正在睡的是孫程序。
///
/// 兩支都繼承同一支 stdout 寫端，於是讀端的 EOF 等價於「兩支都死了」。
/// 只殺直接子程序的實作在這裡會露餡：shell 沒了，孫程序還握著寫端，EOF 不會來。
fn a_long_sleeper_behind_a_shell() -> Command {
    #[cfg(windows)]
    {
        // `cmd /C` 沒有 exec 這回事，它一定會另外開一支 powershell 當孫程序，
        // 而且會一直等到那支結束才自己退
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "powershell", "-NoProfile", "-NonInteractive", "-Command", "Start-Sleep"])
            .arg("-Seconds")
            .arg(SLEEP_SECONDS);
        cmd
    }
    #[cfg(not(windows))]
    {
        // `&` 是關鍵：不加的話 sh 會直接 exec 掉自己變成 sleep，就只剩一層，
        // 測不到孫程序。丟到背景再 `wait`，sh 與 sleep 就是穩定的兩層，
        // 而且 sh 還活著就代表 `wait` 還沒回來、也就代表 sleep 還活著。
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!("sleep {SLEEP_SECONDS} & wait"));
        cmd
    }
}

/// 在觀察窗內反覆確認子程序沒被收掉；中途退了就回 false。
async fn stays_alive(child: &mut Child, window: Duration) -> bool {
    let end = Instant::now() + window;
    while Instant::now() < end {
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => {
                eprintln!("子程序在觀察窗內就退了：{status}");
                return false;
            }
            Err(e) => panic!("try_wait 不該失敗：{e}"),
        }
        tokio::time::sleep(TICK).await;
    }
    true
}

// ------------------------------------------------------------------
// 規格 §1：is_listening
// ------------------------------------------------------------------

/// §1(i)：綁了一個 TcpListener 的埠必須回 true。
///
/// 用 OS 配發的 ephemeral 埠（bind 0 再讀實際埠號），不寫死。
/// 綁定到「掃描得到」之間可能隔著一個核心表的更新，因此是輪詢＋期限，
/// 不是綁完就當場問一次。
#[test]
fn a_bound_loopback_port_is_reported_as_listening() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("要綁得起來");
    let port = listener.local_addr().expect("listener 一定有本地位址").port();

    let seen = poll_until(DEADLINE, || is_listening(port));
    // listener 撐到斷言之後才放掉：提早 drop 的話就變成在測一個已經關掉的埠
    drop(listener);

    assert!(
        seen,
        "127.0.0.1:{port} 上有 TcpListener 在 LISTEN，is_listening 卻在 {DEADLINE:?} 內都沒回 true"
    );
}

/// §1(ii)：listener 關掉之後必須回 false。
///
/// 「關掉」到「掃描不到」不保證同一個 tick 生效，所以給期限輪詢；
/// 但期限內一定要翻面，否則 tunnel 的 port_busy 判斷會一直誤報佔用、
/// 重接永遠卡在 5 秒一輪的重試。
#[test]
fn a_closed_port_stops_being_reported_as_listening() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("要綁得起來");
    let port = listener.local_addr().expect("listener 一定有本地位址").port();

    assert!(
        poll_until(DEADLINE, || is_listening(port)),
        "前提不成立：埠 {port} 綁上之後就該看得到 LISTEN"
    );

    drop(listener);

    assert!(
        poll_until(DEADLINE, || !is_listening(port)),
        "listener 已經關掉，is_listening({port}) 卻在 {DEADLINE:?} 內都還回 true"
    );
}

/// §1(iii)：沒人佔的埠回 false。
///
/// 先等它到期限內回 false（實作若有快取或殘留，這裡就會逾時），
/// 再用一段觀察窗確認它是**穩定地**假，而不是剛好被問到的那一下才是假。
#[test]
fn a_port_nobody_holds_is_not_reported_as_listening() {
    let port = borrow_a_free_port();

    assert!(
        poll_until(DEADLINE, || !is_listening(port)),
        "沒有任何程序佔著埠 {port}，is_listening 卻在 {DEADLINE:?} 內一直回 true"
    );

    let end = Instant::now() + QUIET_WINDOW;
    while Instant::now() < end {
        assert!(!is_listening(port), "沒人佔的埠 {port} 在觀察窗內冒出了一次 true");
        std::thread::sleep(TICK);
    }
}

// ------------------------------------------------------------------
// 規格 §2：ProcessSupervisor
// ------------------------------------------------------------------

/// §2(i)：supervisor 消失後，它 spawn 的子程序必須在期限內死掉。
///
/// 這是 halt／restart／關掉程式時「不留孤兒 ssh」的地基：`state::Worker::Ssh`
/// 只是把 supervisor 抱著不動，真正動手的是它的 Drop。
#[tokio::test]
async fn a_supervised_child_dies_after_the_supervisor_is_dropped() {
    let supervisor = ProcessSupervisor::new().expect("ProcessSupervisor::new 要成功");

    let mut cmd = a_long_sleeper();
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = supervisor.spawn(&mut cmd, "w3a: ").expect("受監督的 spawn 要成功");

    assert!(
        stays_alive(&mut child, QUIET_WINDOW).await,
        "前提不成立：子程序在 supervisor 還活著的時候就退了，後面的斷言會白過"
    );

    drop(supervisor);

    let exited = tokio::time::timeout(DEADLINE, child.wait()).await;
    assert!(exited.is_ok(), "supervisor 已經 drop，子程序卻在 {DEADLINE:?} 內都還活著——這就是孤兒");
}

/// §2(ii)：子程序再生出來的**孫程序**也必須一起死——防孤兒的核心。
///
/// 對應真實情境：ssh 的 ProxyCommand 會再拉起 cloudflared。只殺 ssh 的話，
/// cloudflared 留著繼續握住資源，下一輪重接就撞到自己上一輪的殘骸。
///
/// 判定不去問任何平台的「pid 還在嗎」，而是看繼承下去的 stdout 管道有沒有 EOF：
/// 只要還有任何一支後代握著寫端，read 就不會結束。
#[tokio::test]
async fn a_grandchild_dies_with_the_supervisor_as_well() {
    let supervisor = ProcessSupervisor::new().expect("ProcessSupervisor::new 要成功");

    let mut cmd = a_long_sleeper_behind_a_shell();
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = supervisor.spawn(&mut cmd, "w3a: ").expect("受監督的 spawn 要成功");
    let mut pipe = child.stdout.take().expect("測試自己設了 stdout(piped)，這裡一定拿得到");

    // 前提：兩層都起來了。shell 還活著就代表它還在等那支孫程序，
    // 也就代表孫程序確實存在——這一段只是等前提成立，不是被測的行為。
    assert!(
        stays_alive(&mut child, QUIET_WINDOW).await,
        "前提不成立：shell 在 supervisor 還活著的時候就退了，這一輪根本沒有孫程序可測"
    );

    drop(supervisor);

    let mut drained = Vec::new();
    let closed = tokio::time::timeout(DEADLINE, pipe.read_to_end(&mut drained)).await;
    assert!(
        closed.is_ok(),
        "supervisor 已經 drop，卻還有後代握著繼承來的 stdout（{DEADLINE:?} 內等不到 EOF）——孫程序變成孤兒了"
    );

    // 直接子程序也該一起走；跟上一條同樣的道理，這裡再收一次確認整棵樹都清乾淨
    let exited = tokio::time::timeout(DEADLINE, child.wait()).await;
    assert!(exited.is_ok(), "整棵樹的寫端都關了，直接子程序卻還沒退");
}

/// §2(iii)：supervisor 還活著的期間，正常運行的子程序不可以被誤殺。
///
/// 反面同樣是規格：把「不留孤兒」做成「動不動就整棵砍掉」，代價是連線隨機斷。
/// 這裡用觀察窗反覆確認，不是睡一段時間之後問一次。
#[tokio::test]
async fn a_running_child_is_left_alone_while_the_supervisor_lives() {
    let supervisor = ProcessSupervisor::new().expect("ProcessSupervisor::new 要成功");

    let mut cmd = a_long_sleeper();
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = supervisor.spawn(&mut cmd, "w3a: ").expect("受監督的 spawn 要成功");

    assert!(
        stays_alive(&mut child, QUIET_WINDOW).await,
        "supervisor 還活著，正常運行中的子程序不可以被收掉"
    );

    // 收尾：測完不留東西在機器上跑
    drop(supervisor);
    let _ = tokio::time::timeout(DEADLINE, child.wait()).await;
}

// ------------------------------------------------------------------
// 規格 §3：與程序管理無關，但兩平台原本逐字重複的其餘契約測試
// ------------------------------------------------------------------
//
// 這兩支跟 §1／§2 一樣不帶任何平台閘，但測的不是程序或埠，而是
// `platform::local_time_hms` 與 `platform::{small_icon_size,large_icon_size}`
// 這兩組門面函式：兩邊分別在 `macos::sys` 與 `windows::winsys` 的測試模組裡
// 各自抄了一份、逐字相同（連斷言帶訊息都一樣，`metrics_are_sane_on_this_machine`
// 只有一句失敗訊息的措辭不同，已收斂成不偏袒任一平台的版本），這裡沿用本檔
// 「兩平台都必須成立的話收在門面這一層」的同一個理由，一併搬進來，兩邊各自
// 的副本已刪除。

/// §3(i)：時間戳的形狀就是日誌行的格式契約：固定八個字元的 HH:mm:ss。
#[test]
fn local_time_is_a_fixed_width_hms() {
    let ts = local_time_hms();
    assert_eq!(ts.len(), 8, "{ts}");
    let parts: Vec<&str> = ts.split(':').collect();
    assert_eq!(parts.len(), 3, "{ts}");
    let bounds = [24, 60, 60];
    for (p, max) in parts.iter().zip(bounds) {
        assert_eq!(p.len(), 2, "每段都要補到兩位：{ts}");
        assert!(p.parse::<u32>().unwrap() < max, "{ts}");
    }
}

/// §3(ii)：這台機器兩種圖示尺寸的合理性——大圖示不會比小圖示小，也不會是 0，
/// 兩者都該是正方形。
#[test]
fn metrics_are_sane_on_this_machine() {
    let (sw, sh) = small_icon_size();
    let (lw, lh) = large_icon_size();
    assert_eq!(sw, sh, "小圖示應為正方");
    assert_eq!(lw, lh, "大圖示應為正方");
    assert!(sw >= 16 && lw >= 32, "small={sw} large={lw}");
    assert!(lw >= sw && lh >= sh, "大圖示不該小於小圖示");
}
