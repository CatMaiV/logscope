//! 应用状态：多文件会话与导入/检索任务管理。

use logscope_core::detail::DetailReader;
use logscope_core::index::WorkspaceIndex;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// 后台任务类型
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskKind {
    Import,
    /// M2 可取消长查询接入时启用
    #[allow(dead_code)]
    Search,
}

/// 任务状态
#[derive(Clone, Debug)]
pub enum TaskStatus {
    Pending,
    Running,
    Done,
    Cancelled,
    Failed(String),
}

/// 后台任务句柄：取消标志 + 状态。
pub struct TaskHandle {
    pub id: String,
    pub kind: TaskKind,
    pub cancel: Arc<AtomicBool>,
    pub status: Mutex<TaskStatus>,
}

/// 检索任务句柄：取消标志 + 状态 + 物化结果集（M2 游标分页的数据源）。
pub struct SearchHandle {
    pub id: String,
    pub cancel: Arc<AtomicBool>,
    pub status: Mutex<TaskStatus>,
    /// 执行完成后物化；读取时上锁
    pub result: Mutex<Option<logscope_core::search::SearchResultSet>>,
}

impl SearchHandle {
    pub fn new(id: String) -> Self {
        SearchHandle {
            id,
            cancel: Arc::new(AtomicBool::new(false)),
            status: Mutex::new(TaskStatus::Pending),
            result: Mutex::new(None),
        }
    }

    pub fn status(&self) -> TaskStatus {
        self.status.lock().clone()
    }
}

impl TaskHandle {
    pub fn new(id: String, kind: TaskKind) -> Self {
        TaskHandle {
            id,
            kind,
            cancel: Arc::new(AtomicBool::new(false)),
            status: Mutex::new(TaskStatus::Pending),
        }
    }

    pub fn set_status(&self, s: TaskStatus) {
        *self.status.lock() = s;
    }

    pub fn status(&self) -> TaskStatus {
        self.status.lock().clone()
    }

    pub fn cancel(&self) -> bool {
        self.cancel.swap(true, std::sync::atomic::Ordering::Release)
    }
}

/// 当前工作区：全局索引 + 各文件原文读取器。
pub struct Workspace {
    pub index: WorkspaceIndex,
    pub readers: HashMap<u32, Arc<Mutex<DetailReader>>>,
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace {
            index: WorkspaceIndex {
                files: HashMap::new(),
                time_order: Vec::new(),
                unknown_time: Vec::new(),
            },
            readers: HashMap::new(),
        }
    }
}

/// 应用全局状态（`tauri::manage` 托管）。
pub struct AppState {
    pub workspace: RwLock<Workspace>,
    pub tasks: RwLock<HashMap<String, Arc<TaskHandle>>>,
    /// 会话内检索任务（含物化结果集）
    pub searches: RwLock<HashMap<String, Arc<SearchHandle>>>,
    next_file_id: std::sync::atomic::AtomicU32,
    /// 并发扫描限制（开发计划 §4.1：防止磁盘竞争和内存峰值）
    pub scan_permits: Arc<tokio::sync::Semaphore>,
    /// 工作区默认时区（分钟偏移），默认 +480
    pub default_tz_offset_min: i32,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            workspace: RwLock::new(Workspace::default()),
            tasks: RwLock::new(HashMap::new()),
            searches: RwLock::new(HashMap::new()),
            next_file_id: std::sync::atomic::AtomicU32::new(0),
            scan_permits: Arc::new(tokio::sync::Semaphore::new(3)),
            default_tz_offset_min: 480,
        }
    }
}

impl AppState {
    pub fn alloc_file_id(&self) -> u32 {
        self.next_file_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn register_task(&self, handle: Arc<TaskHandle>) {
        self.tasks.write().insert(handle.id.clone(), handle);
    }

    pub fn get_task(&self, id: &str) -> Option<Arc<TaskHandle>> {
        self.tasks.read().get(id).cloned()
    }

    pub fn register_search(&self, handle: Arc<SearchHandle>) {
        self.searches.write().insert(handle.id.clone(), handle);
    }

    pub fn get_search(&self, id: &str) -> Option<Arc<SearchHandle>> {
        self.searches.read().get(id).cloned()
    }

    /// 前端显式释放检索结果集（Done 的结果需保留供分页，不能自动清理）
    pub fn remove_search(&self, id: &str) -> Option<Arc<SearchHandle>> {
        self.searches.write().remove(id)
    }

    /// 移除已完成任务（保留终态供短时查询，由调用方决定清理时机）
    pub fn prune_tasks(&self) {
        self.tasks.write().retain(|_, h| {
            !matches!(
                h.status(),
                TaskStatus::Done | TaskStatus::Cancelled | TaskStatus::Failed(_)
            )
        });
    }
}
