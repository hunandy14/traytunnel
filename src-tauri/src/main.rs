// 避免 release 版跳出額外的主控台視窗，勿移除
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    traytunnel_lib::run()
}
