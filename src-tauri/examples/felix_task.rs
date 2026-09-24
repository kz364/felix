//! Run one of Felix's computer tasks (Codex + Cua Driver) from the shell.
//!
//!     cargo run --example felix_task -- "Which apps are running?"

fn main() {
    let task = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if task.is_empty() {
        eprintln!("usage: felix_task <task>");
        std::process::exit(2);
    }
    let result = handy_app_lib::agent::run_task(&task);
    handy_app_lib::agent::stop();
    match result {
        Ok(reply) => println!("{reply}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
