//! # 请求取消管理
//!
//! 提供按 `window_id` 粒度的请求取消能力。每个正在执行的请求都会在
//! [`register`] 时分配一个 [`CancellationToken`]，调用 [`cancel`] 或 [`cancel_all`]
//! 后，持有该 token 的请求会立刻收到取消信号并退出循环。
//!
//! ## 线程安全
//! 使用 `tokio::sync::Mutex` 保护内部 `HashMap`，允许跨 Tauri 命令与后台任务共享。

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// 全局取消注册表：维护 `window_id -> CancellationToken` 的映射
#[derive(Default, Clone)]
pub struct CancelRegistry {
    inner: Arc<Mutex<HashMap<usize, CancellationToken>>>,
}

impl CancelRegistry {
    /// 创建一个空的取消注册表
    pub fn new() -> Self {
        Self::default()
    }

    /// 为指定的 `window_id` 注册一个全新的 [`CancellationToken`]
    ///
    /// 如果该 `window_id` 已经存在旧的 token（例如上一轮未正常清理），会被覆盖。
    /// 调用方负责在请求结束后调用 [`unregister`]，避免内存泄漏。
    pub async fn register(&self, window_id: usize) -> CancellationToken {
        let token = CancellationToken::new();
        let mut guard = self.inner.lock().await;
        // 若已存在旧的 token，先取消它并替换（防止旧请求遗漏）
        if let Some(old) = guard.insert(window_id, token.clone()) {
            old.cancel();
        }
        token
    }

    /// 取出（并移除）指定 `window_id` 的 token，返回 `Some(token)` 表示仍在运行，
    /// `None` 表示该 `window_id` 没有活跃请求。
    ///
    /// 通常在请求自然结束时调用，以便释放内存。
    pub async fn unregister(&self, window_id: usize) -> Option<CancellationToken> {
        self.inner.lock().await.remove(&window_id)
    }

    /// 取消指定的 `window_id` 请求；返回 `true` 表示存在并触发了取消。
    ///
    /// 即使 token 已被 [`unregister`] 取走，`cancel` 本身是无害的：
    /// 已经结束的请求不会再收到这个信号。
    pub async fn cancel(&self, window_id: usize) -> bool {
        let guard = self.inner.lock().await;
        if let Some(token) = guard.get(&window_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// 取消所有正在执行的请求，返回被取消的 `window_id` 列表
    pub async fn cancel_all(&self) -> Vec<usize> {
        let guard = self.inner.lock().await;
        let ids: Vec<usize> = guard.keys().copied().collect();
        for token in guard.values() {
            token.cancel();
        }
        ids
    }

    /// 当前正在执行的请求数量
    #[allow(dead_code)]
    pub async fn active_count(&self) -> usize {
        self.inner.lock().await.len()
    }
}