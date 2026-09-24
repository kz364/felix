//! Print an app's accessibility tree, to find the controls a skill presses.
//!
//!     cargo run --example ax_dump -- com.anthropic.claudefordesktop [depth]

fn main() {
    use handy_app_lib::ax_tree;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(bundle) = args.first() else {
        eprintln!("usage: ax_dump <bundle id> [depth]");
        std::process::exit(2);
    };
    let depth = args
        .get(1)
        .and_then(|d| d.parse().ok())
        .unwrap_or(ax_tree::DEFAULT_DEPTH);
    if !ax_tree::is_trusted() {
        eprintln!("Not trusted for accessibility");
    }
    let Some(pid) = ax_tree::pid_of(bundle) else {
        eprintln!("{bundle} isn't running");
        std::process::exit(1);
    };
    for node in ax_tree::dump(pid, depth) {
        if node.label.is_empty() && node.role == "AXGroup" {
            continue;
        }
        let label: String = node.label.chars().take(80).collect();
        println!(
            "{}{} {:?}",
            "  ".repeat(node.depth.min(30)),
            node.role,
            label
        );
    }
}
