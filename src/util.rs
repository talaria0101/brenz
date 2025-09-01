use std::fs::OpenOptions;
use std::io::Write;

pub(crate) fn log_print(msg: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .append(true)
        .create(true)
        .open("/tmp/brenz_lsp.log")
    {
        let _ = writeln!(file, "{}", msg);
    }
}
