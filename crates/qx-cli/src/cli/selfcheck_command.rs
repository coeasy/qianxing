//! CLI adapters for the standalone self-check and smoke command family.

use super::*;

fn dispatch_output_result(result: Result<(), String>, command: &str) {
    if let Err(error) = result {
        eprintln!("[qx-cli · CLI] {command} 输出失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_default() {
    dispatch_output_result(selfcheck::run(selfcheck::Scope::Full, false), "自校验");
}

pub(super) fn dispatch_ecosystem(json: bool) {
    dispatch_output_result(run_ecosystem_smoke(json), "生态自检");
}

pub(super) fn dispatch_paper() {
    dispatch_output_result(run_paper_smoke(true), "Paper 冒烟");
}

pub(super) fn dispatch_all(json: bool) {
    dispatch_output_result(selfcheck::run(selfcheck::Scope::Full, json), "全量自检");
}

pub(super) fn dispatch_verify(json: bool) {
    dispatch_output_result(
        selfcheck::run(selfcheck::Scope::KernelOnly, json),
        "内核自检",
    );
}
