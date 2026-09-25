//! Run one of Felix's computer tasks from the shell. Set FELIX_APP (and
//! FELIX_CONTENT, the text to type) to try the Simple Jev fast path first.
//!
//!     cargo run --example felix_task -- "Which apps are running?"
//!     FELIX_APP=Notes FELIX_CONTENT="Socks" cargo run --example felix_task -- "Start a packing list in Notes"

fn main() {
    let task = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if task.is_empty() {
        eprintln!("usage: felix_task <task>");
        std::process::exit(2);
    }
    let result = handy_app_lib::agent::run_task(
        &task,
        &std::env::var("FELIX_APP").unwrap_or_default(),
        &std::env::var("FELIX_CONTENT").unwrap_or_default(),
        true,
    );
    handy_app_lib::agent::stop();
    match result {
        Ok(reply) => println!("{reply}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
