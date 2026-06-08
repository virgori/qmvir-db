//! Multi-language support for QMvir CLI.
//!
//! Supported: vi (Vietnamese, no diacritics), en, zht (Traditional Chinese), zh (Simplified Chinese).

pub enum Lang {
    Vi,
    En,
    Zht,
    Zh,
}

impl Lang {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "vi" | "vn" => Lang::Vi,
            "en" => Lang::En,
            "zht" | "zh-tw" | "zh_tw" => Lang::Zht,
            "zh" | "zh-cn" | "zh_cn" => Lang::Zh,
            _ => Lang::En, // default
        }
    }

    /// Get a localized message by key.
    /// `arg` is an optional argument to embed in the message.
    pub fn msg(&self, key: &str, arg: &str) -> String {
        match self {
            Lang::Vi => Self::vi(key, arg),
            Lang::En => Self::en(key, arg),
            Lang::Zht => Self::zht(key, arg),
            Lang::Zh => Self::zh(key, arg),
        }
    }

    fn vi(key: &str, arg: &str) -> String {
        match key {
            "error" => "Loi".to_string(),
            "err_no_data_dir" => format!("Thu muc du lieu khong ton tai: {}", arg),
            "hint_try" => "Thu:".to_string(),
            "hint_list" => "Xem danh sach bang:".to_string(),
            "hint_syntax" => "Xem cu phap:".to_string(),
            "hint_auth" => "Can xac thuc:".to_string(),
            "hint_start" => "Khoi dong server:".to_string(),
            "hint_status" => "Kiem tra trang thai:".to_string(),
            "checkpoint_done" => "Checkpoint hoan tat".to_string(),
            "server_started" => format!("QMvir da khoi dong (PID {})", arg),
            "server_stopped" => "Da gui tin hieu dung".to_string(),
            "server_not_running" => "QMvir khong chay".to_string(),
            "server_running" => format!("QMvir dang chay (PID {})", arg),
            "server_stale_pid" => format!("PID {} khong con chay. Da xoa PID file.", arg),
            "shutdown_start" => "Dang tat...".to_string(),
            "shutdown_checkpoint" => "Dang checkpoint...".to_string(),
            "shutdown_done" => "Checkpoint xong.".to_string(),
            _ => format!("[{}] {}", key, arg),
        }
    }

    fn en(key: &str, arg: &str) -> String {
        match key {
            "error" => "Error".to_string(),
            "err_no_data_dir" => format!("Data directory does not exist: {}", arg),
            "hint_try" => "Try:".to_string(),
            "hint_list" => "List tables:".to_string(),
            "hint_syntax" => "See syntax:".to_string(),
            "hint_auth" => "Authentication required:".to_string(),
            "hint_start" => "Start server:".to_string(),
            "hint_status" => "Check status:".to_string(),
            "checkpoint_done" => "Checkpoint completed".to_string(),
            "server_started" => format!("QMvir started (PID {})", arg),
            "server_stopped" => "Sent stop signal".to_string(),
            "server_not_running" => "QMvir is not running".to_string(),
            "server_running" => format!("QMvir is running (PID {})", arg),
            "server_stale_pid" => format!("PID {} is not running. Removed stale PID file.", arg),
            "shutdown_start" => "Shutting down...".to_string(),
            "shutdown_checkpoint" => "Checkpointing...".to_string(),
            "shutdown_done" => "Checkpoint done.".to_string(),
            _ => format!("[{}] {}", key, arg),
        }
    }

    fn zht(key: &str, arg: &str) -> String {
        match key {
            "error" => "\u{932f}\u{8aa4}".to_string(), // 錯誤
            "err_no_data_dir" => format!("\u{8cc7}\u{6599}\u{76ee}\u{9304}\u{4e0d}\u{5b58}\u{5728}: {}", arg), // 資料目錄不存在
            "hint_try" => "\u{8acb}\u{5617}\u{8a66}:".to_string(), // 請嘗試:
            "hint_list" => "\u{67e5}\u{770b}\u{8868}\u{5217}\u{8868}:".to_string(), // 查看表列表:
            "hint_syntax" => "\u{67e5}\u{770b}\u{8a9e}\u{6cd5}:".to_string(), // 查看語法:
            "hint_auth" => "\u{9700}\u{8981}\u{9a57}\u{8b49}:".to_string(), // 需要驗證:
            "hint_start" => "\u{555f}\u{52d5}\u{4f3a}\u{670d}\u{5668}:".to_string(), // 啟動伺服器:
            "hint_status" => "\u{6aa2}\u{67e5}\u{72c0}\u{614b}:".to_string(), // 檢查狀態:
            "checkpoint_done" => "\u{6aa2}\u{67e5}\u{9ede}\u{5b8c}\u{6210}".to_string(), // 檢查點完成
            "server_started" => format!("QMvir \u{5df2}\u{555f}\u{52d5} (PID {})", arg), // 已啟動
            "server_stopped" => "\u{5df2}\u{767c}\u{9001}\u{505c}\u{6b62}\u{4fe1}\u{865f}".to_string(), // 已發送停止信號
            "server_not_running" => "QMvir \u{672a}\u{904b}\u{884c}".to_string(), // 未運行
            "server_running" => format!("QMvir \u{904b}\u{884c}\u{4e2d} (PID {})", arg), // 運行中
            "server_stale_pid" => format!("PID {} \u{5df2}\u{505c}\u{6b62}\u{3002}\u{5df2}\u{522a}\u{9664}PID\u{6a94}\u{3002}", arg), // 已停止。已刪除PID檔。
            "shutdown_start" => "\u{6b63}\u{5728}\u{95dc}\u{9589}...".to_string(), // 正在關閉...
            "shutdown_checkpoint" => "\u{6b63}\u{5728}\u{6aa2}\u{67e5}\u{9ede}...".to_string(), // 正在檢查點...
            "shutdown_done" => "\u{6aa2}\u{67e5}\u{9ede}\u{5b8c}\u{6210}\u{3002}".to_string(), // 檢查點完成。
            _ => format!("[{}] {}", key, arg),
        }
    }

    fn zh(key: &str, arg: &str) -> String {
        match key {
            "error" => "\u{9519}\u{8bef}".to_string(), // 错误
            "err_no_data_dir" => format!("\u{6570}\u{636e}\u{76ee}\u{5f55}\u{4e0d}\u{5b58}\u{5728}: {}", arg), // 数据目录不存在
            "hint_try" => "\u{8bf7}\u{5c1d}\u{8bd5}:".to_string(), // 请尝试:
            "hint_list" => "\u{67e5}\u{770b}\u{8868}\u{5217}\u{8868}:".to_string(), // 查看表列表:
            "hint_syntax" => "\u{67e5}\u{770b}\u{8bed}\u{6cd5}:".to_string(), // 查看语法:
            "hint_auth" => "\u{9700}\u{8981}\u{9a8c}\u{8bc1}:".to_string(), // 需要验证:
            "hint_start" => "\u{542f}\u{52a8}\u{670d}\u{52a1}\u{5668}:".to_string(), // 启动服务器:
            "hint_status" => "\u{68c0}\u{67e5}\u{72b6}\u{6001}:".to_string(), // 检查状态:
            "checkpoint_done" => "\u{68c0}\u{67e5}\u{70b9}\u{5b8c}\u{6210}".to_string(), // 检查点完成
            "server_started" => format!("QMvir \u{5df2}\u{542f}\u{52a8} (PID {})", arg), // 已启动
            "server_stopped" => "\u{5df2}\u{53d1}\u{9001}\u{505c}\u{6b62}\u{4fe1}\u{53f7}".to_string(), // 已发送停止信号
            "server_not_running" => "QMvir \u{672a}\u{8fd0}\u{884c}".to_string(), // 未运行
            "server_running" => format!("QMvir \u{8fd0}\u{884c}\u{4e2d} (PID {})", arg), // 运行中
            "server_stale_pid" => format!("PID {} \u{5df2}\u{505c}\u{6b62}\u{3002}\u{5df2}\u{5220}\u{9664}PID\u{6587}\u{4ef6}\u{3002}", arg), // 已停止。已删除PID文件。
            "shutdown_start" => "\u{6b63}\u{5728}\u{5173}\u{95ed}...".to_string(), // 正在关闭...
            "shutdown_checkpoint" => "\u{6b63}\u{5728}\u{68c0}\u{67e5}\u{70b9}...".to_string(), // 正在检查点...
            "shutdown_done" => "\u{68c0}\u{67e5}\u{70b9}\u{5b8c}\u{6210}\u{3002}".to_string(), // 检查点完成。
            _ => format!("[{}] {}", key, arg),
        }
    }
}
