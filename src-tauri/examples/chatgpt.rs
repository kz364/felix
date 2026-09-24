//! Try Sign in with ChatGPT without building the app. Uses the same sign-in
//! file as the app, so signing in here signs the app in too.
//!
//!     cargo run --example chatgpt -- login
//!     cargo run --example chatgpt -- status
//!     cargo run --example chatgpt -- ask "text" [model] [effort]
//!     cargo run --example chatgpt -- logout

#[cfg(target_os = "macos")]
fn main() {
    use handy_app_lib::chatgpt;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = tauri::async_runtime::block_on(async {
        match args.first().map(String::as_str) {
            Some("login") => chatgpt::sign_in(|url| {
                std::process::Command::new("open")
                    .arg(url)
                    .status()
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .await
            .map(|email| format!("Signed in as {email}")),
            Some("status") => Ok(match chatgpt::signed_in_as() {
                Some(email) => format!("Signed in as {email}"),
                None => "Not signed in".into(),
            }),
            Some("logout") => {
                chatgpt::sign_out();
                Ok("Signed out".into())
            }
            Some("ask") if args.len() >= 2 => {
                let started = std::time::Instant::now();
                let reply = chatgpt::complete(chatgpt::Request {
                    model: args.get(2).map_or("gpt-6-luna", String::as_str),
                    effort: args.get(3).map_or("low", String::as_str),
                    instructions: "You are a helpful assistant. Answer briefly.",
                    input: &args[1],
                    schema: None,
                })
                .await;
                reply.map(|r| format!("{r}\n({:.1} s)", started.elapsed().as_secs_f32()))
            }
            _ => {
                Err("usage: chatgpt login | status | ask \"text\" [model] [effort] | logout".into())
            }
        }
    });
    match result {
        Ok(out) => println!("{out}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
