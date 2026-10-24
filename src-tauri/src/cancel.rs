//! # 请求取消管理
//!
//! 提供按 `window_id` 粒度的请求取消能力。每个正在执行的请求都会在
//! [`register`] 时分配一个 [`CancellationToken`]，调用 [`cancel`] 或 [`cancel_all`]
//! 后，持有该 token 的请求会立刻收到取消信号并退出循环。
//!
//! ## 线程安全
//! 使用 `tokio::sync::Mutex` 保护内部 `HashMap`，允许跨 Tauri 命令与后台任务共享。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// 注册序号生成器：全局单调递增，保证序号永不复用
///
/// 用于区分同一 `window_id` 上先后注册的多次请求，避免旧请求结束时
/// 把新请求的 token 一起清理掉（见 [`CancelRegistry::unregister`]）。
static NEXT_SEQ: AtomicU64 = AtomicU64::new(0);

/// 注册表内部条目：注册序号 + 取消令牌
struct Entry {
    seq: u64,
    token: CancellationToken,
}

/// 全局取消注册表：维护 `window_id -> Entry` 的映射
#[derive(Default, Clone)]
pub struct CancelRegistry {
    inner: Arc<Mutex<HashMap<usize, Entry>>>,
}

impl CancelRegistry {
    /// 创建一个空的取消注册表
    pub fn new() -> Self {
        Self::default()
    }

    /// 为指定的 `window_id` 注册一个全新的 [`CancellationToken`]
    ///
    /// 返回 `(序号, token)`，序号需要在请求结束时原样交回 [`unregister`]。
    /// 如果该 `window_id` 已经存在旧的 token（例如上一轮未正常清理），会被覆盖。
    pub async fn register(&self, window_id: usize) -> (u64, CancellationToken) {
        let token = CancellationToken::new();
        let seq = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.inner.lock().await;
        // 若已存在旧的 token，先取消它并替换（防止旧请求遗漏）
        let old = guard.insert(window_id, Entry { seq, token: token.clone() });
        if let Some(old) = old {
            old.token.cancel();
        }
        (seq, token)
    }

    /// 移除指定 `window_id` 的 token
    ///
    /// 只有注册序号仍然匹配时才真正移除：若期间已有新请求覆盖注册，
    /// 旧请求的清理动作不得影响新请求，否则新请求会变得无法取消。
    /// 通常在请求自然结束时调用，以便释放内存。
    pub async fn unregister(&self, window_id: usize, seq: u64) {
        let mut guard = self.inner.lock().await;
        if guard.get(&window_id).is_some_and(|entry| entry.seq == seq) {
            guard.remove(&window_id);
        }
    }

    /// 取消指定的 `window_id` 请求；返回 `true` 表示存在并触发了取消。
    ///
    /// 即使 token 已被 [`unregister`] 取走，`cancel` 本身是无害的：
    /// 已经结束的请求不会再收到这个信号。
    pub async fn cancel(&self, window_id: usize) -> bool {
        let guard = self.inner.lock().await;
        if let Some(entry) = guard.get(&window_id) {
            entry.token.cancel();
            true
        } else {
            false
        }
    }

    /// 取消所有正在执行的请求，返回被取消的 `window_id` 列表
    pub async fn cancel_all(&self) -> Vec<usize> {
        let guard = self.inner.lock().await;
        let ids: Vec<usize> = guard.keys().copied().collect();
        for entry in guard.values() {
            entry.token.cancel();
        }
        ids
    }

    /// 当前正在执行的请求数量
    #[allow(dead_code)]
    pub async fn active_count(&self) -> usize {
        self.inner.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 覆盖注册时必须取消旧 token，避免同一窗口存在两个活跃请求
    #[tokio::test]
    async fn test_register_cancels_previous_token() {
        let registry = CancelRegistry::new();
        let (_, old) = registry.register(0).await;
        let (_, new) = registry.register(0).await;

        assert!(old.is_cancelled(), "旧 token 应被取消");
        assert!(!new.is_cancelled(), "新 token 不应被取消");
    }

    /// 回归：旧请求结束时不得清除新请求的 token，否则新请求将无法取消
    #[tokio::test]
    async fn test_unregister_does_not_remove_newer_token() {
        let registry = CancelRegistry::new();
        let (old_seq, _old) = registry.register(0).await;
        let (_new_seq, _new) = registry.register(0).await;

        // 旧请求带着过期序号清理，应当被忽略
        registry.unregister(0, old_seq).await;

        assert!(
            registry.cancel(0).await,
            "新请求仍应可取消（旧请求的清理不得误删）"
        );
    }

    /// 序号匹配时正常移除，且不影响其它窗口
    #[tokio::test]
    async fn test_unregister_removes_matching_token() {
        let registry = CancelRegistry::new();
        let (seq, _token) = registry.register(1).await;
        let (other_seq, _other) = registry.register(2).await;

        registry.unregister(1, seq).await;

        assert!(!registry.cancel(1).await, "已注销的窗口不应再响应取消");
        assert!(registry.cancel(2).await, "其它窗口不受影响");
        assert_eq!(registry.active_count().await, 1);
        // 其它窗口的序号仍然匹配自己的注册
        registry.unregister(2, other_seq).await;
        assert_eq!(registry.active_count().await, 0);
    }
}