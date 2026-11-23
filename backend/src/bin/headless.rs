#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    meshshare_lib::run_headless().await
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("meshshare-headless requires Linux");
    std::process::exit(1);
}
