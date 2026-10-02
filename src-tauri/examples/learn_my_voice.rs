//! "This is my voice" for meetings while Felix is quit.
//!
//! cargo run --example learn_my_voice -- <meeting dir>...

fn main() {
    for dir in std::env::args().skip(1) {
        match handy_app_lib::meetings::remembered::learn_my_voice(std::path::Path::new(&dir)) {
            Ok(mic) => println!("{dir}: learned on {mic}"),
            Err(e) => println!("{dir}: {e}"),
        }
    }
}
