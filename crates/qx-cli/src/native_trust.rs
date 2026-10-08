//! P0-3 / DD-4：in-process 原生策略的信任门。
//!
//! `DynamicCAbiStrategy` 把原生库 `dlopen` 进宿主进程——库里的任何代码都直接拥有宿主的
//! 地址空间。方案 DD-4 的裁定是：**native 扩展默认只允许独立进程**（C++ worker 的共享内存
//! / JSONL 协议），in-process 仅在 `trusted_native` + 签名信任根 + 架构/平台匹配三样齐备时
//! 开启。本模块是这条判定在生产代码里的唯一落点：`load_c_abi_strategy` 在 `dlopen` 之前先问它，
//! 三样缺任何一样都当场拒，而不是留到原生代码已经在宿主里跑起来之后。

use super::*;

/// 宿主的目标三元组（`arch-os`，如 `x86_64-windows`），供 `c_abi_target_triple` 比对。
///
/// 只用 `std::env::consts` 里那两个编译期常量拼，不读环境变量、不跑外部命令：
/// 这个判定要的是"当前这份二进制是什么架构"，而不是"用户想让它是什么架构"。
pub(crate) fn host_target_triple() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// 三元组匹配：架构段逐字相等，且平台段作为 token 出现在期望三元组里。
///
/// 期望值允许写完整三元组（`x86_64-pc-windows-msvc`）或简写（`x86_64-windows`）：
/// 厂商段（`pc` / `unknown` / `apple`）不参与判定，因为宿主的 `std::env::consts`
/// 给不出厂商段，拿它当必填只会逼出一堆假拒。架构写错（`aarch64` vs `x86_64`）或平台
/// 写错（`linux` vs `windows`）都判不匹配——那正是"架构/平台不匹配的原生库进宿主"的入口。
pub(crate) fn target_triple_matches(expected: &str, host: &str) -> bool {
    let expected = expected.trim().to_ascii_lowercase();
    let host = host.trim().to_ascii_lowercase();
    let mut host_parts = host.split('-');
    let host_arch = host_parts.next().unwrap_or("");
    let host_os = host_parts.next().unwrap_or("");
    if expected.is_empty() || host_arch.is_empty() || host_os.is_empty() {
        return false;
    }
    let mut expected_parts = expected.split('-');
    expected_parts.next() == Some(host_arch) && expected_parts.any(|part| part == host_os)
}

/// 判定是否允许在宿主进程内加载这个 C ABI 策略。fail closed：任何一项不满足即拒。
pub(crate) fn admit_in_process_c_abi(strategy: &StrategyRuntimeConfig) -> Result<(), String> {
    if !strategy.c_abi_trusted_native {
        return Err(
            "in-process C ABI 策略未开启信任门：native 扩展默认只允许独立进程，\
             要在宿主进程内加载必须显式设置 strategy.c_abi_trusted_native=true，\
             并提供 Ed25519 签名信任根与 c_abi_target_triple（P0-3 / DD-4）"
                .to_string(),
        );
    }
    if strategy.c_abi_ed25519_public_key.is_none() || strategy.c_abi_ed25519_signature.is_none() {
        return Err(
            "in-process C ABI 策略缺少 Ed25519 签名信任根（c_abi_ed25519_public_key 与 \
             c_abi_ed25519_signature 必须成对配置）"
                .to_string(),
        );
    }
    let expected = strategy
        .c_abi_target_triple
        .as_deref()
        .ok_or_else(|| "in-process C ABI 策略缺少 c_abi_target_triple".to_string())?;
    let host = host_target_triple();
    if !target_triple_matches(expected, &host) {
        return Err(format!(
            "in-process C ABI 策略的目标三元组 {expected:?} 与宿主 {host:?} 不匹配，拒绝加载"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{admit_in_process_c_abi, host_target_triple, target_triple_matches};
    use qx_runtime::StrategyRuntimeConfig;

    #[test]
    fn host_triple_is_arch_dash_os() {
        let host = host_target_triple();
        assert_eq!(host.split('-').count(), 2);
        assert_eq!(host.split('-').next().unwrap(), std::env::consts::ARCH);
    }

    #[test]
    fn full_and_short_triples_match_the_same_host() {
        let host = "x86_64-windows";
        assert!(target_triple_matches("x86_64-pc-windows-msvc", host));
        assert!(target_triple_matches("x86_64-windows", host));
        assert!(target_triple_matches("X86_64-PC-WINDOWS-MSVC", host));
    }

    #[test]
    fn wrong_arch_or_platform_is_rejected() {
        let host = "x86_64-windows";
        assert!(!target_triple_matches("aarch64-pc-windows-msvc", host));
        assert!(!target_triple_matches("x86_64-unknown-linux-gnu", host));
        assert!(!target_triple_matches("", host));
        assert!(!target_triple_matches("x86_64-windows", ""));
    }

    #[test]
    fn admit_is_fail_closed_until_all_three_are_present() {
        // 从"只有库与摘要"起步，再逐格往上加——这条梯子读的是"缺哪一格就被拒"，
        // 所以初始两格用结构体字面量给出，避免 `Default::default()` 后再逐格赋值的写法。
        let mut strategy = StrategyRuntimeConfig {
            c_abi_library: Some("strategy.dll".into()),
            c_abi_sha256: Some("ab".repeat(32)),
            ..Default::default()
        };
        // 没开信任门 → 拒（默认拒绝）
        assert!(admit_in_process_c_abi(&strategy).is_err());
        strategy.c_abi_trusted_native = true;
        // 开了门但没有签名信任根 → 拒
        assert!(admit_in_process_c_abi(&strategy).is_err());
        strategy.c_abi_ed25519_public_key = Some("00".repeat(32));
        strategy.c_abi_ed25519_signature = Some("00".repeat(64));
        // 有签名但没有目标三元组 → 拒
        assert!(admit_in_process_c_abi(&strategy).is_err());
        strategy.c_abi_target_triple = Some(host_target_triple());
        // 三样齐备 → 放行
        assert!(admit_in_process_c_abi(&strategy).is_ok());
    }
}
