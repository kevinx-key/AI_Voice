//! llama-server 生命周期管理：懒加载 + 复用 + 可选回收
//!
//! 红线：`stop()` 只杀**本进程自己拉起**的实例（`child.is_some()`）；
//! 用户手动启动或别的程序（如 Python 版）拉起的实例一律不动。

use crate::config::{Paths, PolishLocal};
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 本地大模型状态（回传 GUI）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmState {
    Unknown,
    Starting,
    Ready,
    Reused,
    Failed(String),
}

impl LlmState {
    pub fn label(&self) -> String {
        match self {
            LlmState::Unknown => "未启动".into(),
            LlmState::Starting => "启动中…".into(),
            LlmState::Ready => "就绪（本程序拉起）".into(),
            LlmState::Reused => "就绪（复用已有实例）".into(),
            LlmState::Failed(e) => format!("失败：{e}"),
        }
    }
}

type StateSink = Arc<dyn Fn(LlmState) + Send + Sync>;

pub struct LlamaServer {
    exe: PathBuf,
    model: PathBuf,
    host: String,
    port: u16,
    ngl: i32,
    ctx: u32,
    start_timeout: Duration,
    log_path: PathBuf,
    stop_on_exit: bool,
    wait_on_first_use: bool,
    client: reqwest::blocking::Client,
    /// 「它在不在跑」专用探针：**极短超时**（见 `alive_fast` 的注释）
    probe: reqwest::blocking::Client,
    child: Mutex<Option<Child>>,
    starting: AtomicBool,
    state: Mutex<LlmState>,
    sink: Option<StateSink>,
}

impl LlamaServer {
    pub fn new(cfg: &PolishLocal, paths: &Paths) -> Self {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap_or_default();
        // ★ 探针客户端必须**卡死上限**（2026-10-20）：
        // 本机实测「连一个没人监听的本地端口」要 ~2s 才失败（SYN 被丢弃后重试/RST 迟到，
        // 不是我们代码的问题）。而链路状态卡片每秒都要问一次“整理后端在不在跑”，
        // 一旦整理后端没起（很常见：懒加载、刚被空闲释放、wait_on_first_use=false），
        // worker 就几乎一直在等这个 HTTP，Release 要 1.9s 才被处理、短按的 1.5s 窗口直接错过。
        // 探针只需要“活着吗”，不值得为它阻塞录音线程 → 给它 200ms 硬上限。
        let probe = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_millis(120))
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap_or_default();
        Self {
            exe: paths.resolve(&cfg.exe),
            model: paths.resolve(&cfg.model),
            host: cfg.host.clone(),
            port: cfg.port,
            ngl: cfg.ngl,
            ctx: cfg.ctx,
            start_timeout: Duration::from_secs(cfg.start_timeout_s.max(5)),
            log_path: paths.logs_dir().join("llama_server.log"),
            stop_on_exit: cfg.stop_on_exit,
            wait_on_first_use: cfg.wait_on_first_use,
            client,
            probe,
            child: Mutex::new(None),
            starting: AtomicBool::new(false),
            state: Mutex::new(LlmState::Unknown),
            sink: None,
        }
    }

    pub fn with_sink(mut self, sink: StateSink) -> Self {
        self.sink = Some(sink);
        self
    }

    pub fn wait_on_first_use(&self) -> bool {
        self.wait_on_first_use
    }

    pub fn state(&self) -> LlmState {
        self.state.lock().map(|g| g.clone()).unwrap_or(LlmState::Unknown)
    }

    fn set_state(&self, s: LlmState) {
        {
            let mut g = match self.state.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            if *g == s {
                return;
            }
            *g = s.clone();
        }
        tracing::info!("本地大模型状态: {}", s.label());
        if let Some(f) = &self.sink {
            f(s);
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}:{}{}", self.host, self.port, path)
    }

    /// OpenAI 兼容根地址（无 /v1 前缀，llama-server 两者都认）
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// GET /health，200 = 就绪（含别人拉起的实例）
    ///
    /// 注意：这是**正式请求**用的客户端（1s 连接 / 2s 总超时）。录音线程上的状态探测
    /// 一律用 [`LlamaServer::alive_fast`]——本机连一个没人监听的本地端口实测要 ~2s，
    /// 2s 的超时丢在录音循环里就是“按下没反应/松手半天不收尾”。
    pub fn alive(&self) -> bool {
        self.client
            .get(self.url("/health"))
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// 「在不在跑」专用探针：**硬上限 200ms**（120ms 连接 + 200ms 总）
    ///
    /// 只认“连不上/超时 = 没在跑”，不会把慢当成活。链路状态卡片、托盘 tooltip、
    /// `--status` 都走这条，绝不允许它把录音线程卡住。
    pub fn alive_fast(&self) -> bool {
        self.probe
            .get(self.url("/health"))
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// 端口有东西在监听但不是 llama-server
    fn port_busy(&self) -> bool {
        let addr = format!("{}:{}", self.host, self.port);
        let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&addr) else {
            return false;
        };
        for a in addrs {
            if std::net::TcpStream::connect_timeout(&a, Duration::from_millis(500)).is_ok() {
                return true;
            }
        }
        false
    }

    fn spawn_process(&self) -> Result<Child> {
        if !self.exe.is_file() {
            bail!("llama-server 不存在：{}", self.exe.display());
        }
        if !self.model.is_file() {
            bail!("GGUF 模型不存在：{}", self.model.display());
        }
        if let Some(dir) = self.log_path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .with_context(|| format!("打开日志失败: {}", self.log_path.display()))?;
        let err_log = log.try_clone().context("复制日志句柄失败")?;

        let mut cmd = Command::new(&self.exe);
        cmd.arg("-m")
            .arg(&self.model)
            .arg("--host")
            .arg(&self.host)
            .arg("--port")
            .arg(self.port.to_string())
            .arg("-ngl")
            .arg(self.ngl.to_string())
            .arg("-c")
            .arg(self.ctx.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err_log));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        tracing::info!(
            "拉起 llama-server: {} -m {} --host {} --port {} -ngl {} -c {}",
            self.exe.display(),
            self.model.display(),
            self.host,
            self.port,
            self.ngl,
            self.ctx
        );
        cmd.spawn().context("启动 llama-server 失败")
    }

    /// 已在跑 → 复用；否则 spawn 并轮询就绪（最多 start_timeout）
    pub fn ensure_started(&self) -> Result<bool> {
        if self.alive() {
            self.set_state(LlmState::Reused);
            return Ok(true);
        }
        if self.port_busy() {
            let msg = format!(
                "端口 {} 已被占用，且不是 llama-server（改 polish.local.port）",
                self.port
            );
            self.set_state(LlmState::Failed(msg.clone()));
            bail!("{msg}");
        }

        // 只有一个线程负责 spawn
        if !self.starting.swap(true, Ordering::SeqCst) {
            match self.spawn_process() {
                Ok(c) => {
                    if let Ok(mut g) = self.child.lock() {
                        *g = Some(c);
                    }
                }
                Err(e) => {
                    self.starting.store(false, Ordering::SeqCst);
                    self.set_state(LlmState::Failed(format!("{e:#}")));
                    return Err(e);
                }
            }
        }
        self.set_state(LlmState::Starting);

        let deadline = Instant::now() + self.start_timeout;
        while Instant::now() < deadline {
            if self.alive() {
                self.starting.store(false, Ordering::SeqCst);
                self.set_state(LlmState::Ready);
                return Ok(true);
            }
            let mut wait_err: Option<String> = None;
            let exited = {
                let mut g = match self.child.lock() {
                    Ok(g) => g,
                    Err(_) => break,
                };
                match g.as_mut() {
                    Some(c) => match c.try_wait() {
                        Ok(status) => status,
                        Err(e) => {
                            wait_err = Some(e.to_string());
                            None
                        }
                    },
                    None => None,
                }
            };
            if let Some(e) = wait_err {
                // 句柄坏了就没法再监控：先回收，避免残留进程占住端口
                self.kill_child();
                let msg = format!("监控 llama-server 进程失败：{e}");
                self.set_state(LlmState::Failed(msg.clone()));
                bail!("{msg}");
            }
            if let Some(status) = exited {
                if let Ok(mut g) = self.child.lock() {
                    *g = None;
                }
                self.starting.store(false, Ordering::SeqCst);
                let msg = format!(
                    "llama-server 提前退出（{status}），详见 {}",
                    self.log_path.display()
                );
                self.set_state(LlmState::Failed(msg.clone()));
                bail!("{msg}");
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        // 超时必须回收子进程：否则 child 残留、starting 已复位，
        // 下次 ensure_started 会走「端口被占用」分支，本地整理永久不可用
        self.kill_child();
        let msg = format!(
            "llama-server 启动超时（{}s），详见 {}",
            self.start_timeout.as_secs(),
            self.log_path.display()
        );
        self.set_state(LlmState::Failed(msg.clone()));
        bail!("{msg}");
    }

    /// 后台拉起（懒加载首次用，不等待）
    pub fn spawn_background(self: &Arc<Self>) {
        if self.starting.load(Ordering::SeqCst) || self.alive() {
            return;
        }
        let me = self.clone();
        let r = std::thread::Builder::new()
            .name("llama-start".into())
            .spawn(move || {
                if let Err(e) = me.ensure_started() {
                    tracing::warn!("本地大模型启动失败: {e:#}");
                }
            });
        if let Err(e) = r {
            tracing::error!("创建启动线程失败: {e}");
        }
    }

    /// 只回收自己拉起的进程；别人启动的一律不动
    pub fn stop(&self) {
        if self.kill_child() {
            self.set_state(LlmState::Unknown);
        }
    }

    /// 杀自己拉起的子进程；返回是否真的杀了
    fn kill_child(&self) -> bool {
        let child = match self.child.lock() {
            Ok(mut g) => g.take(),
            Err(_) => None,
        };
        self.starting.store(false, Ordering::SeqCst);
        let Some(mut c) = child else {
            // 没自己拉起过：可能是复用别人的实例，也可能压根没跑——都不动
            return false;
        };
        tracing::info!("回收 llama-server（pid={}）", c.id());
        let _ = c.kill();
        let _ = c.wait();
        true
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        // 退出路径：不发状态事件（对象正在销毁，避免覆盖新实例的状态）
        if self.stop_on_exit {
            self.kill_child();
        }
    }
}
