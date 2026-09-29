use chrono::{Datelike, Local, Timelike};
use ron::ser::PrettyConfig;
use std::fs::OpenOptions;
use std::fs::create_dir_all;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::LazyLock;

fn log_save(msg: &str) {
    // Best-effort debug log. Failures stay silent on purpose: logging
    // must never break the language server.
    if let Ok(mut file) = OpenOptions::new()
        .append(true)
        .create(true)
        .open("/tmp/brenz_lsp.log")
    {
        let _ = writeln!(file, "{}", msg);
    }
}

pub(crate) enum LogType {
    Info,
    Success,
    //Warning,
    Error,
}

macro_rules! logprint {
    () => {
        println!()
    };
    ($logtype:expr, $($arg:tt)*) => {{
        use crate::util::do_logprint;
        do_logprint(&format!($($arg)*), $logtype);
    }};
}

pub(crate) use logprint;

pub(crate) fn do_logprint(message: &str, logtype: LogType) {
    // Timestamps, saves to the debug log, and prints. Stdout doubles
    // as LSP transport, so clients must tolerate the chatter.
    let prefix = match logtype {
        LogType::Info => "[  INFO ]",
        LogType::Success => "[SUCCESS]",
        LogType::Error => "[ ERROR ]",
    };

    let now = now_fmt();

    let msg = format!("{now} {prefix} {message}");
    log_save(&msg);
    println!("{msg}");
}

pub(crate) fn now_fmt() -> String {
    // Day-first stamp for the log lines.
    let _now = Local::now();
    format!(
        "{:02}/{:02}/{} {:02}:{:02}:{:02}",
        _now.day(),
        _now.month(),
        _now.year(),
        _now.hour(),
        _now.minute(),
        _now.second()
    )
}

pub(crate) fn resolove_scr_path(
    root: &Option<PathBuf>,
    scr_path: Option<String>,
    script: String,
) -> io::Result<PathBuf> {
    // Yes, the name is misspelled, and half the codebase calls it by
    // now. Renaming means touching every call site for zero behavior
    // change, so the typo stays.
    let wd = &std::env::current_dir()?;
    logprint!(LogType::Info, "WD: {}", &wd.to_str().unwrap());
    let base = root.as_ref().unwrap_or(wd);
    match scr_path {
        Some(p) => Ok(base
            .join(p.replace("\\", "/"))
            .join(format!("{}.gsc", script))),
        None => Ok(base.join(format!("{}.gsc", script))),
    }
}

pub(crate) fn init_cfg() -> io::Result<PathBuf> {
    // `brenz --init` lands here: one `.brenz` in the current folder.
    let base = std::env::current_dir()?;
    crate::config::BrenzConfig::init_in(&base)
}

pub(crate) fn get_data_dir() -> io::Result<PathBuf> {
    // Home for unpacked docs, outside any single project.
    let base = std::env::home_dir().unwrap().join(".local/share/brenz");
    if !base.exists() {
        create_dir_all(&base)?;
    }

    Ok(base)
}

pub fn ron_pcfg() -> &'static PrettyConfig {
    static PC: LazyLock<PrettyConfig> = LazyLock::new(|| {
        PrettyConfig::new().indentor("  ".to_owned())
    });

    &PC
}
