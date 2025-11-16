use chrono::{Local, Datelike, Timelike};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;

fn log_save(msg: &str) {
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

pub(crate) fn do_logprint(message: &str, logtype: LogType)
{
    let prefix = match logtype {
        LogType::Info => format!("[  INFO ]"),
        LogType::Success => format!("[SUCCESS]"),
        LogType::Error => format!("[ ERROR ]"),
    };

    let now = now_fmt();

    let msg = format!("{now} {prefix} {message}");
    log_save(&msg);
    println!("{msg}");
}

pub(crate) fn now_fmt() -> String
{
    let _now = Local::now();
    format!("{:02}/{:02}/{} {:02}:{:02}:{:02}",
        _now.day(), _now.month(), _now.year(),
            _now.hour(), _now.minute(), _now.second()
    )
}

pub(crate) fn resolove_scr_path(
    root: &Option<PathBuf>, scr_path: Option<String>, script: String
) -> io::Result<PathBuf>
{
    let wd = &std::env::current_dir()?;
    logprint!(LogType::Info, "WD: {}", &wd.to_str().unwrap());
    let base = root.as_ref().unwrap_or(wd);
    match scr_path {
        Some(p) => {
            Ok(base.join(p.replace("\\", "/")).join(format!("{}.gsc", script)))
        }
        None => {
            Ok(base.join(format!("{}.gsc", script)))
        }
    }
}

pub(crate) fn init_cfg() -> io::Result<PathBuf>
{
    let base = std::env::current_dir()?;
    let f = base.join(".brenz");
    if f.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, ".brenz file already exists"));
    }

    let content = r#"# Brenz project file
# Currently this file is only being used to tell the root of your project.
# In future, it will be used for configuration like `.clangd` for clangd server.
"#;
    std::fs::write(f, content)?;

    Ok(base)
}
